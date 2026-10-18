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

/// 连续滚动时相邻两页之间的空隙。
pub const PAGE_GAP: f32 = 16.0;
/// 可视范围上下各多渲染几页，滚动时不会先看到空白再补上。
pub const PREFETCH_PAGES: usize = 1;
/// 阅读锚线在视口里的纵向位置比例：取中线，页码在页面过半时切换。
pub const ANCHOR_RATIO: f32 = 0.5;

/// 缩放步长（百分点）：键盘/胶囊 ± 按钮一次一步，Ctrl + 滚轮一行一步。
pub const ZOOM_STEP_PERCENT: f32 = 1.0;

/// 按住中键拖动时内容的跟随比例：指针走 2px，内容滚 1px。
///
/// 取的是一半速度而不是 1:1，拖起来慢而稳，长文档里也容易停在某一行上。
pub const PAN_RATIO: f32 = 0.5;

/// 中键拖动时指针位移换算成滚动位移：反方向、半速。
pub fn pan_delta(distance: f32) -> f32 {
    if !distance.is_finite() {
        return 0.0;
    }
    -distance * PAN_RATIO
}

/// 按百分点步进缩放：先把当前比例对齐到整百分点，再加减 `percent`。
///
/// 键盘与底部胶囊的 ± 按钮用它，保证每次刚好变 1%；适页/适宽算出来的
/// 小数比例也先对齐，显示出来的百分比才会一格一格地走。
pub fn zoom_step(zoom: f32, percent: f32) -> f32 {
    let base = (zoom * 100.0).round();
    ((base + percent) / 100.0).clamp(MIN_ZOOM, MAX_ZOOM)
}

/// 在现有比例上连续加减百分点。
///
/// Ctrl + 滚轮用它：触控板的细小增量也能累积，不会因为量化成整百分点而丢掉。
pub fn zoom_add(zoom: f32, percent: f32) -> f32 {
    (zoom + percent / 100.0).clamp(MIN_ZOOM, MAX_ZOOM)
}

/// 把滚轮行数换算成缩放百分点：一行 1%，一格约 3 行（即 3%）。
/// 单次事件最多夹到两格，避免触控板一次报个大增量就把缩放甩到底。
pub(crate) fn wheel_zoom_percent(lines: f32) -> f32 {
    if !lines.is_finite() {
        return 0.0;
    }
    lines.clamp(-6.0, 6.0) * ZOOM_STEP_PERCENT
}

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
    if !page.width.is_finite()
        || !page.height.is_finite()
        || page.width <= 0.0
        || page.height <= 0.0
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

/// 一页在滚动内容里的位置与尺寸（已乘缩放，单位像素）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBox {
    /// 页顶在内容坐标里的 y（内容坐标原点在滚动内容左上角）。
    pub top: f32,
    pub height: f32,
    pub width: f32,
    /// 页左在内容坐标里的 x；页面按阅读区宽度居中。
    pub left: f32,
}

impl PageBox {
    pub fn bottom(&self) -> f32 {
        self.top + self.height
    }
}

/// 整篇文档的排版：把各页从上到下排成一列，页间留 `PAGE_GAP`。
///
/// 连续滚动全靠这份几何：滚动偏移定位到哪一页、鼠标落在哪一页、
/// 当前该渲染哪几页，都由它算，不依赖 GPUI 的布局结果。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocumentLayout {
    boxes: Vec<PageBox>,
    /// 滚动内容的总高，含上下留白。
    pub content_height: f32,
    /// 滚动内容的总宽：不超过视口，页面更宽时横向可滚动。
    pub content_width: f32,
}

