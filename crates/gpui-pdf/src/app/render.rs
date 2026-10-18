//! 渲染调度：用 `DocumentLayout` 把缩放、视口与滚动偏移换算成「哪些页可见」，
//! 据此发位图请求、淘汰缓存、同步当前页与搜索高亮。
//!
//! 边界是「这一帧该有哪些位图」：输入是缩放、视口和滚动句柄，输出是
//! `Command::Render` 与 `Viewer` 里的缓存状态。这里不写盘、不改窗口，也不处理
//! 输入事件；阅读进度的持久化由 `document` 负责，窗口尺寸由 `window` 负责。

use std::sync::Arc;

use gpui::{Context, RenderImage, px};
use pdf_core::{CharBox, Command, LinkBox};

use crate::layout::{DocumentLayout, FitArea, FitMode, PREFETCH_PAGES};
use crate::viewer::PageData;

/// PageDown/PageUp 一次滚过视口高度的比例，留一点重叠方便接续阅读。
const PAGE_SCROLL_OVERLAP: f32 = 0.9;

impl super::AppState {
    // ---------- 滚动几何 ----------

    /// 用当前视口把恢复出来的页内比例换算成滚动偏移。
    pub(super) fn apply_pending_scroll(&mut self) {
        let Some((page, ratio_x, ratio_y)) = self.viewer.take_pending_scroll() else {
            return;
        };
        let Some(layout) = self.doc_layout() else {
            return;
        };
        self.scroll_to_anchor(&layout, page, ratio_y);

        // 横向：页面比视口宽时按比例还原，否则保持最左。
        let max_x = (layout.content_width - f32::from(self.viewport.width)).max(0.0);
        if max_x > 0.0 {
            let mut offset = self.viewer.scroll.offset();
            offset.x = px(-ratio_x.clamp(0.0, 1.0) * max_x);
            self.viewer.scroll.set_offset(offset);
        }
    }

    /// 当前缩放和视口下的整篇排版。
    ///
    /// 每页只算几下浮点，一帧调用两三次（同步页码、补渲染、画页面）不构成瓶颈；
    /// 不缓存是有意的：缩放、窗口宽度、文档任一变化都不必操心失效。
    pub(crate) fn doc_layout(&self) -> Option<DocumentLayout> {
        let doc = self.viewer.doc.as_ref()?;
        Some(DocumentLayout::new(
            &doc.pages,
            self.viewer.zoom,
            f32::from(self.viewport.width),
        ))
    }

    /// 滚动内容上边缘在内容坐标里的位置；0 表示贴顶。
    pub(crate) fn scroll_top(&self) -> f32 {
        -f32::from(self.viewer.scroll.offset().y)
    }

    /// 设置滚动顶部位置，夹在内容范围内。
    fn set_scroll_top(&self, layout: &DocumentLayout, top: f32) {
        let top = layout.clamp_scroll_top(top, f32::from(self.viewport.height));
        let mut offset = self.viewer.scroll.offset();
        offset.y = px(-top);
        self.viewer.scroll.set_offset(offset);
    }

    /// 阅读锚点：视口中线落在第几页、页内比例多少。
    ///
    /// 页码判定、进度保存、缩放前后对齐位置都用这一个锚点。
    fn reading_anchor(&self, layout: &DocumentLayout) -> (usize, f32) {
        layout.anchor_at(self.scroll_top(), f32::from(self.viewport.height))
    }

    /// 把锚点挪到「第 `page` 页的 `ratio` 处」。
    fn scroll_to_anchor(&self, layout: &DocumentLayout, page: usize, ratio: f32) {
        let top = layout.scroll_top_for_anchor(page, ratio, f32::from(self.viewport.height));
        self.set_scroll_top(layout, top);
    }

    /// 横向滚动比例：0 是最左，1 是最右。
    pub(super) fn scroll_ratio_x(&self, layout: &DocumentLayout) -> f32 {
        let max = (layout.content_width - f32::from(self.viewport.width)).max(0.0);
        if max <= 0.0 {
            return 0.0;
        }
        let ratio = (-f32::from(self.viewer.scroll.offset().x) / max).clamp(0.0, 1.0);
        if ratio == 0.0 { 0.0 } else { ratio }
    }

    /// 按视口高度滚动一屏（PageDown / PageUp / 空格）。
    pub(super) fn scroll_by_viewport(&mut self, direction: f32, cx: &mut Context<Self>) {
        let Some(layout) = self.doc_layout() else {
            return;
        };
        let step = f32::from(self.viewport.height) * PAGE_SCROLL_OVERLAP;
        self.set_scroll_top(&layout, self.scroll_top() + direction * step);
        self.ensure_visible_pages();
        self.sync_current_page();
        if self.capture_progress() {
            self.schedule_progress_flush(cx);
        }
        cx.notify();
    }

    // ---------- 位图调度 ----------

    /// 请求渲染一页；同一缩放已在跑或已有同位图时直接跳过。
    fn ensure_page(&mut self, page: usize) {
        if page >= self.viewer.page_count() {
            return;
        }
        let scale = (self.viewer.zoom * self.scale_factor).clamp(0.1, 8.0);
        let request = self.request_id + 1;
        if !self.viewer.begin_request(page, request, scale) {
            return;
        }
        self.request_id = request;
        if let Err(error) = self.engine.send(Command::Render {
            page,
            scale,
            request,
        }) {
            log::warn!("渲染请求发送失败: {error}");
        }
    }

