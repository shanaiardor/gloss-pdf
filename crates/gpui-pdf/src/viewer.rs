//! 阅读器状态：当前文档、当前页、缩放、滚动、页面缓存，以及选择/搜索/高亮。

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{RenderImage, ScrollHandle};
use pdf_core::{CharBox, LinkBox, OutlineItem, PageSize};

use crate::highlights::HighlightStore;
use crate::layout::{FitArea, FitMode, MAX_ZOOM, MIN_ZOOM, zoom_for};
use crate::progress::ReadingProgress;
use crate::search::SearchState;
use crate::selection::Selection;

pub struct Document {
    pub path: PathBuf,
    pub pages: Vec<PageSize>,
    pub outline: Vec<OutlineItem>,
}

/// 当前页已渲染好的位图与元素数据。
pub struct PageData {
    pub image: Arc<RenderImage>,
    pub chars: Vec<CharBox>,
    pub links: Vec<LinkBox>,
}

pub struct Viewer {
    pub doc: Option<Document>,
    pub current_page: usize,
    pub zoom: f32,
    /// 当前缩放方式：手动缩放后窗口变化不再自动重算。
    pub fit_mode: FitMode,
    pub scroll: ScrollHandle,
    pub page: Option<PageData>,
    pub selection: Selection,
    pub search: SearchState,
    pub highlights: HighlightStore,
    /// 从上次阅读进度恢复的页内滚动比例，等 AppState 用实际视口换算成偏移后应用。
    pending_scroll: Option<(f32, f32)>,
}

impl Default for Viewer {
    fn default() -> Self {
        Self::new()
    }
}

impl Viewer {
    pub fn new() -> Self {
        Self {
            doc: None,
            current_page: 0,
            zoom: 1.0,
            fit_mode: FitMode::Free,
            scroll: ScrollHandle::new(),
            page: None,
            selection: Selection::default(),
            search: SearchState::default(),
            highlights: HighlightStore::default(),
            pending_scroll: None,
        }
    }

    pub fn has_document(&self) -> bool {
        self.doc.is_some()
    }

    /// 回到首页：清空当前文档与所有交互状态。
    pub fn close(&mut self) {
        *self = Self::new();
    }

    pub fn page_count(&self) -> usize {
        self.doc.as_ref().map_or(0, |doc| doc.pages.len())
    }

    pub fn page_size(&self) -> Option<PageSize> {
        let doc = self.doc.as_ref()?;
        doc.pages.get(self.current_page).copied()
    }

    pub fn chars(&self) -> &[CharBox] {
        self.page
            .as_ref()
            .map(|page| page.chars.as_slice())
            .unwrap_or_default()
    }

    /// 按模式计算缩放；返回缩放或模式是否变了。
    ///
    /// 不会清掉已渲染的页面位图：窗口拖动缩放时先拉伸旧图，等新位图回来再替换，
    /// 避免每一帧都闪一下「正在渲染」。
    pub fn fit(&mut self, mode: FitMode, area: FitArea) -> bool {
        let Some(size) = self.page_size() else {
            return false;
        };
        let Some(zoom) = zoom_for(size, area, mode) else {
            return false;
        };

        let zoom_changed = (self.zoom - zoom).abs() > 0.0001;
        let changed = zoom_changed || self.fit_mode != mode;
        self.zoom = zoom;
        self.fit_mode = mode;
        if zoom_changed {
            self.scroll = ScrollHandle::new();
        }
        changed
    }

    /// 切换文档：清空缓存与交互状态，按整页适应打开。
    pub fn reset(&mut self, doc: Document, area: FitArea) {
        self.doc = Some(doc);
        self.current_page = 0;
        self.page = None;
        self.scroll = ScrollHandle::new();
        self.selection.clear();
        self.search.clear();
        self.highlights.clear();
        self.pending_scroll = None;
        self.fit_mode = FitMode::Page;
        self.fit(FitMode::Page, area);
    }

