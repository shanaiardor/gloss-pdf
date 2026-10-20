//! 通过 GitHub Releases 检查与安装更新。
//!
//! 查 release、比较版本、下载资产都交给 `self_update`（底层是 ureq + rustls），
//! 这里只负责线程模型、状态事件和安装器启动。更新包就是 CI 产出的 Inno Setup
//! 安装器，下载完以静默参数运行，然后退出应用让安装器覆盖文件。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::{Context as _, Result};
use futures::channel::mpsc::UnboundedSender;
use self_update::backends::github::ReleaseList;

/// 当前版本号，与 `Cargo.toml` 保持一致；`Cargo.toml` 是唯一事实来源。
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 安装包文件名后缀
#[cfg(target_os = "windows")]
const INSTALLER_SUFFIX: &str = "-setup.exe";
#[cfg(target_os = "macos")]
const INSTALLER_SUFFIX: &str = ".dmg";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const INSTALLER_SUFFIX: &str = ".tar.gz";

/// release notes 太长会把设置面板撑开，截断到一段话。
const MAX_NOTES_CHARS: usize = 800;

/// 仓库坐标：CI 构建时用 `GITHUB_REPOSITORY` 注入，本地构建回落到默认值。
pub fn repository() -> (&'static str, &'static str) {
    let full = option_env!("GITHUB_REPOSITORY").unwrap_or("shanaiardor/gpui-pdf");
    full.split_once('/').unwrap_or(("shanaiardor", "gpui-pdf"))
}

/// 一次可用的更新。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: String,
    pub url: String,
}

pub enum Command {
    Check,
    Download { info: UpdateInfo },
}

pub enum Event {
    Checking,
    UpToDate { version: String },
    Available(UpdateInfo),
    Downloading,
    Progress { downloaded: u64, total: Option<u64> },
    Downloaded(PathBuf),
    Failed(String),
}

pub struct UpdateEngine {
    pub commands: Sender<Command>,
}

impl UpdateEngine {
    pub fn spawn(events: UnboundedSender<Event>) -> Self {
        let (commands, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("update-worker".into())
            .spawn(move || worker_loop(receiver, events))
            .expect("failed to spawn update worker");
        Self { commands }
    }
}

fn worker_loop(commands: Receiver<Command>, events: UnboundedSender<Event>) {
    while let Ok(command) = commands.recv() {
        match command {
            Command::Check => {
                let _ = events.unbounded_send(Event::Checking);
                match check_latest() {
                    Ok(Some(info)) => {
                        let _ = events.unbounded_send(Event::Available(info));
                    }
                    Ok(None) => {
                        let _ = events.unbounded_send(Event::UpToDate {
                            version: CURRENT_VERSION.to_string(),
                        });
                    }
                    Err(error) => {
                        log::warn!("检查更新失败: {error:#}");
                        let _ = events.unbounded_send(Event::Failed(format!("{error:#}")));
                    }
                }
            }
            Command::Download { info } => {
                let _ = events.unbounded_send(Event::Downloading);
                match download(&info, &events) {
                    Ok(path) => {
                        let _ = events.unbounded_send(Event::Downloaded(path));
                    }
                    Err(error) => {
                        log::warn!("下载更新失败: {error:#}");
                        let _ = events.unbounded_send(Event::Failed(format!("{error:#}")));
                    }
                }
            }
        }
    }
}

/// 找比当前版本新的正式版 release（跳过 `-beta` 之类的预发布）。
fn check_latest() -> Result<Option<UpdateInfo>> {
    let (owner, repo) = repository();
    let releases = ReleaseList::configure()
        .repo_owner(owner)
        .repo_name(repo)
        .build()?
        .fetch()
        .context("获取 GitHub release 列表失败")?;

    // `all()` 是 GitHub 返回的顺序（新→旧）；只认第一个比当前版本新的正式版。
    let Some(release) = releases.all().iter().find(|release| {
        !release.version().contains('-')
            && self_update::version::bump_is_greater(CURRENT_VERSION, release.version())
                .unwrap_or(false)
            && installer_asset(release).is_some()
    }) else {
        return Ok(None);
    };

    let asset = installer_asset(release).expect("checked above");
    Ok(Some(UpdateInfo {
        version: release.version().to_string(),
        notes: truncate(release.body().unwrap_or_default(), MAX_NOTES_CHARS),
        url: asset.download_url().to_string(),
    }))
}

fn installer_asset(
    release: &self_update::update::Release,
) -> Option<&self_update::update::ReleaseAsset> {
    release.assets().iter().find(|asset| {
        let name = asset.name().to_ascii_lowercase();
        #[cfg(target_os = "windows")]
        {
            name.ends_with(INSTALLER_SUFFIX) || (name.contains("setup") && name.ends_with(".exe"))
        }
        #[cfg(target_os = "macos")]
        {
            name.ends_with(INSTALLER_SUFFIX) || (name.contains("macos") && (name.ends_with(".dmg") || name.ends_with(".zip")))
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            name.ends_with(INSTALLER_SUFFIX) || name.ends_with(".tar.gz")
        }
    })
}

fn download(info: &UpdateInfo, events: &UnboundedSender<Event>) -> Result<PathBuf> {
    let filename = if cfg!(target_os = "windows") {
        format!("gpui-pdf-{}-setup.exe", info.version)
    } else if cfg!(target_os = "macos") {
        format!("gpui-pdf-{}.dmg", info.version)
    } else {
        format!("gpui-pdf-{}.tar.gz", info.version)
    };
    let path = std::env::temp_dir().join(filename);
    let file = std::fs::File::create(&path)
        .with_context(|| format!("无法创建临时文件 {}", path.display()))?;

    let progress_events = events.clone();
    let mut download = self_update::Download::from_url(info.url.clone());
    download
        .progress_callback(move |downloaded, total| {
            let _ = progress_events.unbounded_send(Event::Progress { downloaded, total });
        })
        .download_to(std::io::BufWriter::new(file))
        .context("下载安装包失败")?;

    Ok(path)
}

/// 静默运行安装器或打开安装包；调用方应该在之后退出应用。
pub fn launch_installer(path: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new(path)
            // Inno Setup 的静默安装参数：显示进度条、关闭占用文件的程序、不自动重启。
            .args(["/SILENT", "/CLOSEAPPLICATIONS", "/NORESTART"])
            .spawn()
            .with_context(|| format!("无法启动安装程序 {}", path.display()))?;
    }
    #[cfg(target_os = "macos")]
    {
        // macOS 上使用系统 open 命令挂载/打开 dmg 安装包
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .with_context(|| format!("无法打开安装包 {}", path.display()))?;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .with_context(|| format!("无法打开更新包 {}", path.display()))?;
    }
    Ok(())
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut output: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use self_update::update::{Release, ReleaseAsset};

