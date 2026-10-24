//! 阅读器状态：当前文档、多页位图缓存、当前页、缩放与滚动，以及选择/搜索/高亮。
//!
//! 连续滚动下滚动偏移是「文档级」的，页内位置只在保存/恢复阅读进度时用得到；
//! 这里只维护状态与缓存，偏移和当前页的换算由 `app.rs` 结合 `DocumentLayout` 完成。

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{RenderImage, ScrollHandle};
use pdf_core::{CharBox, DocumentMetadata, LinkBox, OutlineItem, PageSize};

use crate::highlights::HighlightStore;
use crate::layout::{FitArea, FitMode, MAX_ZOOM, MIN_ZOOM, zoom_for};
use crate::progress::ReadingProgress;
use crate::search::SearchState;
use crate::selection::Selection;
use crate::tiles::TILE_SIZE;

/// 位图缓存上限：400% 缩放时一张 A4 位图约 30 MB，不能把整篇都留在内存里。
pub const MAX_CACHED_PAGES: usize = 12;

/// 瓦片位图缓存的默认字节预算。
pub const DEFAULT_CACHE_BUDGET: usize = 256 * 1024 * 1024;

/// 一个瓦片在缓存里的唯一标识：(页号, 整页像素坐标原点)。
///
/// 与引擎的 [`pdf_core::TileId`] 是同一个类型，每帧的可视快照直接用它发布，
/// 不用做任何换算。
pub type TileKey = pdf_core::TileId;

pub struct Document {
    pub path: PathBuf,
    pub pages: Vec<PageSize>,
    pub outline: Vec<OutlineItem>,
    pub metadata: DocumentMetadata,
}

/// 一个已渲染的页面瓦片；坐标是整页像素坐标。
pub struct TileData {
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: u32,
    pub height: u32,
    pub image: Arc<RenderImage>,
}

/// 某一页已渲染好的瓦片与元素数据。
pub struct PageData {
    pub tiles: Vec<TileData>,
    pub chars: Vec<CharBox>,
    pub links: Vec<LinkBox>,
    /// 渲染这些瓦片时用的缩放（已乘 DPI）；缩放变了旧瓦片会被新瓦片替换。
    pub scale: f32,
    /// 文字/链接覆盖层是否已提取；它和缩放无关，每页只取一次。
    pub elements_loaded: bool,
}

impl PageData {
    /// 这一页所有瓦片位图占用的字节数（BGRA，每像素 4 字节）。
    pub fn bitmap_bytes(&self) -> usize {
        self.tiles
            .iter()
            .map(|tile| tile.width as usize * tile.height as usize * 4)
            .sum()
    }

    fn empty(scale: f32) -> Self {
        Self {
            tiles: Vec::new(),
            chars: Vec::new(),
            links: Vec::new(),
            scale,
            elements_loaded: false,
        }
    }
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
    /// 瓦片位图缓存的字节预算；测试可以调小。
    cache_budget: usize,
    /// 已发出、还没回来的瓦片请求：((页, 原点 x, 原点 y), (代号, 缩放))。
    tile_requests: HashMap<(usize, i32, i32), (u64, f32)>,
    /// 已发出、还没回来的页面元素请求。
    element_requests: HashMap<usize, u64>,
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
            cache_budget: DEFAULT_CACHE_BUDGET,
            tile_requests: HashMap::new(),
            element_requests: HashMap::new(),
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

    /// 标记「这个瓦片已经按这个缩发放了渲染请求」；已在跑或已有同缩放瓦片时返回 `false`。
    ///
    /// 连续滚动会在每一帧检查可视瓦片，靠这个去重，避免同一个瓦片被反复排队。
    pub fn begin_tile_request(
        &mut self,
        page: usize,
        origin_x: i32,
        origin_y: i32,
        request: u64,
        scale: f32,
    ) -> bool {
        let key = (page, origin_x, origin_y);
        if let Some(&(_, pending_scale)) = self.tile_requests.get(&key)
            && (pending_scale - scale).abs() < 0.005
        {
            return false;
        }
        if self.pages.get(&page).is_some_and(|data| {
            (data.scale - scale).abs() < 0.005
                && data
                    .tiles
                    .iter()
                    .any(|tile| tile.origin_x == origin_x && tile.origin_y == origin_y)
        }) {
            return false;
        }
        self.tile_requests.insert(key, (request, scale));
        true
    }

    /// 认领一个瓦片渲染结果；代号过期（缩放已变、请求已作废）时返回 `None`。
    pub fn take_tile_request(
        &mut self,
        page: usize,
        origin_x: i32,
        origin_y: i32,
        request: u64,
    ) -> Option<f32> {
        let key = (page, origin_x, origin_y);
        match self.tile_requests.get(&key) {
            Some(&(pending, scale)) if pending == request => {
                self.tile_requests.remove(&key);
                Some(scale)
            }
            _ => None,
        }
    }