impl DocumentLayout {
    pub fn new(pages: &[PageSize], zoom: f32, viewport_width: f32) -> Self {
        let zoom = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        let viewport_width = if viewport_width.is_finite() {
            viewport_width.max(0.0)
        } else {
            0.0
        };

        let mut boxes = Vec::with_capacity(pages.len());
        let mut top = TOP_INSET;
        let mut right_edge: f32 = 0.0;

        for (index, page) in pages.iter().enumerate() {
            if index > 0 {
                top += PAGE_GAP;
            }
            let width = (page.width * zoom).max(1.0);
            let height = (page.height * zoom).max(1.0);
            let available = viewport_width - PAGE_PAD * 2.0;
            let left = PAGE_PAD + ((available - width) / 2.0).max(0.0);
            boxes.push(PageBox {
                top,
                height,
                width,
                left,
            });
            right_edge = right_edge.max(left + width + PAGE_PAD);
            top += height;
        }

        let content_height = if boxes.is_empty() {
            VERTICAL_INSETS
        } else {
            top + BOTTOM_INSET
        };

        Self {
            boxes,
            content_height,
            content_width: right_edge.max(viewport_width),
        }
    }

    #[allow(dead_code)] // 测试与调试用；界面只关心 is_empty
    pub fn len(&self) -> usize {
        self.boxes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    pub fn page_box(&self, page: usize) -> Option<PageBox> {
        self.boxes.get(page).copied()
    }

    /// 锚线所在的页：`y` 落在页间空隙时仍算上一页，直到下一页的页顶越过锚线。
    ///
    /// 这就是「翻到第几页」的判定：锚线固定在视口顶部往下 `TOP_INSET` 处，
    /// 下一页的页顶越过它时页码才加一，避免空隙里页码来回跳。
    pub fn page_at(&self, y: f32) -> usize {
        if self.boxes.is_empty() {
            return 0;
        }
        let y = if y.is_finite() { y } else { 0.0 };
        self.boxes
            .partition_point(|page| page.top <= y)
            .saturating_sub(1)
    }

    /// 与 `[top, top + height]` 相交的页，再向两侧各扩 `overscan` 页。
    pub fn visible_range(&self, top: f32, height: f32, overscan: usize) -> std::ops::Range<usize> {
        if self.boxes.is_empty() {
            return 0..0;
        }
        let top = if top.is_finite() { top } else { 0.0 };
        let bottom = top
            + if height.is_finite() {
                height.max(0.0)
            } else {
                0.0
            };

        // 页底仍在视口之下说明它至少露了一部分，包含它。
        let first = self.boxes.partition_point(|page| page.bottom() <= top);
        let end = self
            .boxes
            .partition_point(|page| page.top < bottom)
            .max(first + 1);

        let start = first.saturating_sub(overscan);
        start..(end + overscan).min(self.boxes.len())
    }

    /// 内容坐标 `(x, y)` 落在哪一页、页内横纵比例各是多少。
    ///
    /// 比例夹在 0..1：指针落在页边留白或页间空隙时，仍以最近的一页为锚。
    pub fn page_ratios(&self, x: f32, y: f32) -> Option<(usize, f32, f32)> {
        let page = self.page_at(y);
        let page_box = self.page_box(page)?;
        let ratio_x = ((x - page_box.left) / page_box.width.max(1.0)).clamp(0.0, 1.0);
        let ratio_y = ((y - page_box.top) / page_box.height.max(1.0)).clamp(0.0, 1.0);
        Some((page, ratio_x, ratio_y))
    }

    /// 让「第 `page` 页的 `(ratio_x, ratio_y)` 点」落在视口 `(screen_x, screen_y)` 处
    /// 所需的滚动偏移（左、上，单位像素），已夹在内容范围内。
    pub fn scroll_for_ratios(
        &self,
        page: usize,
        ratio_x: f32,
        ratio_y: f32,
        screen_x: f32,
        screen_y: f32,
        viewport_width: f32,
        viewport_height: f32,
    ) -> Option<(f32, f32)> {
        let page_box = self.page_box(page)?;
        let left = page_box.left + ratio_x.clamp(0.0, 1.0) * page_box.width - screen_x;
        let top = page_box.top + ratio_y.clamp(0.0, 1.0) * page_box.height - screen_y;
        let max_left = (self.content_width - viewport_width.max(0.0)).max(0.0);
        let max_top = (self.content_height - viewport_height.max(0.0)).max(0.0);
        Some((left.clamp(0.0, max_left), top.clamp(0.0, max_top)))
    }

    /// 把滚动位置夹到合法范围，返回内容坐标下的顶部偏移（≥ 0）。
    pub fn clamp_scroll_top(&self, top: f32, viewport_height: f32) -> f32 {
        let viewport_height = if viewport_height.is_finite() {
            viewport_height.max(0.0)
        } else {
            0.0
        };
        let max = (self.content_height - viewport_height).max(0.0);
        let top = if top.is_finite() { top } else { 0.0 };
        top.clamp(0.0, max)
    }

    /// 视口顶部停在内容坐标 `top` 时，阅读锚线落在第几页、页内比例多少。
    ///
    /// 锚线取视口竖直中线：页码在页面过半时切换，比「页顶刚滚出去才切」更跟手；
    /// 滚到文档末尾（最后一页比视口矮）时中线会落到最后一页上，页码不会退回上一页。
    /// 这个结果同时用于翻页判定和保存阅读进度，所以恢复时能精确回到原处。
    pub fn anchor_at(&self, top: f32, viewport_height: f32) -> (usize, f32) {
        if self.boxes.is_empty() {
            return (0, 0.0);
        }
        let viewport_height = if viewport_height.is_finite() {
            viewport_height.max(0.0)
        } else {
            0.0
        };
        let top = self.clamp_scroll_top(top, viewport_height);
        let anchor = top + viewport_height * ANCHOR_RATIO;

        let page = self.page_at(anchor);
        let ratio = self.page_box(page).map_or(0.0, |page_box| {
            ((anchor - page_box.top) / page_box.height).clamp(0.0, 1.0)
        });
        (page, ratio)
    }

    /// 锚点 → 滚动顶部位置：`anchor_at` 的逆运算。
    ///
    /// 页面顶端最高只能停在顶部留白处：旧版本记录的是「页顶比例」，
    /// 比例接近 0 时按这条下界恢复，老进度不会把页面推到屏幕中间。
    pub fn scroll_top_for_anchor(&self, page: usize, ratio: f32, viewport_height: f32) -> f32 {
        let Some(page_box) = self.page_box(page) else {
            return 0.0;
        };
        let viewport_height = if viewport_height.is_finite() {
            viewport_height.max(0.0)
        } else {
            0.0
        };
        let anchor = page_box.top + ratio.clamp(0.0, 1.0) * page_box.height;
        let top = (anchor - viewport_height * ANCHOR_RATIO).max(page_box.top - TOP_INSET);
        self.clamp_scroll_top(top, viewport_height)
    }
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

    fn pages(count: usize, width: f32, height: f32) -> Vec<PageSize> {
        vec![PageSize { width, height }; count]
    }

    #[test]
    fn document_layout_stacks_pages_with_gaps_and_insets() {
        let layout = DocumentLayout::new(&pages(3, 600.0, 800.0), 1.0, 1000.0);

        assert_eq!(layout.len(), 3);
        assert_eq!(layout.page_box(0).unwrap().top, TOP_INSET);
        assert_eq!(
            layout.page_box(1).unwrap().top,
            TOP_INSET + 800.0 + PAGE_GAP
        );
        assert_eq!(
            layout.page_box(2).unwrap().top,
            TOP_INSET + 2.0 * (800.0 + PAGE_GAP)
        );
        // 内容高度 = 上下留白 + 三页 + 两道页间距。
        assert_eq!(
            layout.content_height,
            VERTICAL_INSETS + 2400.0 + 2.0 * PAGE_GAP
        );
        // 页面比视口窄时居中，内容宽度不缩到页面宽度。
        assert_eq!(layout.content_width, 1000.0);
        let first = layout.page_box(0).unwrap();
        assert!((first.left - (PAGE_PAD + (1000.0 - PAGE_PAD * 2.0 - 600.0) / 2.0)).abs() < 0.001);
    }

    #[test]
    fn document_layout_scrolls_horizontally_for_wide_pages() {
        // 400% 放大后页面比视口宽，内容宽度要跟着长出来。
        let layout = DocumentLayout::new(&pages(2, 600.0, 800.0), 4.0, 1000.0);
        assert_eq!(layout.page_box(0).unwrap().width, 2400.0);
        assert_eq!(layout.content_width, PAGE_PAD * 2.0 + 2400.0);
        assert_eq!(layout.page_box(0).unwrap().left, PAGE_PAD);
    }

    #[test]
    fn page_at_follows_the_anchor_line_through_gaps() {
        let layout = DocumentLayout::new(&pages(2, 600.0, 800.0), 1.0, 1000.0);
        let second_top = layout.page_box(1).unwrap().top;

        assert_eq!(layout.page_at(0.0), 0);
        assert_eq!(layout.page_at(TOP_INSET), 0);
        // 页间空隙里仍算上一页，直到下一页页顶越过锚线。
        assert_eq!(layout.page_at(second_top - PAGE_GAP / 2.0), 0);
        assert_eq!(layout.page_at(second_top), 1);
        // 越过最后一页也不越界。
        assert_eq!(layout.page_at(layout.content_height * 2.0), 1);
    }

    #[test]
    fn visible_range_includes_partially_visible_neighbours() {
        let layout = DocumentLayout::new(&pages(5, 600.0, 800.0), 1.0, 1000.0);
        let second = layout.page_box(1).unwrap();

        // 视口正好盖住第 2 页，不含预取时只有它自己。
        assert_eq!(layout.visible_range(second.top, 800.0, 0), 1..2);
        // 视口压在两页交界处：两页都要渲染。
        assert_eq!(layout.visible_range(second.top - 400.0, 800.0, 0), 0..2);
        // 预取各多一页。
        assert_eq!(layout.visible_range(second.top, 800.0, 1), 0..3);
        // 顶部与底部的夹取。
        assert_eq!(layout.visible_range(0.0, 100.0, 1), 0..2);
        let last_top = layout.page_box(4).unwrap().top;
        assert_eq!(layout.visible_range(last_top, 800.0, 1), 3..5);
    }

    #[test]
    fn empty_document_layout_is_harmless() {
        let layout = DocumentLayout::new(&[], 1.0, 800.0);
        assert!(layout.is_empty());
        assert_eq!(layout.page_at(100.0), 0);
        assert_eq!(layout.visible_range(0.0, 500.0, 1), 0..0);
        assert_eq!(layout.content_height, VERTICAL_INSETS);
        assert_eq!(layout.clamp_scroll_top(50.0, 400.0), 0.0);
    }

    #[test]
    fn clamp_scroll_top_keeps_offset_inside_content() {
        let layout = DocumentLayout::new(&pages(2, 600.0, 800.0), 1.0, 1000.0);
        assert_eq!(layout.clamp_scroll_top(-30.0, 600.0), 0.0);

        let max = layout.content_height - 600.0;
        assert_eq!(layout.clamp_scroll_top(max + 500.0, 600.0), max);
        // 内容比视口矮时不能滚动。
        assert_eq!(layout.clamp_scroll_top(120.0, 5000.0), 0.0);
    }

    #[test]
    fn anchor_at_follows_the_viewport_center() {
        let layout = DocumentLayout::new(&pages(2, 600.0, 800.0), 1.0, 1000.0);
        let second_top = layout.page_box(1).unwrap().top;

        // 视口 600 高，中线在 300：还停在第 1 页。
        assert_eq!(layout.anchor_at(0.0, 600.0).0, 0);
        // 中线越过第二页页顶时切换。
        assert_eq!(layout.anchor_at(second_top - 300.0, 600.0).0, 1);
        assert_eq!(layout.anchor_at(second_top - 301.0, 600.0).0, 0);
    }

    #[test]
    fn anchor_at_the_document_end_stays_on_the_last_page() {
        // 最后一页比视口矮：滚到底时页码不能退回上一页。
        let pages = vec![
            PageSize {
                width: 600.0,
                height: 800.0,
            },
            PageSize {
                width: 600.0,
                height: 120.0,
            },
        ];
        let layout = DocumentLayout::new(&pages, 1.0, 1000.0);
        let (page, ratio) = layout.anchor_at(f32::MAX, 400.0);

        assert_eq!(page, 1);
        assert!(ratio <= 1.0);
        // 这个位置能原样换算回去。
        let top = layout.scroll_top_for_anchor(page, ratio, 400.0);
        assert_eq!(layout.anchor_at(top, 400.0), (page, ratio));
    }

    #[test]
    fn anchor_and_scroll_top_are_inverse() {
        let layout = DocumentLayout::new(&pages(6, 600.0, 800.0), 1.0, 1000.0);
        // 只看中间几页中间几档：首页会被夹到内容顶端，末尾会被最大滚动距离夹住。
        for page in 1..3 {
            for ratio in [0.4, 0.75] {
                let top = layout.scroll_top_for_anchor(page, ratio, 600.0);
                let (found, restored) = layout.anchor_at(top, 600.0);
                assert_eq!(found, page);
                assert!((restored - ratio).abs() < 0.001);
            }
        }
    }

    #[test]
    fn restoring_a_top_of_page_ratio_keeps_the_page_at_the_top() {
        let layout = DocumentLayout::new(&pages(3, 600.0, 800.0), 1.0, 1000.0);
        let second_top = layout.page_box(1).unwrap().top;

        // 旧记录只有「页顶比例 0」，恢复后页顶要落在顶部留白处，而不是被推到屏幕中间。
        let top = layout.scroll_top_for_anchor(1, 0.0, 600.0);
        assert!((top - (second_top - TOP_INSET)).abs() < 0.001);
    }
}

#[cfg(test)]
mod point_anchor_tests {
    use super::*;

