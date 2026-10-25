//! PDFium 动态库的自动补齐。
//!
//! 发行包会带上动态库，但本地编译、更新覆盖或手动清理后，运行时常常只剩一句
//! 「找不到 libpdfium」。这里在第一次需要时从 pdfium-binaries 下载当前平台的
//! 构建，放进用户数据目录，之后所有查找路径都能命中。

use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use flate2::read::GzDecoder;

/// 压缩包体积上限：当前各平台最大约 7 MiB，留足冗余并防止异常响应撑爆内存。
const MAX_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024;
/// 单个动态库解压后的体积上限。
const MAX_LIBRARY_BYTES: u64 = 256 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const MANAGED_DIRECTORY: &str = "pdfium";
/// 自动补齐失败后的冷却时间。首页缩略图会连续触发绑定，不能每次都等网络超时。
const RETRY_DELAY: Duration = Duration::from_secs(60);
static LAST_FAILURE: Mutex<Option<Instant>> = Mutex::new(None);

#[cfg(target_os = "windows")]
pub(crate) const LIBRARY_FILE_NAME: &str = "pdfium.dll";
#[cfg(target_os = "macos")]
pub(crate) const LIBRARY_FILE_NAME: &str = "libpdfium.dylib";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) const LIBRARY_FILE_NAME: &str = "libpdfium.so";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PLATFORM_TARGET: Option<&str> = Some("mac-arm64");
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const PLATFORM_TARGET: Option<&str> = Some("mac-x64");
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
const PLATFORM_TARGET: Option<&str> = Some("win-arm64");
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const PLATFORM_TARGET: Option<&str> = Some("win-x64");
#[cfg(all(target_os = "windows", target_arch = "x86"))]
const PLATFORM_TARGET: Option<&str> = Some("win-x86");
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const PLATFORM_TARGET: Option<&str> = Some("linux-arm64");
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PLATFORM_TARGET: Option<&str> = Some("linux-x64");
#[cfg(all(target_os = "linux", target_arch = "arm"))]
const PLATFORM_TARGET: Option<&str> = Some("linux-arm");
#[cfg(all(target_os = "linux", target_arch = "x86"))]
const PLATFORM_TARGET: Option<&str> = Some("linux-x86");
#[cfg(not(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "aarch64"),
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "arm"),
    all(target_os = "linux", target_arch = "x86"),
)))]
const PLATFORM_TARGET: Option<&str> = None;

/// 自动下载的库放在各平台统一的用户数据目录里，避免写入不可控的安装目录。
fn data_directory() -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("share"))
            })
    }?;
    Some(base.join("gloss-pdf").join(MANAGED_DIRECTORY))
}

pub(crate) fn managed_library_path() -> Option<PathBuf> {
    data_directory().map(|directory| directory.join(LIBRARY_FILE_NAME))
}

pub(crate) fn can_retry() -> bool {
    LAST_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_none_or(|failed_at| failed_at.elapsed() >= RETRY_DELAY)
}

fn note_failure() {
    *LAST_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
}

/// 下载并安装当前平台的 PDFium；已存在时直接返回路径。
///
/// 只会在调用方确认「找不到可用的库」之后进来，因此失败时必须给出可操作的信息，
/// 而不是让调用点只看到一句 dlopen 错误。
pub(crate) fn install(report: &mut dyn FnMut(&str)) -> Result<PathBuf> {
    // 阅读器与缩略图两个 worker 都可能触发补齐，串行化避免重复下载。
    static INSTALL_LOCK: Mutex<()> = Mutex::new(());
    let _guard = INSTALL_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let directory = data_directory().context("无法确定用户数据目录，不能自动下载 PDFium")?;
    let library = directory.join(LIBRARY_FILE_NAME);
    if library.is_file() {
        return Ok(library);
    }

    let target = PLATFORM_TARGET.context("当前平台没有可用的 PDFium 预编译包")?;
    let url = std::env::var("GLOSS_PDF_PDFIUM_URL")
        .ok()
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| {
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-{target}.tgz"
            )
        });

    log::info!("downloading PDFium ({target}) from {url}");
    report("缺少 PDFium 运行库，正在自动下载…");
    let archive = download(&url).inspect_err(|_| note_failure())?;
    let bytes = extract_library(&archive, LIBRARY_FILE_NAME).inspect_err(|_| note_failure())?;

    std::fs::create_dir_all(&directory)
        .with_context(|| format!("无法创建 {}", directory.display()))?;
    let temporary = directory.join(format!(".{LIBRARY_FILE_NAME}.download"));
    std::fs::write(&temporary, &bytes)
        .with_context(|| format!("无法写入 {}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, &library) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("无法保存 {}", library.display()));
    }

    log::info!("PDFium installed to {}", library.display());
    report("PDFium 运行库已就绪");
    Ok(library)
}

