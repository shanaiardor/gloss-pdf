//! 阅读器状态：当前文档、当前页、缩放、滚动、页面缓存，以及选择/搜索/高亮。

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{RenderImage, ScrollHandle};
use pdf_core::{CharBox, LinkBox, OutlineItem, PageSize};

use crate::highlights::HighlightStore;
use crate::search::SearchState;
use crate::selection::Selection;

pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 4.0;

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
    pub scroll: ScrollHandle,
    pub page: Option<PageData>,
    pub selection: Selection,
    pub search: SearchState,
    pub highlights: HighlightStore,
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
            scroll: ScrollHandle::new(),
            page: None,
            selection: Selection::default(),
            search: SearchState::default(),
            highlights: HighlightStore::default(),
        }
    }

    pub fn has_document(&self) -> bool {
        self.doc.is_some()
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

    /// 按可用宽度计算适宽缩放。
    pub fn fit_to_width(&mut self, available_width: f32) {
        let Some(size) = self.page_size() else {
            return;
        };
        if size.width <= 0.0 {
            return;
        }
        self.zoom = (available_width / size.width).clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// 切换文档：清空缓存与交互状态，并重置到第一页。
    pub fn reset(&mut self, doc: Document, available_width: f32) {
        self.doc = Some(doc);
        self.current_page = 0;
        self.page = None;
        self.scroll = ScrollHandle::new();
        self.selection.clear();
        self.search.clear();
        self.highlights.clear();
        self.fit_to_width(available_width);
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
        true
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.page = None;
        self.scroll = ScrollHandle::new();
    }
}
