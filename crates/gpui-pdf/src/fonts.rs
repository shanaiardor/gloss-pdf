//! 默认界面字体：LXGW 文楷 TC。
//!
//! 字体文件放在 `assets/fonts/lxgw-wenkai-tc`，编译时直接嵌进二进制：分发给
//! 别人时只要带上 exe，界面就是文楷，不再依赖某台机器上的下载目录。
//!
//! 想在不重新编译的情况下换字体版本，把 `GPUI_PDF_FONT_DIR` 指向包含
//! `LXGWWenKaiTC-Regular.ttf` 与 `LXGWWenKaiMonoTC-Regular.ttf` 的目录即可。
//! 目录不存在、缺文件或读取失败时回退到内置字体；内置字体也注册失败时，
//! GPUI 会退到系统 UI 字体，不影响启动。

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use gpui::App;

/// 与字体文件 name 表里的 family 完全一致，否则 DirectWrite 匹配不到。
pub(crate) const UI_FAMILY: &str = "LXGW WenKai TC";
pub(crate) const MONO_FAMILY: &str = "LXGW WenKai Mono TC";

const FONT_DIR_ENV: &str = "GPUI_PDF_FONT_DIR";
const FONT_FILES: &[&str] = &["LXGWWenKaiTC-Regular.ttf", "LXGWWenKaiMonoTC-Regular.ttf"];

/// `include_bytes!` 的数据位于只读段，生命周期是 `'static`。
///
/// DirectWrite 的内存字体引用只保存指针、不拷贝数据，所以这里必须传
/// `Cow::Borrowed`；用运行期读出来的 `Vec<u8>` 会留下悬垂指针。
const EMBEDDED_FONTS: &[(&str, &[u8])] = &[
    (
        "LXGWWenKaiTC-Regular.ttf",
        include_bytes!("../assets/fonts/lxgw-wenkai-tc/LXGWWenKaiTC-Regular.ttf"),
    ),
    (
        "LXGWWenKaiMonoTC-Regular.ttf",
        include_bytes!("../assets/fonts/lxgw-wenkai-tc/LXGWWenKaiMonoTC-Regular.ttf"),
    ),
];

/// 注册界面所需的常规体与等宽体。重复调用是安全的。
pub(crate) fn install(cx: &mut App) {
    let (fonts, source) = match override_dir() {
        Some(directory) => match load_dir(&directory) {
            Ok(fonts) => (fonts, directory.display().to_string()),
            Err(error) => {
                log::warn!("{error}，改用内置字体");
                (embedded_fonts(), String::from("内置"))
            }
        },
        None => (embedded_fonts(), String::from("内置")),
    };

    let count = fonts.len();
    match cx.text_system().add_fonts(fonts) {
        Ok(()) => log::info!("registered {count} LXGW fonts ({source})"),
        Err(error) => log::warn!("注册 LXGW 字体失败: {error:#}"),
    }
}

fn embedded_fonts() -> Vec<Cow<'static, [u8]>> {
    EMBEDDED_FONTS
        .iter()
        .map(|(_, bytes)| Cow::Borrowed(*bytes))
        .collect()
}

/// 只有两个字体文件都在时才认为是有效的覆盖目录，避免只换了一半导致
/// 常规体是文楷、等宽体却是系统字体。
fn override_dir() -> Option<PathBuf> {
    let directory = PathBuf::from(std::env::var_os(FONT_DIR_ENV)?);
    let missing: Vec<&str> = FONT_FILES
        .iter()
        .copied()
        .filter(|name| !directory.join(name).is_file())
        .collect();

    if missing.is_empty() {
        Some(directory)
    } else {
        log::warn!(
            "{FONT_DIR_ENV}={} 缺少 {}，改用内置字体",
            directory.display(),
            missing.join("、")
        );
        None
    }
}

/// 读取失败时整组回退，理由同上。
fn load_dir(directory: &Path) -> Result<Vec<Cow<'static, [u8]>>, String> {
    FONT_FILES
        .iter()
        .map(|name| {
            let path = directory.join(name);
            std::fs::read(&path)
                .map(|bytes| {
                    // 同上：DirectWrite 只保留指针，字体数据必须活到进程结束。
                    Cow::Borrowed(&*Box::leak(bytes.into_boxed_slice()))
                })
                .map_err(|error| format!("读取字体 {} 失败: {error}", path.display()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用文件头判断 sfnt（TrueType/OpenType）格式，避免 include 错文件。
    fn is_sfnt(bytes: &[u8]) -> bool {
        matches!(
            bytes.get(..4),
            Some([0x00, 0x01, 0x00, 0x00] | b"OTTO" | b"true" | b"ttcf")
        )
    }

    #[test]
    fn embedded_fonts_are_complete_sfnt_files() {
        assert_eq!(EMBEDDED_FONTS.len(), FONT_FILES.len());

        for (name, bytes) in EMBEDDED_FONTS {
            assert!(bytes.len() > 1_000_000, "{name} 体积异常");
            assert!(is_sfnt(bytes), "{name} 不是有效的字体文件");
        }
    }

    #[test]
    fn embedded_font_order_matches_lookup_order() {
        let names: Vec<&str> = EMBEDDED_FONTS.iter().map(|(name, _)| *name).collect();

        assert_eq!(names, FONT_FILES);
    }
}