    /// 存一个瓦片。缩放变了就丢掉旧瓦片，但保留已提取的文字/链接。
    pub fn store_tile(
        &mut self,
        page: usize,
        scale: f32,
        origin_x: i32,
        origin_y: i32,
        width: u32,
        height: u32,
        image: Arc<RenderImage>,
    ) {
        let data = self
            .pages
            .entry(page)
            .or_insert_with(|| PageData::empty(scale));
        if (data.scale - scale).abs() >= 0.005 {
            data.tiles.clear();
            data.scale = scale;
        }
        if let Some(tile) = data
            .tiles
            .iter_mut()
            .find(|tile| tile.origin_x == origin_x && tile.origin_y == origin_y)
        {
            tile.width = width;
            tile.height = height;
            tile.image = image;
            return;
        }
        data.tiles.push(TileData {
            origin_x,
            origin_y,
            width,
            height,
            image,
        });
    }

    /// 标记要提取某一页的文字/链接；已提取或已在跑时返回 `false`。
    pub fn begin_element_request(&mut self, page: usize, request: u64) -> bool {
        if self
            .pages
            .get(&page)
            .is_some_and(|data| data.elements_loaded)
        {
            return false;
        }
        if self.element_requests.contains_key(&page) {
            return false;
        }
        self.element_requests.insert(page, request);
        true
    }

    pub fn take_element_request(&mut self, page: usize, request: u64) -> bool {
        match self.element_requests.get(&page) {
            Some(&pending) if pending == request => {
                self.element_requests.remove(&page);
                true
            }
            _ => false,
        }
    }

    /// 存文字/链接覆盖层。元素到达可能早于位图，此时先占一个空页数据。
    pub fn store_elements(&mut self, page: usize, chars: Vec<CharBox>, links: Vec<LinkBox>) {
        let data = self
            .pages
            .entry(page)
            .or_insert_with(|| PageData::empty(0.0));
        data.chars = chars;
        data.links = links;
        data.elements_loaded = true;
    }

    /// 测试或外部恢复时直接放入整页数据。
    #[cfg(test)]
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

    /// 当前缓存的瓦片位图字节数。
    pub fn cached_bytes(&self) -> usize {
        self.pages.values().map(PageData::bitmap_bytes).sum()
    }

    #[cfg(test)]
    pub fn cache_budget(&self) -> usize {
        self.cache_budget
    }

    #[cfg(test)]
    pub fn set_cache_budget(&mut self, bytes: usize) {
        self.cache_budget = bytes.max(1);
    }