    /// 恢复上次阅读位置：页码、缩放（或适宽/适页模式）和页内滚动比例。
    ///
    /// 总页数对不上说明文件已经换过，宁可从头开始，也不跳到错误的位置。
    /// 返回 `true` 表示页码/缩放已经恢复，滚动比例留在 `take_pending_scroll`。
    pub fn restore_progress(&mut self, progress: &ReadingProgress, area: FitArea) -> bool {
        let Some(doc) = self.doc.as_ref() else {
            return false;
        };
        if progress.pages == 0
            || progress.pages != doc.pages.len()
            || progress.page >= doc.pages.len()
        {
            return false;
        }

        self.current_page = progress.page;
        self.page = None;
        self.scroll = ScrollHandle::new();
        self.selection.clear();

        match progress.fit {
            // 上次就是适宽/适页：按当前窗口重新算，而不是复用当时的百分比。
            Some(mode) if mode != FitMode::Free => {
                self.fit_mode = mode;
                self.fit(mode, area);
            }
            // 手动缩放：沿用上次的百分比。
            Some(_) if progress.zoom.is_finite() && progress.zoom > 0.0 => {
                self.zoom = progress.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
                self.fit_mode = FitMode::Free;
            }
            // 旧版本记录没有 fit 字段，或保存的缩放无效：按新的默认整页打开。
            _ => {
                self.fit_mode = FitMode::Page;
                self.fit(FitMode::Page, area);
            }
        }

        self.pending_scroll = Some((
            clamp_ratio(progress.scroll_x),
            clamp_ratio(progress.scroll_y),
        ));
        true
    }

    /// 取走待应用的页内滚动比例（只应应用一次）。
    pub fn take_pending_scroll(&mut self) -> Option<(f32, f32)> {
        self.pending_scroll.take()
    }

    /// 当前页内滚动比例：0 是顶部/最左，1 是底部/最右。
    pub fn scroll_ratios(&self) -> (f32, f32) {
        let max = self.scroll.max_offset();
        let offset = self.scroll.offset();
        (
            axis_ratio(f32::from(offset.x), f32::from(max.x)),
            axis_ratio(f32::from(offset.y), f32::from(max.y)),
        )
    }

    /// 切页；越界返回 false。
    pub fn set_page(&mut self, page: usize) -> bool {
        if page >= self.page_count() {
            return false;
        }
        self.current_page = page;
        self.page = None;
        self.scroll = ScrollHandle::new();
        self.selection.clear();
        self.pending_scroll = None;
        true
    }

    /// 手动缩放；之后窗口尺寸变化不再自动重算缩放。
    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.fit_mode = FitMode::Free;
        self.page = None;
        self.scroll = ScrollHandle::new();
        self.pending_scroll = None;
    }
}

fn clamp_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn axis_ratio(offset: f32, max: f32) -> f32 {
    if !max.is_finite() || max <= 0.0 || offset >= 0.0 {
        return 0.0;
    }
    let ratio = (-offset / max).clamp(0.0, 1.0);
    // 避免把 -0.0 写进 JSON。
    if ratio == 0.0 { 0.0 } else { ratio }
}

