//! 默认界面字体：在窗口创建前把 LXGW 文楷注册到 GPUI 的文本系统。
//!
//! 字体文件不打包进仓库，按以下顺序查找目录：
//! 1. `GPUI_PDF_FONT_DIR` 环境变量；
//! 2. 可执行文件旁边的 `fonts/lxgw-wenkai-tc` 或 `assets/fonts/lxgw-wenkai-tc`；
//! 3. 开发目录的 `crates/gpui-pdf/assets/fonts/lxgw-wenkai-tc`；
//! 4. 本机下载目录 `C:\Users\shanay\Downloads\lxgw-wenkai-tc-v1.522`。
//!
//! 都找不到时只记录警告，GPUI 会回退到系统 UI 字体，不会影响启动。

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use gpui::App;

const FONT_DIR_ENV: &str = "GPUI_PDF_FONT_DIR";
const DOWNLOAD_DIR: &str = r"C:\Users\shanay\Downloads\lxgw-wenkai-tc-v1.522";
const FONT_FILES: &[&str] = &[
    "LXGWWenKaiTC-Regular.ttf",
    "LXGWWenKaiMonoTC-Regular.ttf",
];

/// 注册界面所需的常规体与等宽体。重复调用是安全的。
pub(crate) fn install(cx: &mut App) {
    let Some(directory) = font_dir() else {
        log::warn!(
            "未找到 LXGW 文楷字体，界面将回退到系统字体；可设置 {FONT_DIR_ENV} 指向字体目录"
        );
        return;
    };

    let mut fonts = Vec::with_capacity(FONT_FILES.len());
    for name in FONT_FILES {
        let path = directory.join(name);
        match std::fs::read(&path) {
            Ok(bytes) => fonts.push(Cow::Owned(bytes)),
            Err(error) => log::warn!("读取字体 {} 失败: {error}", path.display()),
        }
    }

    if fonts.is_empty() {
        log::warn!("字体目录 {} 中没有可用的 LXGW 字体", directory.display());
        return;
    }

    let count = fonts.len();
    match cx.text_system().add_fonts(fonts) {
        Ok(()) => log::info!(
            "registered {count} LXGW fonts from {}",
            directory.display()
        ),
        Err(error) => log::warn!("注册 LXGW 字体失败: {error:#}"),
    }
}

fn font_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(directory) = std::env::var_os(FONT_DIR_ENV) {
        candidates.push(PathBuf::from(directory));
    }

    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join("fonts/lxgw-wenkai-tc"));
        candidates.push(directory.join("assets/fonts/lxgw-wenkai-tc"));
    }

    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/lxgw-wenkai-tc"),
    );
    candidates.push(PathBuf::from(DOWNLOAD_DIR));

    candidates
        .into_iter()
        .find(|directory| has_font(directory))
}

fn has_font(directory: &Path) -> bool {
    FONT_FILES
        .iter()
        .any(|name| directory.join(name).is_file())
}
