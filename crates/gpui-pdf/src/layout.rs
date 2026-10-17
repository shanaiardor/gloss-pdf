//! 阅读器布局：页边距、适宽/适页缩放，以及按纸张比例选择窗口尺寸。
//!
//! 这里只做纯计算，不碰 GPUI，方便直接写单元测试。

use pdf_core::PageSize;
use serde::{Deserialize, Serialize};

/// 页面左右留白。
pub const PAGE_PAD: f32 = 28.0;
/// 页面顶部留白：让浮动胶囊不压住页面。
pub const TOP_INSET: f32 = 56.0;
/// 页面底部留白：给底部胶囊留位置。
pub const BOTTOM_INSET: f32 = 88.0;
/// 阅读区上下留白总和。
pub const VERTICAL_INSETS: f32 = TOP_INSET + BOTTOM_INSET;

pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 4.0;

/// 自动窗口尺寸最多占工作区的比例，给桌面留一点空间。
pub const WINDOW_FILL: f32 = 0.94;
/// 窗口离屏幕边缘至少留出的空隙。
pub const SCREEN_MARGIN: f32 = 24.0;
/// 再窄底部胶囊就放不下了。
pub const MIN_WINDOW_WIDTH: f32 = 640.0;
pub const MIN_WINDOW_HEIGHT: f32 = 520.0;

/// 缩放模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    /// 手动缩放；窗口尺寸变化时不再自动改缩放。
    #[default]
    Free,
    /// 页面宽度撑满阅读区，高度可以滚动。
    Width,
    /// 整页都放进阅读区，宽高同时适应。
    Page,
}

/// 阅读区里真正能放页面的尺寸（已扣掉页边距和浮动胶囊）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitArea {
    pub width: f32,
    pub height: f32,
}

impl FitArea {
    /// 从窗口视口尺寸算出页面可用区域。
    pub fn from_viewport(width: f32, height: f32) -> Self {
        Self {
            width: (width - PAGE_PAD * 2.0).max(1.0),
            height: (height - VERTICAL_INSETS).max(1.0),
        }
    }
}

/// 按模式算出缩放。页面尺寸无效或模式是手动缩放时返回 `None`。
pub fn zoom_for(page: PageSize, area: FitArea, mode: FitMode) -> Option<f32> {
    if !page.width.is_finite() || !page.height.is_finite() || page.width <= 0.0 || page.height <= 0.0
    {
        return None;
    }

    let zoom = match mode {
        FitMode::Free => return None,
        FitMode::Width => area.width / page.width,
        FitMode::Page => (area.width / page.width).min(area.height / page.height),
    };

    zoom.is_finite().then(|| zoom.clamp(MIN_ZOOM, MAX_ZOOM))
}

/// 为一张页面挑一个窗口内容尺寸。
///
/// `room_width` / `room_height` 是窗口当前还能占用的空间（已考虑屏幕工作区和
/// 窗口当前位置）。页面按比例尽量填满，同时夹在最小窗口尺寸和可用空间之间。
pub fn window_size_for_page(page: PageSize, room_width: f32, room_height: f32) -> (f32, f32) {
    let room_width = room_width.max(1.0);
    let room_height = room_height.max(1.0);

    if !page.width.is_finite()
        || !page.height.is_finite()
        || page.width <= 0.0
        || page.height <= 0.0
    {
        return (room_width, room_height);
    }

    let scale = ((room_width - PAGE_PAD * 2.0) / page.width)
        .min((room_height - VERTICAL_INSETS) / page.height)
        .clamp(MIN_ZOOM, MAX_ZOOM);

    let width = page.width * scale + PAGE_PAD * 2.0;
    let height = page.height * scale + VERTICAL_INSETS;

    (
        width.clamp(MIN_WINDOW_WIDTH.min(room_width), room_width),
        height.clamp(MIN_WINDOW_HEIGHT.min(room_height), room_height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a4_portrait() -> PageSize {
        PageSize {
            width: 595.0,
            height: 842.0,
        }
    }

    #[test]
    fn fit_page_uses_the_tighter_dimension() {
        let area = FitArea::from_viewport(1040.0, 720.0);
        assert_eq!(area.width, 984.0);
        assert_eq!(area.height, 576.0);

        let zoom = zoom_for(a4_portrait(), area, FitMode::Page).unwrap();
        // 高度先到极限：576 / 842。
        assert!((zoom - 576.0 / 842.0).abs() < 0.0001);
    }

    #[test]
    fn fit_width_ignores_viewport_height() {
        let area = FitArea::from_viewport(1040.0, 200.0);
        let zoom = zoom_for(a4_portrait(), area, FitMode::Width).unwrap();
        assert!((zoom - 984.0 / 595.0).abs() < 0.0001);
    }

    #[test]
    fn fit_respects_min_and_max_zoom() {
        let tiny = PageSize {
            width: 1.0,
            height: 1.0,
        };
        let huge = PageSize {
            width: 100_000.0,
            height: 100_000.0,
        };
        let area = FitArea::from_viewport(1040.0, 720.0);
        assert_eq!(zoom_for(tiny, area, FitMode::Page), Some(MAX_ZOOM));
        assert_eq!(zoom_for(huge, area, FitMode::Page), Some(MIN_ZOOM));
        assert_eq!(zoom_for(huge, area, FitMode::Free), None);
    }

    #[test]
    fn window_size_keeps_page_aspect_and_fits_room() {
        // 从默认 1040x720 窗口打开 A4：高度受当前位置限制，宽度保底 640。
        let (width, height) = window_size_for_page(a4_portrait(), 1804.0, 836.0);
        assert!((width - MIN_WINDOW_WIDTH).abs() < 0.01);
        assert!((height - 836.0).abs() < 0.01);

        // 可用空间越大，窗口越接近纸张比例。
        let (width, height) = window_size_for_page(a4_portrait(), 1804.0, 1200.0);
        let page_ratio = a4_portrait().width / a4_portrait().height;
        let content_ratio = (width - PAGE_PAD * 2.0) / (height - VERTICAL_INSETS);
        assert!((page_ratio - content_ratio).abs() < 0.001);
    }

    #[test]
    fn landscape_page_gets_a_wide_window() {
        let page = PageSize {
            width: 842.0,
            height: 595.0,
        };
        let (width, height) = window_size_for_page(page, 1804.0, 836.0);
        let page_ratio = page.width / page.height;
        let content_ratio = (width - PAGE_PAD * 2.0) / (height - VERTICAL_INSETS);
        assert!((page_ratio - content_ratio).abs() < 0.001);
    }

    #[test]
    fn invalid_page_falls_back_to_room() {
        let (width, height) = window_size_for_page(PageSize::default(), 700.0, 800.0);
        assert_eq!((width, height), (700.0, 800.0));
    }

    #[test]
    fn fit_mode_serializes_as_snake_case() {
        assert_eq!(serde_json::to_string(&FitMode::Page).unwrap(), "\"page\"");
        assert_eq!(serde_json::to_string(&FitMode::Width).unwrap(), "\"width\"");
        assert_eq!(serde_json::to_string(&FitMode::Free).unwrap(), "\"free\"");
    }
}