    /// 按字节预算和页数上限淘汰缓存，优先丢离可视范围最远的瓦片。
    ///
    /// `visible` 是当前与视口相交的瓦片；它们不会被淘汰。若预算小到连可见
    /// 瓦片都放不下，允许暂时超出预算，避免阅读时出现空白。
    pub fn evict(&mut self, keep: Range<usize>, visible: &HashSet<TileKey>) {
        let visible_rects = visible_page_rects(visible);
        loop {
            let over_pages = self.pages.len() > MAX_CACHED_PAGES;
            let over_bytes = self.cached_bytes() > self.cache_budget;
            if !over_pages && !over_bytes {
                break;
            }

            // 先挑单个瓦片：页号越远越优先，同页内离可见区域越远越优先。
            let mut farthest_tile: Option<((usize, u64), TileKey)> = None;
            for (page, data) in &self.pages {
                for tile in &data.tiles {
                    let key = (*page, tile.origin_x, tile.origin_y);
                    if visible.contains(&key) {
                        continue;
                    }
                    let distance = tile_distance(key, &keep, &visible_rects);
                    if farthest_tile.is_none_or(|(best, _)| distance > best) {
                        farthest_tile = Some((distance, key));
                    }
                }
            }

            if let Some((_, (page, origin_x, origin_y))) = farthest_tile {
                if let Some(data) = self.pages.get_mut(&page) {
                    data.tiles
                        .retain(|tile| tile.origin_x != origin_x || tile.origin_y != origin_y);
                }
                if self
                    .pages
                    .get(&page)
                    .is_some_and(|data| data.tiles.is_empty() && !keep.contains(&page))
                {
                    self.pages.remove(&page);
                }
                continue;
            }

            // 没有可淘汰的瓦片时，页数上限仍可淘汰整页（例如只存了元素数据）。
            if over_pages {
                let farthest_page = self
                    .pages
                    .keys()
                    .filter(|page| !keep.contains(page))
                    .max_by_key(|page| distance_outside(**page, &keep))
                    .copied();
                if let Some(page) = farthest_page {
                    self.pages.remove(&page);
                    continue;
                }
            }

            break;
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
        self.tile_requests.clear();
        self.element_requests.clear();
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
        self.tile_requests.clear();
        self.element_requests.clear();
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

/// 当前可见瓦片在每页上覆盖的矩形（整页像素坐标）。
fn visible_page_rects(visible: &HashSet<TileKey>) -> HashMap<usize, (i32, i32, i32, i32)> {
    let mut rects: HashMap<usize, (i32, i32, i32, i32)> = HashMap::new();
    for &(page, origin_x, origin_y) in visible {
        let rect = rects.entry(page).or_insert((
            origin_x,
            origin_y,
            origin_x + TILE_SIZE as i32,
            origin_y + TILE_SIZE as i32,
        ));
        rect.0 = rect.0.min(origin_x);
        rect.1 = rect.1.min(origin_y);
        rect.2 = rect.2.max(origin_x + TILE_SIZE as i32);
        rect.3 = rect.3.max(origin_y + TILE_SIZE as i32);
    }
    rects
}

/// 淘汰优先级：先按页号距离，再按瓦片到可见区域的平方距离；越大越先淘汰。
fn tile_distance(
    key: TileKey,
    keep: &Range<usize>,
    visible_rects: &HashMap<usize, (i32, i32, i32, i32)>,
) -> (usize, u64) {
    let (page, origin_x, origin_y) = key;
    let page_distance = distance_outside(page, keep);
    let tile_left = origin_x;
    let tile_top = origin_y;
    let tile_right = origin_x + TILE_SIZE as i32;
    let tile_bottom = origin_y + TILE_SIZE as i32;

    let (dx, dy) = match visible_rects.get(&page) {
        Some(&(left, top, right, bottom)) => {
            let dx = if tile_right <= left {
                left - tile_right
            } else if tile_left >= right {
                tile_left - right
            } else {
                0
            };
            let dy = if tile_bottom <= top {
                top - tile_bottom
            } else if tile_top >= bottom {
                tile_top - bottom
            } else {
                0
            };
            (dx as i64, dy as i64)
        }
        // 这一页没有可见瓦片（纯预取页），页内顺序不重要。
        None => (0, 0),
    };

    (page_distance, (dx * dx + dy * dy) as u64)
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
            metadata: DocumentMetadata::default(),
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
        PageData::empty(scale)
    }

    fn tile_image() -> Arc<RenderImage> {
        Arc::new(RenderImage::new([image::Frame::new(
            image::RgbaImage::new(1, 1),
        )]))
    }

    fn store_test_tile(viewer: &mut Viewer, page: usize, origin_x: i32, origin_y: i32) {
        viewer.store_tile(
            page,
            1.0,
            origin_x,
            origin_y,
            TILE_SIZE,
            TILE_SIZE,
            tile_image(),
        );
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
    fn duplicate_tile_requests_are_coalesced() {
        let mut viewer = viewer_with_pages(3);
        assert!(viewer.begin_tile_request(1, 0, 0, 7, 1.0));
        // 同一个瓦片同一缩放还在跑：不再排队。
        assert!(!viewer.begin_tile_request(1, 0, 0, 8, 1.0));
        // 缩放变了要重发。
        assert!(viewer.begin_tile_request(1, 0, 0, 9, 2.0));
        // 另一个瓦片独立排队。
        assert!(viewer.begin_tile_request(1, 512, 0, 10, 2.0));

        // 过期的代号认领不到。
        assert_eq!(viewer.take_tile_request(1, 0, 0, 7), None);
        assert_eq!(viewer.take_tile_request(1, 0, 0, 9), Some(2.0));
        assert_eq!(viewer.take_tile_request(1, 512, 0, 10), Some(2.0));
        // 认领之后同一缩放可以再请求（比如结果被淘汰了）。
        assert!(viewer.begin_tile_request(1, 0, 0, 11, 2.0));
    }

    #[test]
    fn new_scale_replaces_tiles_but_keeps_elements() {
        let mut viewer = viewer_with_pages(2);
        viewer.store_elements(0, Vec::new(), Vec::new());
        let image = Arc::new(RenderImage::new([image::Frame::new(
            image::RgbaImage::new(1, 1),
        )]));
        viewer.store_tile(0, 1.0, 0, 0, 1, 1, image.clone());
        viewer.store_tile(0, 1.0, 512, 0, 1, 1, image.clone());
        assert_eq!(viewer.page(0).unwrap().tiles.len(), 2);

        viewer.store_tile(0, 2.0, 0, 0, 1, 1, image);
        let page = viewer.page(0).unwrap();
        assert_eq!(page.tiles.len(), 1);
        assert!(page.elements_loaded);
        assert_eq!(page.scale, 2.0);
    }

    #[test]
    fn element_requests_are_sent_once_per_page() {
        let mut viewer = viewer_with_pages(2);
        assert!(viewer.begin_element_request(1, 5));
        assert!(!viewer.begin_element_request(1, 6));
        assert!(!viewer.take_element_request(1, 4));
        assert!(viewer.take_element_request(1, 5));

        viewer.store_elements(1, Vec::new(), Vec::new());
        assert!(!viewer.begin_element_request(1, 7));
    }

    #[test]
    fn stored_page_blocks_same_scale_request() {
        let mut viewer = viewer_with_pages(2);
        viewer.store_page(0, page_data(1.0));
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
        viewer.evict(0..3, &HashSet::new());
        assert_eq!(viewer.cached_pages().len(), MAX_CACHED_PAGES);
        assert!(viewer.page(0).is_some());
        assert!(viewer.page(2).is_some());
        assert!(viewer.page(MAX_CACHED_PAGES + 3).is_none());
    }

    #[test]
    fn cache_budget_evicts_farthest_tiles_first() {
        let mut viewer = viewer_with_pages(3);
        let tile_bytes = TILE_SIZE as usize * TILE_SIZE as usize * 4;
        viewer.set_cache_budget(tile_bytes * 2);

        // 第 0 页的 (0,0) 可见，同页 (0,512) 紧邻可见区域，(0,1024) 更远。
        store_test_tile(&mut viewer, 0, 0, 0);
        store_test_tile(&mut viewer, 0, 0, TILE_SIZE as i32);
        store_test_tile(&mut viewer, 0, 0, TILE_SIZE as i32 * 2);
        // 第 1 页在可视范围外，应当最先淘汰，即使它在页内离原点更近。
        store_test_tile(&mut viewer, 1, 0, 0);

        let visible = HashSet::from([(0usize, 0i32, 0i32)]);
        viewer.evict(0..1, &visible);

        assert_eq!(viewer.cached_bytes(), tile_bytes * 2);
        let tiles = &viewer.page(0).expect("可见页应保留").tiles;
        let origins: Vec<_> = tiles.iter().map(|tile| tile.origin_y).collect();
        assert_eq!(origins, vec![0, TILE_SIZE as i32]);
        assert!(viewer.page(1).is_none());
    }

    #[test]
    fn visible_tiles_survive_a_budget_smaller_than_one_tile() {
        let mut viewer = viewer_with_pages(3);
        viewer.set_cache_budget(1);
        store_test_tile(&mut viewer, 0, 0, 0);
        store_test_tile(&mut viewer, 1, 0, 0);
        store_test_tile(&mut viewer, 2, 0, 0);

        let visible = HashSet::from([(0usize, 0i32, 0i32)]);
        viewer.evict(0..1, &visible);

        // 宁可暂时超预算，也不能把正在看的瓦片丢掉。
        assert!(viewer.cached_bytes() > viewer.cache_budget());
        assert_eq!(viewer.page(0).expect("可见瓦片应保留").tiles.len(), 1);
        assert!(viewer.page(1).is_none());
        assert!(viewer.page(2).is_none());
    }

    #[test]
    fn browsing_at_high_zoom_stays_within_the_byte_budget() {
        let mut viewer = viewer_with_pages(80);
        let tile_bytes = TILE_SIZE as usize * TILE_SIZE as usize * 4;
        let budget = tile_bytes * 8;
        viewer.set_cache_budget(budget);

        // 模拟 400% 下一页 5 列可见瓦片、再预取 7 行，连续浏览 40 页。
        for page in 0..40 {
            for column in 0..5 {
                store_test_tile(&mut viewer, page, column * TILE_SIZE as i32, 0);
            }
            for row in 1..8 {
                store_test_tile(&mut viewer, page, 0, row * TILE_SIZE as i32);
            }
            let visible: HashSet<_> = (0..5)
                .map(|column| (page, column * TILE_SIZE as i32, 0))
                .collect();
            viewer.evict(page..page + 1, &visible);
            assert!(viewer.cached_bytes() <= budget, "第 {page} 页后超预算");
        }

        // 40 页若全留会远超预算；预算淘汰后只保留当前页附近的少量瓦片。
        assert!(viewer.cached_bytes() <= budget);
        assert!(viewer.page(39).is_some());
        assert!(viewer.cached_pages().len() <= 2);
    }
}