    #[test]
    fn zooming_keeps_the_pointer_over_the_same_content_point() {
        let pages = [PageSize {
            width: 600.0,
            height: 800.0,
        }];
        let old = DocumentLayout::new(&pages, 1.0, 800.0);

        // 指针在视口 (300, 400)，滚动到第 0 页页内 30% 处。
        let (page, ratio_x, ratio_y) = old.page_ratios(300.0, 400.0).unwrap();
        let (left, top) = old
            .scroll_for_ratios(page, ratio_x, ratio_y, 300.0, 400.0, 800.0, 600.0)
            .unwrap();
        let content_x = left + 300.0;
        let content_y = top + 400.0;

        let new = DocumentLayout::new(&pages, 2.0, 800.0);
        let (page, ratio_x, ratio_y) = new.page_ratios(content_x, content_y).unwrap();
        let (left, top) = new
            .scroll_for_ratios(page, ratio_x, ratio_y, 300.0, 400.0, 800.0, 600.0)
            .unwrap();

        // 缩放后指针下仍是同一个内容点。
        assert!((left + 300.0 - content_x).abs() < 0.01);
        assert!((top + 400.0 - content_y).abs() < 0.01);
    }

    #[test]
    fn one_wheel_notch_zooms_by_three_percent() {
        // 一行 1%，一格约 3 行。
        assert!((wheel_zoom_percent(3.0) - 3.0).abs() < 0.0001);
        assert!((wheel_zoom_percent(-3.0) + 3.0).abs() < 0.0001);
        assert!((wheel_zoom_percent(1.0) - 1.0).abs() < 0.0001);
        assert!((wheel_zoom_percent(0.0)).abs() < f32::EPSILON);
        // 极大增量夹在两格以内，避免一次事件把缩放甩到底。
        assert!((wheel_zoom_percent(300.0) - 6.0).abs() < 0.0001);
        assert!((wheel_zoom_percent(f32::NAN)).abs() < f32::EPSILON);
    }

