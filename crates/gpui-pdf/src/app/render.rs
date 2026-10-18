//! 渲染调度：用 `DocumentLayout` 把缩放、视口与滚动偏移换算成「哪些页可见」，
//! 据此发位图请求、淘汰缓存、同步当前页与搜索高亮。
//!
//! 边界是「这一帧该有哪些位图」：输入是缩放、视口和滚动句柄，输出是
//! `Command::Render` 与 `Viewer` 里的缓存状态。这里不写盘、不改窗口，也不处理
//! 输入事件；阅读进度的持久化由 `document` 负责，窗口尺寸由 `window` 负责。

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{Context, Point, RenderImage, px};
use pdf_core::{Command, LinkBox};

use crate::layout::{DocumentLayout, FitArea, FitMode, PREFETCH_PAGES};
use crate::tiles::TileGrid;
use crate::viewer::TileKey;

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

    /// 当前窗口缩放因子下的渲染缩放（已乘 DPI）。
    fn render_scale(&self) -> f32 {
        (self.viewer.zoom * self.scale_factor).clamp(0.1, 8.0)
    }

    /// 请求某一页的文字/链接覆盖层；它和缩放无关，每页只取一次。
    fn ensure_page_elements(&mut self, page: usize) {
        if page >= self.viewer.page_count() {
            return;
        }
        let request = self.request_id + 1;
        if !self.viewer.begin_element_request(page, request) {
            return;
        }
        self.request_id = request;
        if let Err(error) = self.engine.send(Command::PageElements { page, request }) {
            log::warn!("页面元素请求发送失败: {error}");
        }
    }

    /// 只请求与视口相交的瓦片；同一瓦片同一缩放在跑或已缓存时跳过。
    fn ensure_page_tiles(
        &mut self,
        page: usize,
        layout: &DocumentLayout,
        visible: &mut HashSet<TileKey>,
    ) {
        let Some(geometry) = layout.page_box(page) else {
            return;
        };
        let Some(page_size) = self.viewer.page_size_at(page) else {
            return;
        };
        let scale = self.render_scale();
        let grid = TileGrid::new(page_size, scale);

        // 视口在页面局部坐标里的可见矩形（逻辑像素）。
        let offset = self.viewer.scroll.offset();
        let view_left = -f32::from(offset.x);
        let view_top = -f32::from(offset.y);
        let view_right = view_left + f32::from(self.viewport.width);
        let view_bottom = view_top + f32::from(self.viewport.height);
        let left = (view_left - geometry.left).clamp(0.0, geometry.width);
        let right = (view_right - geometry.left).clamp(0.0, geometry.width);
        let top = (view_top - geometry.top).clamp(0.0, geometry.height);
        let bottom = (view_bottom - geometry.top).clamp(0.0, geometry.height);
        if right <= left || bottom <= top {
            return;
        }

        // 逻辑像素 → 整页像素。二者之比就是 DPI 缩放因子。
        let to_pixels_x = grid.full_width as f32 / geometry.width.max(1.0);
        let to_pixels_y = grid.full_height as f32 / geometry.height.max(1.0);
        let tiles = grid.tiles_for_rect(
            left * to_pixels_x,
            top * to_pixels_y,
            right * to_pixels_x,
            bottom * to_pixels_y,
        );

        for tile in tiles {
            visible.insert((page, tile.origin_x, tile.origin_y));
            let request = self.request_id + 1;
            if !self
                .viewer
                .begin_tile_request(page, tile.origin_x, tile.origin_y, request, scale)
            {
                continue;
            }
            self.request_id = request;
            if let Err(error) = self.engine.send(Command::RenderTile {
                page,
                scale,
                origin_x: tile.origin_x,
                origin_y: tile.origin_y,
                width: tile.width,
                height: tile.height,
                request,
            }) {
                log::warn!("瓦片渲染请求发送失败: {error}");
            }
        }
    }

    /// 按可视范围补渲染，并淘汰离得远的页面位图。
    ///
    /// 每一帧都会调用，靠 `Viewer` 的请求去重，所以不会重复排队。
    pub(super) fn ensure_visible_pages(&mut self) {
        let Some(layout) = self.doc_layout() else {
            return;
        };
        let range = layout.visible_range(
            self.scroll_top(),
            f32::from(self.viewport.height),
            PREFETCH_PAGES,
        );
        let mut visible = HashSet::new();
        for page in range.clone() {
            self.ensure_page_elements(page);
            self.ensure_page_tiles(page, &layout, &mut visible);
        }
        self.viewer.evict(range, &visible);
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

    /// 瓦片到达：只认还没作废的请求，缩放变了或页已经滚走就直接丢掉。
    pub(super) fn on_tile_rendered(
        &mut self,
        page: usize,
        request: u64,
        origin_x: i32,
        origin_y: i32,
        width: u32,
        height: u32,
        bgra: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        let Some(scale) = self
            .viewer
            .take_tile_request(page, origin_x, origin_y, request)
        else {
            return;
        };
        if (scale - self.render_scale()).abs() >= 0.005 {
            return;
        }
        let Some(buffer) = image::RgbaImage::from_raw(width, height, bgra) else {
            self.set_status("页面位图格式错误", cx);
            cx.notify();
            return;
        };
        let render_image = RenderImage::new([image::Frame::new(buffer)]);
        self.viewer.store_tile(
            page,
            scale,
            origin_x,
            origin_y,
            width,
            height,
            Arc::new(render_image),
        );
    }

    /// 文字/链接覆盖层到达：坐标与瓦片无关，直接刷新搜索匹配。
    pub(super) fn on_page_elements(
        &mut self,
        page: usize,
        request: u64,
        chars: Vec<pdf_core::CharBox>,
        links: Vec<LinkBox>,
        cx: &mut Context<Self>,
    ) {
        if !self.viewer.take_element_request(page, request) {
            return;
        }
        self.viewer.store_elements(page, chars, links);
        self.refresh_matches(page);
        cx.notify();
    }

    /// 跳到某一页：滚到页顶，并立刻安排这一页的渲染。
    pub(crate) fn set_page(&mut self, page: usize, cx: &mut Context<Self>) {
        // 从别处翻页时收起页码输入，避免框里还停着旧页码。
        if self.page_input_open {
            self.page_input_open = false;
        }
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

    /// 按百分点步进缩放（键盘快捷键与底部胶囊的 ± 按钮）：每次刚好 1%。
    ///
    /// 用 `zoom_step` 而不是直接相加，是为了先把适页/适宽留下的小数比例
    /// 对齐到整百分点，连按几下就能落到 100%、125% 这种整数百分比。
    pub(crate) fn step_zoom(&mut self, percent: f32, cx: &mut Context<Self>) {
        let zoom = crate::layout::zoom_step(self.viewer.zoom, percent);
        self.set_zoom(zoom, cx);
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

    /// 以指针为锚点缩放：指针下的内容点缩放后仍停在指针下。
    ///
    /// `percent` 是缩放百分点变化量（1 表示放大 1%）；Ctrl + 滚轮传的是
    /// `wheel_zoom_percent` 算出的带小数的值，触控板的细小增量也能生效。
    ///
    /// `scrolled` 是内建滚动监听已经施加到滚动偏移上的像素量。同一个元素上
    /// GPUI 先跑内建滚动、再跑用户监听，所以这里先减回去，得到事件发生前的
    /// 滚动位置；否则锚点会带上一次滚轮的位移。
    pub(super) fn zoom_at_pointer(
        &mut self,
        percent: f32,
        position: Point<gpui::Pixels>,
        scrolled: Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.doc_layout() else {
            return;
        };
        let viewport_width = f32::from(self.viewport.width);
        let viewport_height = f32::from(self.viewport.height);
        let offset = self.viewer.scroll.offset();
        let max_left = (layout.content_width - viewport_width).max(0.0);
        let max_top = (layout.content_height - viewport_height).max(0.0);
        let old_left = (-f32::from(offset.x) - f32::from(scrolled.x)).clamp(0.0, max_left);
        let old_top = (-f32::from(offset.y) - f32::from(scrolled.y)).clamp(0.0, max_top);

        let content_x = old_left + f32::from(position.x);
        let content_y = old_top + f32::from(position.y);
        let Some((page, ratio_x, ratio_y)) = layout.page_ratios(content_x, content_y) else {
            return;
        };

        let zoom = crate::layout::zoom_add(self.viewer.zoom, percent);
        if (zoom - self.viewer.zoom).abs() < 0.0001 {
            return;
        }
        self.viewer.set_zoom(zoom);

        let Some(layout) = self.doc_layout() else {
            return;
        };
        let Some((left, top)) = layout.scroll_for_ratios(
            page,
            ratio_x,
            ratio_y,
            f32::from(position.x),
            f32::from(position.y),
            viewport_width,
            viewport_height,
        ) else {
            return;
        };
        self.viewer
            .scroll
            .set_offset(Point::new(px(-left), px(-top)));

        self.ensure_visible_pages();
        self.sync_current_page();
        self.flush_progress();
        cx.notify();
    }

    /// 按住中键拖动：把指针位移换算成滚动偏移（反方向、半速）。
    ///
    /// 两个轴都夹在内容范围内，拖到文档头/尾或左右边界就停住；
    /// 每步只处理增量，不记「按下时的原点」，所以指针在容器外丢过几次
    /// 事件再回来也不会跳一段。
    pub(super) fn pan_by(&mut self, delta: Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let Some(layout) = self.doc_layout() else {
            return;
        };
        let max_left = (layout.content_width - f32::from(self.viewport.width)).max(0.0);
        let max_top = (layout.content_height - f32::from(self.viewport.height)).max(0.0);
        let offset = self.viewer.scroll.offset();
        let left = (-f32::from(offset.x) + crate::layout::pan_delta(f32::from(delta.x)))
            .clamp(0.0, max_left);
        let top = (-f32::from(offset.y) + crate::layout::pan_delta(f32::from(delta.y)))
            .clamp(0.0, max_top);
        if (f32::from(offset.x) + left).abs() < 0.01 && (f32::from(offset.y) + top).abs() < 0.01 {
            // 已经在边界上：不重设偏移，免得每帧都把滚动句柄推回同一个值。
            return;
        }
        self.viewer
            .scroll
            .set_offset(Point::new(px(-left), px(-top)));

        self.ensure_visible_pages();
        self.sync_current_page();
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
