//! 阅读器状态：当前文档、多页位图缓存、当前页、缩放与滚动，以及选择/搜索/高亮。
//!
//! 连续滚动下滚动偏移是「文档级」的，页内位置只在保存/恢复阅读进度时用得到；
//! 这里只维护状态与缓存，偏移和当前页的换算由 `app.rs` 结合 `DocumentLayout` 完成。

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{RenderImage, ScrollHandle};
use pdf_core::{CharBox, LinkBox, OutlineItem, PageSize};

use crate::highlights::HighlightStore;
use crate::layout::{FitArea, FitMode, MAX_ZOOM, MIN_ZOOM, zoom_for};
use crate::progress::ReadingProgress;
use crate::search::SearchState;
use crate::selection::Selection;

/// 位图缓存上限：400% 缩放时一张 A4 位图约 30 MB，不能把整篇都留在内存里。
pub const MAX_CACHED_PAGES: usize = 12;

pub struct Document {
    pub path: PathBuf,
    pub pages: Vec<PageSize>,
    pub outline: Vec<OutlineItem>,
}

/// 某一页已渲染好的位图与元素数据。
pub struct PageData {
    pub image: Arc<RenderImage>,
    pub chars: Vec<CharBox>,
    pub links: Vec<LinkBox>,
    /// 渲染这张位图时用的缩放（已乘 DPI）；缩放变了要重新渲染。
    pub scale: f32,
}

pub struct Viewer {
    pub doc: Option<Document>,
    /// 视口锚线所在的页，滚动时由 `AppState` 更新。
    pub current_page: usize,
    pub zoom: f32,
    /// 当前缩放方式：手动缩放后窗口变化不再自动重算。
    pub fit_mode: FitMode,
    /// 整篇文档共用的滚动句柄，偏移是文档级而不是页内。
    pub scroll: ScrollHandle,
    /// 已渲染的页，按页索引缓存；离当前页太远的会被淘汰。
    pages: HashMap<usize, PageData>,
    /// 已发出、还没回来的渲染请求：(代号, 缩放)，用来丢掉过期位图。
    requests: HashMap<usize, (u64, f32)>,
    pub selection: Selection,
    pub search: SearchState,
    pub highlights: HighlightStore,
    /// 从阅读进度恢复的位置：(页, 横向比例, 页内纵向比例)。
    pending_scroll: Option<(usize, f32, f32)>,
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
            pages: HashMap::new(),
            requests: HashMap::new(),
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

    /// 当前页的纸张尺寸。
    pub fn page_size(&self) -> Option<PageSize> {
        self.page_size_at(self.current_page)
    }

    pub fn page_size_at(&self, page: usize) -> Option<PageSize> {
        self.doc.as_ref()?.pages.get(page).copied()
    }

    /// 已渲染好的某一页；没渲染完时返回 `None`。
    pub fn page(&self, page: usize) -> Option<&PageData> {
        self.pages.get(&page)
    }

    pub fn chars_of(&self, page: usize) -> &[CharBox] {
        self.page(page)
            .map(|data| data.chars.as_slice())
            .unwrap_or_default()
    }

    /// 已经缓存了位图的页，切换查询时重算匹配用。
    pub fn cached_pages(&self) -> Vec<usize> {
        let mut pages: Vec<usize> = self.pages.keys().copied().collect();
        pages.sort_unstable();
        pages
    }

    // ---------- 渲染调度 ----------

    /// 标记「这一页已经按这个缩发放了渲染请求」；已在跑或已有同位图时返回 `false`。
    ///
    /// 连续滚动会在每一帧检查可视页，靠这个去重，避免同一页被反复排队。
    pub fn begin_request(&mut self, page: usize, request: u64, scale: f32) -> bool {
        if let Some(&(_, pending_scale)) = self.requests.get(&page)
            && (pending_scale - scale).abs() < 0.005
        {
            return false;
        }
        if self
            .pages
            .get(&page)
            .is_some_and(|data| (data.scale - scale).abs() < 0.005)
        {
            return false;
        }
        self.requests.insert(page, (request, scale));
        true
    }

    /// 认领一个渲染结果；代号过期（缩放已变、请求已作废）时返回 `None`。
    ///
    /// 返回渲染时用的缩放，存进 `PageData` 供下次比对。
    pub fn take_request(&mut self, page: usize, request: u64) -> Option<f32> {
        match self.requests.get(&page) {
            Some(&(pending, scale)) if pending == request => {
                self.requests.remove(&page);
                Some(scale)
            }
            _ => None,
        }
    }

    pub fn store_page(&mut self, page: usize, data: PageData) {
        self.pages.insert(page, data);
    }

    /// 用某一页的字符重算搜索匹配（当前页更新 `matches`，其他页只更新逐页缓存）。
    ///
    /// 直接借用缓存里的字符，不复制整页文本；翻页和位图到达时都会调。
    pub fn refresh_search(&mut self, page: usize) {
        let Viewer {
            pages,
            search,
            current_page,
            ..
        } = self;
        let chars: &[CharBox] = pages
            .get(&page)
            .map(|data| data.chars.as_slice())
            .unwrap_or_default();
        if page == *current_page {
            search.recompute(chars, page);
        } else {
            search.cache_page(chars, page);
        }
    }

    /// 双击选词：选区归到 `page`，只在这一页里扩展。
    pub fn select_word_at(&mut self, page: usize, index: usize) {
        let Viewer {
            pages, selection, ..
        } = self;
        let Some(chars) = pages.get(&page).map(|data| data.chars.as_slice()) else {
            return;
        };
        selection.select_word(page, chars, index);
    }