/// 页内滚动比例 → 滚动偏移。GPUI 的偏移向下/向右为负。
pub fn scroll_offset_for_ratio(ratio: f32, content: f32, viewport: f32) -> f32 {
    let max = (content - viewport).max(0.0);
    -clamp_ratio(ratio) * max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(page_count: usize) -> Document {
        Document {
            path: "C:/docs/a.pdf".into(),
            pages: vec![
                PageSize {
                    width: 600.0,
                    height: 800.0,
                };
                page_count
            ],
            outline: Vec::new(),
        }
    }

    fn area() -> FitArea {
        FitArea {
            width: 500.0,
            height: 600.0,
        }
    }

    fn viewer_with_pages(page_count: usize) -> Viewer {
        let mut viewer = Viewer::new();
        viewer.reset(document(page_count), area());
        viewer
    }

    #[test]
    fn reset_opens_in_fit_page_mode() {
        let viewer = viewer_with_pages(3);
        assert_eq!(viewer.fit_mode, FitMode::Page);
        // 600x800 的页面放进 500x600：高度先到极限。
        assert!((viewer.zoom - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn fit_page_uses_height_and_fit_width_uses_width() {
        let mut viewer = viewer_with_pages(1);
        assert!(viewer.fit(FitMode::Width, area()));
        assert!((viewer.zoom - 500.0 / 600.0).abs() < 0.0001);
        assert_eq!(viewer.fit_mode, FitMode::Width);

        assert!(viewer.fit(FitMode::Page, area()));
        assert!((viewer.zoom - 0.75).abs() < f32::EPSILON);
        assert_eq!(viewer.fit_mode, FitMode::Page);
    }

    #[test]
    fn switching_fit_mode_reports_change_even_when_zoom_is_equal() {
        let mut doc = document(1);
        doc.pages[0] = PageSize {
            width: 600.0,
            height: 600.0,
        };
        let mut viewer = Viewer::new();
        viewer.reset(doc, area());

        let zoom = viewer.zoom;
        assert_eq!(viewer.fit_mode, FitMode::Page);
        assert!(viewer.fit(FitMode::Width, area()));
        assert_eq!(viewer.fit_mode, FitMode::Width);
        assert!((viewer.zoom - zoom).abs() < f32::EPSILON);
    }

    #[test]
    fn manual_zoom_leaves_fit_mode() {
        let mut viewer = viewer_with_pages(1);
        viewer.set_zoom(1.5);
        assert_eq!(viewer.fit_mode, FitMode::Free);
        assert!((viewer.zoom - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn restore_applies_page_zoom_and_pending_scroll() {
        let mut viewer = viewer_with_pages(5);
        let progress = ReadingProgress {
            page: 3,
            pages: 5,
            zoom: 1.25,
            fit: Some(FitMode::Free),
            scroll_x: 0.25,
            scroll_y: 0.6,
            updated_at: 10,
        };

        assert!(viewer.restore_progress(&progress, area()));
        assert_eq!(viewer.current_page, 3);
        assert!((viewer.zoom - 1.25).abs() < f32::EPSILON);
        assert_eq!(viewer.fit_mode, FitMode::Free);
        assert_eq!(viewer.take_pending_scroll(), Some((0.25, 0.6)));
        assert_eq!(viewer.take_pending_scroll(), None);
    }

    #[test]
    fn restore_fit_mode_recomputes_zoom_for_current_viewport() {
        let mut viewer = viewer_with_pages(2);
        // 保存的 zoom 是旧窗口的，恢复时应按当前阅读区重新算。
        let progress = ReadingProgress {
            page: 1,
            pages: 2,
            zoom: 0.2,
            fit: Some(FitMode::Page),
            updated_at: 10,
            ..Default::default()
        };

        assert!(viewer.restore_progress(&progress, area()));
        assert_eq!(viewer.fit_mode, FitMode::Page);
        assert!((viewer.zoom - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn legacy_progress_without_fit_opens_fit_page() {
        let mut viewer = viewer_with_pages(3);
        let legacy = ReadingProgress {
            page: 1,
            pages: 3,
            zoom: 1.5,
            fit: None,
            updated_at: 10,
            ..Default::default()
        };

        assert!(viewer.restore_progress(&legacy, area()));
        assert_eq!(viewer.current_page, 1);
        assert_eq!(viewer.fit_mode, FitMode::Page);
        // 旧百分比不再沿用，按当前阅读区整页适应。
        assert!((viewer.zoom - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn restore_rejects_changed_page_count() {
        let mut viewer = viewer_with_pages(5);
        let stale = ReadingProgress {
            page: 3,
            pages: 4,
            zoom: 1.5,
            scroll_y: 0.8,
            ..Default::default()
        };

        assert!(!viewer.restore_progress(&stale, area()));
        assert_eq!(viewer.current_page, 0);
        assert_eq!(viewer.take_pending_scroll(), None);
    }

    #[test]
    fn restore_clamps_invalid_zoom_and_ratios() {
        let mut viewer = viewer_with_pages(3);
        let progress = ReadingProgress {
            page: 0,
            pages: 3,
            zoom: 99.0,
            fit: Some(FitMode::Free),
            scroll_x: -2.0,
            scroll_y: 4.0,
            updated_at: 0,
        };

        assert!(viewer.restore_progress(&progress, area()));
        assert!((viewer.zoom - MAX_ZOOM).abs() < f32::EPSILON);
        assert_eq!(viewer.take_pending_scroll(), Some((0.0, 1.0)));
    }

    #[test]
    fn scroll_offset_for_ratio_handles_short_content() {
        // 内容比视口短时不能产生负向滚动。
        assert_eq!(scroll_offset_for_ratio(0.5, 300.0, 500.0), 0.0);
        // 一半的滚动范围，偏移为负。
        assert_eq!(scroll_offset_for_ratio(0.5, 1000.0, 400.0), -300.0);
        // 越界的比例被夹住。
        assert_eq!(scroll_offset_for_ratio(9.0, 1000.0, 400.0), -600.0);
        assert_eq!(scroll_offset_for_ratio(f32::NAN, 1000.0, 400.0), 0.0);
    }
}