    #[test]
    fn zoom_steps_are_one_percent() {
        assert!((zoom_step(1.0, 1.0) - 1.01).abs() < 0.0001);
        assert!((zoom_step(1.0, -1.0) - 0.99).abs() < 0.0001);
        // 适页算出的小数比例先对齐到整百分点，再加减 1%。
        assert!((zoom_step(0.6245, 1.0) - 0.63).abs() < 0.0001);
        assert!((zoom_step(0.6245, -1.0) - 0.61).abs() < 0.0001);
        // 刚好卡在两个百分点之间时向上对齐（round 远离零），之后每次都是整数。
        assert!((zoom_step(0.625, 1.0) - 0.64).abs() < 0.0001);
        assert!((zoom_step(0.625, -1.0) - 0.62).abs() < 0.0001);
        // 夹在 25%–400%。
        assert!((zoom_step(MAX_ZOOM, 1.0) - MAX_ZOOM).abs() < f32::EPSILON);
        assert!((zoom_step(MIN_ZOOM, -1.0) - MIN_ZOOM).abs() < f32::EPSILON);
    }

    #[test]
    fn continuous_zoom_keeps_small_trackpad_deltas() {
        assert!((zoom_add(1.0, 0.5) - 1.005).abs() < 0.0001);
        assert!((zoom_add(1.0, -3.0) - 0.97).abs() < 0.0001);
        assert!((zoom_add(MAX_ZOOM, 3.0) - MAX_ZOOM).abs() < f32::EPSILON);
        assert!((zoom_add(MIN_ZOOM, -3.0) - MIN_ZOOM).abs() < f32::EPSILON);
    }