    /// 按可视范围补渲染，并淘汰离得远的位图。
    ///
    /// 每一帧都会调用，靠 `Viewer::begin_request` 去重，所以不会重复排队。
    pub(super) fn ensure_visible_pages(&mut self) {
        let Some(layout) = self.doc_layout() else {
            return;
        };
        let range = layout.visible_range(
            self.scroll_top(),
            f32::from(self.viewport.height),
            PREFETCH_PAGES,
        );
        for page in range.clone() {
            self.ensure_page(page);
        }
        self.viewer.evict(range);
    }

    /// 滚动后按锚线更新当前页：读到哪里，页码和搜索高亮就跟到哪里。
    pub(super) fn sync_current_page(&mut self) {
        let Some(layout) = self.doc_layout() else {
            return;
        };
        let (page, _) = self.reading_anchor(&layout);
        if page == self.viewer.current_page {
            return;
        }
        self.viewer.current_page = page;
        self.refresh_matches(page);
    }

    /// 重算某一页的搜索匹配：当前页更新 `matches`，其他页只更新逐页缓存。
    pub(super) fn refresh_matches(&mut self, page: usize) {
        self.viewer.refresh_search(page);
    }

    /// 重新计算当前页的搜索匹配（查询变了、切页后、页面刚渲染完都要调）。
    pub(super) fn recompute_matches(&mut self, cx: &mut Context<Self>) {
        let page = self.viewer.current_page;
        self.refresh_matches(page);
        cx.notify();
    }

    /// 位图到达：只认还没作废的请求，缩放变了或页已经滚走就直接丢掉。
    pub(super) fn on_page_rendered(
        &mut self,
        page: usize,
        request: u64,
        width: u32,
        height: u32,
        bgra: Vec<u8>,
        chars: Vec<CharBox>,
        links: Vec<LinkBox>,
        cx: &mut Context<Self>,
    ) {
        let Some(scale) = self.viewer.take_request(page, request) else {
            return;
        };
        let Some(buffer) = image::RgbaImage::from_raw(width, height, bgra) else {
            self.set_status("页面位图格式错误", cx);
            cx.notify();
            return;
        };
        let render_image = RenderImage::new([image::Frame::new(buffer)]);
        log::info!(
            "rendered page {}: {}x{}, {} chars, {} links",
            page,
            width,
            height,
            chars.len(),
            links.len()
        );
        self.viewer.store_page(
            page,
            PageData {
                image: Arc::new(render_image),
                chars,
                links,
                scale,
            },
        );
        self.refresh_matches(page);
    }

    /// 跳到某一页：滚到页顶，并立刻安排这一页的渲染。
    pub(crate) fn set_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if !self.viewer.set_page(page) {
            return;
        }
        if let Some(layout) = self.doc_layout() {
            self.scroll_to_anchor(&layout, page, 0.0);
        }
        self.ensure_visible_pages();
        self.refresh_matches(page);
        self.flush_progress();
        cx.notify();
    }

    pub(crate) fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        // 缩放前后保持锚点不动，读者视线不会因缩放跳走。
        let anchor = self.doc_layout().map(|layout| self.reading_anchor(&layout));
        self.viewer.set_zoom(zoom);
        if let (Some(layout), Some((page, ratio))) = (self.doc_layout(), anchor) {
            self.scroll_to_anchor(&layout, page, ratio);
        }
        self.ensure_visible_pages();
        self.flush_progress();
        cx.notify();
    }

    // ---------- 适宽 / 适页 ----------

    pub(super) fn fit_area(&self) -> FitArea {
        FitArea::from_viewport(
            f32::from(self.viewport.width),
            f32::from(self.viewport.height),
        )
    }

    /// 视口变化后按当前适宽/适页模式重算缩放，并把阅读位置留在原来的锚点上。
    ///
    /// 返回 `true` 表示缩放或模式变了，调用方需要重新检查可视页。
    pub(super) fn refit(&mut self) -> bool {
        let mode = self.viewer.fit_mode;
        if mode == FitMode::Free {
            return false;
        }

        let anchor = self.doc_layout().map(|layout| self.reading_anchor(&layout));
        if !self.viewer.fit(mode, self.fit_area()) {
            return false;
        }
        if let (Some(layout), Some((page, ratio))) = (self.doc_layout(), anchor) {
            self.scroll_to_anchor(&layout, page, ratio);
        }
        true
    }

    /// 用户主动切换适宽/适页：重算缩放并立即重渲染。
    pub(crate) fn apply_fit(&mut self, mode: FitMode, cx: &mut Context<Self>) {
        let anchor = self.doc_layout().map(|layout| self.reading_anchor(&layout));
        if !self.viewer.fit(mode, self.fit_area()) {
            return;
        }
        if let (Some(layout), Some((page, ratio))) = (self.doc_layout(), anchor) {
            self.scroll_to_anchor(&layout, page, ratio);
        }
        self.ensure_visible_pages();
        self.flush_progress();
        cx.notify();
    }

    pub(crate) fn fit_to_page(&mut self, cx: &mut Context<Self>) {
        self.apply_fit(FitMode::Page, cx);
    }

    pub(crate) fn fit_to_width(&mut self, cx: &mut Context<Self>) {
        self.apply_fit(FitMode::Width, cx);
    }
}