fn download(url: &str) -> Result<Vec<u8>> {
    let mut response = ureq::get(url)
        .config()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .call()
        .with_context(|| format!("下载 PDFium 失败（{url}）"))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_ARCHIVE_BYTES)
        .read_to_vec()
        .with_context(|| format!("读取 PDFium 安装包失败（{url}）"))?;
    if bytes.is_empty() {
        bail!("下载到的 PDFium 安装包为空（{url}）");
    }
    Ok(bytes)
}

fn extract_library(archive: &[u8], library_name: &str) -> Result<Vec<u8>> {
    let decoder = GzDecoder::new(Cursor::new(archive));
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .context("PDFium 安装包格式无效（不是 tar.gz）")?;

    for entry in entries {
        let mut entry = entry.context("读取 PDFium 安装包条目失败")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().context("PDFium 安装包条目路径无效")?;
        if path.file_name().and_then(|name| name.to_str()) != Some(library_name) {
            continue;
        }

        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(MAX_LIBRARY_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("解压 PDFium 动态库失败")?;
        if bytes.len() as u64 > MAX_LIBRARY_BYTES {
            bail!("PDFium 安装包中的 {library_name} 体积异常");
        }
        if !looks_like_library(&bytes) {
            bail!("PDFium 安装包中的 {library_name} 不是有效的动态库");
        }
        return Ok(bytes);
    }

    bail!("PDFium 安装包中未找到 {library_name}")
}

fn looks_like_library(bytes: &[u8]) -> bool {
    if cfg!(target_os = "windows") {
        return bytes.starts_with(b"MZ");
    }
    if cfg!(target_os = "macos") {
        // 单架构 Mach-O（两种字节序）与 universal/fat 头都接受。
        return [
            &[0xcf, 0xfa, 0xed, 0xfe][..],
            &[0xce, 0xfa, 0xed, 0xfe][..],
            &[0xfe, 0xed, 0xfa, 0xcf][..],
            &[0xfe, 0xed, 0xfa, 0xce][..],
            &[0xca, 0xfe, 0xba, 0xbe][..],
            &[0xbe, 0xba, 0xfe, 0xca][..],
        ]
        .iter()
        .any(|magic| bytes.starts_with(magic));
    }
    bytes.starts_with(b"\x7fELF")
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::{Builder, Header};

    fn library_bytes() -> Vec<u8> {
        let mut bytes = if cfg!(target_os = "windows") {
            b"MZ".to_vec()
        } else if cfg!(target_os = "macos") {
            vec![0xcf, 0xfa, 0xed, 0xfe]
        } else {
            b"\x7fELF".to_vec()
        };
        bytes.extend_from_slice(&[0u8; 64]);
        bytes
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let mut builder = Builder::new(encoder);
        for (path, data) in entries {
            let mut header = Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extracts_the_platform_library() {
        let payload = library_bytes();
        let bytes = archive(&[
            ("LICENSE", b"BSD-3-Clause"),
            ("lib/unrelated.so", b"junk"),
            (LIBRARY_FILE_NAME, &payload),
        ]);
        assert_eq!(extract_library(&bytes, LIBRARY_FILE_NAME).unwrap(), payload);
    }

    #[test]
    fn rejects_archive_without_the_library() {
        let bytes = archive(&[("README.md", b"nothing here")]);
        let error = extract_library(&bytes, LIBRARY_FILE_NAME).unwrap_err();
        assert!(error.to_string().contains("未找到"));
    }

    #[test]
    fn rejects_a_file_with_the_wrong_format() {
        let bytes = archive(&[(LIBRARY_FILE_NAME, b"not a dynamic library")]);
        let error = extract_library(&bytes, LIBRARY_FILE_NAME).unwrap_err();
        assert!(error.to_string().contains("不是有效的动态库"));
    }

    #[test]
    fn recognizes_platform_magic() {
        assert!(looks_like_library(&library_bytes()));
        assert!(!looks_like_library(b"plain text"));
    }

    #[test]
    fn managed_path_uses_the_platform_library_name() {
        let path = managed_library_path().unwrap();
        assert_eq!(path.file_name().unwrap(), LIBRARY_FILE_NAME);
        assert!(path.to_string_lossy().contains("gloss-pdf"));
    }
}