    /// 淘汰离可视范围最远的页，把缓存压在 `MAX_CACHED_PAGES` 以内。
    pub fn evict(&mut self, keep: Range<usize>) {
        while self.pages.len() > MAX_CACHED_PAGES {
            let farthest = self
                .pages
                .keys()
                .filter(|page| !keep.contains(page))
                .max_by_key(|page| distance_outside(**page, &keep))
                .copied();
            match farthest {
                Some(page) => {
                    self.pages.remove(&page);
                }
                None => break,
            }
        }
    }

    // ---------- 缩放与文档 ----------

    /// 按模式计算缩放；返回缩放或模式是否变了。
    ///
    /// 不会清掉已渲染的页面位图：缩放过程中先拉伸旧图，等新位图回来再替换，
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
        changed
    }

    /// 切换文档：清空缓存与交互状态，按整页适应打开。
    pub fn reset(&mut self, doc: Document, area: FitArea) {
        self.doc = Some(doc);
        self.current_page = 0;
        self.pages.clear();
        self.requests.clear();
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
        self.pages.clear();
        self.requests.clear();
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
            progress.page,
            clamp_ratio(progress.scroll_x),
            clamp_ratio(progress.scroll_y),
        ));
        true
    }

    /// 取走待应用的阅读位置（只应应用一次）：(页, 横向比例, 页内纵向比例)。
    pub fn take_pending_scroll(&mut self) -> Option<(usize, f32, f32)> {
        self.pending_scroll.take()
    }

    /// 切页；越界返回 `false`。
    ///
    /// 只改页码和页内交互状态，滚动偏移要由 `AppState` 按布局换算后设置。
    pub fn set_page(&mut self, page: usize) -> bool {
        if page >= self.page_count() {
            return false;
        }
        self.current_page = page;
        self.selection.clear();
        self.pending_scroll = None;
        true
    }

    /// 手动缩放；之后窗口尺寸变化不再自动重算缩放。
    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.fit_mode = FitMode::Free;
        self.pending_scroll = None;
    }
}

/// 页号离可视范围有多少页，用来挑最远的缓存淘汰。
fn distance_outside(page: usize, keep: &Range<usize>) -> usize {
    if page < keep.start {
        keep.start - page
    } else if page >= keep.end {
        page + 1 - keep.end
    } else {
        0
    }
}

fn clamp_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    }
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

    fn page_data(scale: f32) -> PageData {
        let buffer = image::RgbaImage::new(1, 1);
        PageData {
            image: Arc::new(RenderImage::new([image::Frame::new(buffer)])),
            chars: Vec::new(),
            links: Vec::new(),
            scale,
        }
    }

    #[test]
    fn reset_opens_in_fit_page_mode() {
        let viewer = viewer_with_pages(3);
        assert_eq!(viewer.fit_mode, FitMode::Page);
        // 600x800 的页面放进 500x600：高度先到极限。
        assert!((viewer.zoom - 0.75).abs() < f32::EPSILON);
        assert_eq!(viewer.page_count(), 3);
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
        assert_eq!(viewer.take_pending_scroll(), Some((3, 0.25, 0.6)));
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
        assert_eq!(viewer.take_pending_scroll(), Some((0, 0.0, 1.0)));
    }

    #[test]
    fn set_page_rejects_out_of_range() {
        let mut viewer = viewer_with_pages(2);
        assert!(viewer.set_page(1));
        assert_eq!(viewer.current_page, 1);
        assert!(!viewer.set_page(2));
        assert_eq!(viewer.current_page, 1);
    }

    #[test]
    fn duplicate_requests_are_coalesced() {
        let mut viewer = viewer_with_pages(3);
        assert!(viewer.begin_request(1, 7, 1.0));
        // 同一页同一缩放还在跑：不再排队。
        assert!(!viewer.begin_request(1, 8, 1.0));
        // 缩放变了要重发。
        assert!(viewer.begin_request(1, 9, 2.0));

        // 过期的代号认领不到。
        assert_eq!(viewer.take_request(1, 7), None);
        assert_eq!(viewer.take_request(1, 9), Some(2.0));
        // 认领之后同一缩放可以再请求（比如结果被淘汰了）。
        assert!(viewer.begin_request(1, 10, 2.0));
    }

    #[test]
    fn stored_page_blocks_same_scale_request() {
        let mut viewer = viewer_with_pages(2);
        viewer.store_page(0, page_data(1.0));
        assert!(!viewer.begin_request(0, 1, 1.0));
        assert!(viewer.begin_request(0, 2, 1.5));
        assert_eq!(viewer.chars_of(0).len(), 0);
        assert_eq!(viewer.cached_pages(), vec![0]);
    }

    #[test]
    fn eviction_drops_farthest_pages_first() {
        let mut viewer = viewer_with_pages(40);
        for page in 0..MAX_CACHED_PAGES + 4 {
            viewer.store_page(page, page_data(1.0));
        }

        // 可视范围在 0..3，最远的应当被淘汰，近的留着。
        viewer.evict(0..3);
        assert_eq!(viewer.cached_pages().len(), MAX_CACHED_PAGES);
        assert!(viewer.page(0).is_some());
        assert!(viewer.page(2).is_some());
        assert!(viewer.page(MAX_CACHED_PAGES + 3).is_none());
    }
}