    #[test]
    fn middle_drag_pans_at_half_speed() {
        // 指针下移 100px：内容跟着往下 50px，即「滚顶」减少 50px。
        assert!((pan_delta(100.0) + 50.0).abs() < 0.0001);
        // 指针上移：内容往上，滚顶增加。
        assert!((pan_delta(-100.0) - 50.0).abs() < 0.0001);
        assert!(pan_delta(0.0).abs() < f32::EPSILON);
        assert!(pan_delta(f32::NAN).abs() < f32::EPSILON);
    }

    #[test]
    fn middle_drag_stops_at_the_document_edges() {
        let layout = DocumentLayout::new(
            &vec![
                PageSize {
                    width: 600.0,
                    height: 800.0,
                };
                3
            ],
            1.0,
            1000.0,
        );
        let top = 100.0;

        // 往上拖过头停在底部，不会滚出内容范围。
        let bottom = layout.clamp_scroll_top(top + pan_delta(-100_000.0), 400.0);
        assert!((bottom - (layout.content_height - 400.0)).abs() < 0.001);
        // 往下拖过头停在顶部。
        let top_again = layout.clamp_scroll_top(top + pan_delta(100_000.0), 400.0);
        assert!(top_again.abs() < 0.001);
    }

    #[test]
    fn anchor_scroll_is_clamped_to_content() {
        let pages = [PageSize {
            width: 600.0,
            height: 800.0,
        }];
        let layout = DocumentLayout::new(&pages, 1.0, 800.0);
        let (left, top) = layout
            .scroll_for_ratios(0, 1.0, 1.0, 0.0, 0.0, 800.0, 600.0)
            .unwrap();
        assert!(left >= 0.0);
        assert!(top >= 0.0);
        assert!(top <= layout.content_height);
    }
}