    fn release(version: &str, assets: &[&str]) -> Release {
        let mut builder = Release::builder();
        builder.version(version).name(version);
        for name in assets {
            builder.asset(ReleaseAsset::new(
                *name,
                format!("https://example.com/{name}"),
            ));
        }
        builder.build().unwrap()
    }

    #[test]
    fn installer_asset_prefers_platform_package() {
        #[cfg(target_os = "windows")]
        {
            let release = release(
                "0.2.0",
                &["gpui-pdf-0.2.0-sha256.txt", "gpui-pdf-0.2.0-setup.exe"],
            );
            let asset = installer_asset(&release).expect("installer");
            assert_eq!(asset.name(), "gpui-pdf-0.2.0-setup.exe");
        }
        #[cfg(target_os = "macos")]
        {
            let release = release(
                "0.2.0",
                &["gpui-pdf-0.2.0-sha256.txt", "gpui-pdf-0.2.0-macos-arm64.dmg"],
            );
            let asset = installer_asset(&release).expect("installer");
            assert_eq!(asset.name(), "gpui-pdf-0.2.0-macos-arm64.dmg");
        }
    }

    #[test]
    fn installer_asset_ignores_other_binaries() {
        let release = release("0.2.0", &["gpui-pdf.exe"]);
        assert!(installer_asset(&release).is_none());
    }

    #[test]
    fn repository_splits_owner_and_name() {
        let (owner, repo) = repository();
        assert!(!owner.is_empty());
        assert!(!repo.is_empty());
    }

    #[test]
    fn notes_are_truncated() {
        let text = "a".repeat(MAX_NOTES_CHARS + 10);
        let shortened = truncate(&text, MAX_NOTES_CHARS);
        assert_eq!(shortened.chars().count(), MAX_NOTES_CHARS + 1);
        assert!(shortened.ends_with('…'));
    }

    /// 手动验证联网路径：`cargo test -p gpui-pdf check_github -- --ignored --nocapture`
    #[test]
    #[ignore = "需要联网访问 GitHub API"]
    fn check_github_release() {
        let result = check_latest();
        println!("{result:?}");
        assert!(result.is_ok(), "{result:?}");
    }
}
