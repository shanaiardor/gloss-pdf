//! 应用主题：颜色与字体集中在这里，方便以后整体替换或接设计稿。

use gpui::{Rgba, SharedString, rgb, rgba};

#[cfg(target_os = "windows")]
const UI_FONT: &str = "Segoe UI";
#[cfg(target_os = "macos")]
const UI_FONT: &str = "SF Pro Text";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const UI_FONT: &str = "Noto Sans";

#[cfg(target_os = "windows")]
const MONO_FONT: &str = "Cascadia Mono";
#[cfg(target_os = "macos")]
const MONO_FONT: &str = "SF Mono";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const MONO_FONT: &str = "Noto Sans Mono";

/// 一套完整的配色 + 字体。
#[derive(Clone)]
pub struct Theme {
    pub dark: bool,
    pub bg: Rgba,
    pub surface: Rgba,
    pub surface_hover: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub accent: Rgba,
    pub accent_hover: Rgba,
    pub accent_text: Rgba,
    pub selection: Rgba,
    pub search_hit: Rgba,
    pub search_current: Rgba,
    pub highlight: Rgba,
    pub font_family: SharedString,
    pub mono_font: SharedString,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            dark: true,
            bg: rgb(0x0f1115),
            surface: rgb(0x171a21),
            surface_hover: rgb(0x1f232c),
            border: rgb(0x2a2f3a),
            text: rgb(0xe6e8ee),
            text_muted: rgb(0x8b93a7),
            accent: rgb(0x6ea8fe),
            accent_hover: rgb(0x8fbcff),
            accent_text: rgb(0x0b0f16),
            selection: rgba(0x6ea8fe55),
            search_hit: rgba(0xfacc1540),
            search_current: rgba(0xfb923c88),
            highlight: rgba(0xfacc1566),

            font_family: UI_FONT.into(),
            mono_font: MONO_FONT.into(),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            bg: rgb(0xfbfaf6),
            surface: rgb(0xfffefb),
            surface_hover: rgb(0xf1efe9),
            border: rgb(0xe7e4dc),
            text: rgb(0x24231f),
            text_muted: rgb(0x8b8982),
            accent: rgb(0x2563eb),
            accent_hover: rgb(0x1d4ed8),
            accent_text: rgb(0xffffff),
            selection: rgba(0x2563eb44),
            search_hit: rgba(0xfacc1540),
            search_current: rgba(0xea580c88),
            highlight: rgba(0xfacc1566),

            font_family: UI_FONT.into(),
            mono_font: MONO_FONT.into(),
        }
    }

    /// 在深色 / 浅色之间切换。
    pub fn toggled(self) -> Self {
        if self.dark {
            Self::light()
        } else {
            Self::dark()
        }
    }

    pub fn name(&self) -> &'static str {
        if self.dark { "深色" } else { "浅色" }
    }
}
