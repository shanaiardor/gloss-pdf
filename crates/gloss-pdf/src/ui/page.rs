//! 页面区域：连续滚动的页面列、每页位图与选择/搜索/高亮/链接覆盖层，以及右侧页边笺注栏。
//!
//! 整篇文档是**一列绝对定位的页面**：每页的 top/left 由 `DocumentLayout` 算好，
//! 总高度固定，所以只渲染可视范围附近的几页也不影响滚动条长度和滚动位移。

use gpui::{Context, ElementId, MouseButton, div, img, prelude::*, px};
use pdf_core::RectPt;

use crate::app::AppState;
use crate::geometry::merge_rects;
use crate::layout::{DocumentLayout, PREFETCH_PAGES};
use crate::theme::Theme;
use crate::ui::empty::empty_state;

impl AppState {
    pub(crate) fn page_content(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut container = div()
            .id("page-scroll")
            .size_full()
            .overflow_scroll()
            .track_scroll(&self.viewer.scroll)
            .on_scroll(cx.listener(Self::on_scroll_wheel))
            // 拖选挂在容器上：指针滑出页面甚至滑到别的页也能继续扩展选区。
            .on_mouse_move(cx.listener(Self::on_page_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_page_mouse_up))
            // 按住中键拖动阅读：像抓着纸往下拖，半速跟随指针。
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_page_middle_down))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_page_middle_up));

        if self.panning.is_some() {
            container = container.cursor(gpui::CursorStyle::ClosedHand);
        }

        let Some(layout) = self.doc_layout() else {
            return container.child(empty_state(theme, cx));
        };
        if layout.is_empty() {
            return container.child(empty_state(theme, cx));
        }

        let range = layout.visible_range(
            self.scroll_top(),
            f32::from(self.viewport.height),
            PREFETCH_PAGES,
        );

        let mut canvas_children: Vec<gpui::AnyElement> = Vec::new();
        for page in range {
            canvas_children.push(self.page_view(page, &layout, theme, cx).into_any_element());
            if let Some(rail) = self.page_gloss_rail(page, &layout, theme, cx) {
                canvas_children.push(rail.into_any_element());
            }
        }

        container.child(
            div()
                .relative()
                .w(px(layout.content_width))
                .h(px(layout.content_height))
                .children(canvas_children),
        )
    }

    /// 一页：位图或占位，加上这一页自己的高亮/搜索命中/选区/笺注锚点/链接覆盖层。
    fn page_view(
        &self,
        index: usize,
        layout: &DocumentLayout,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let Some(geometry) = layout.page_box(index) else {
            return div();
        };
        let Some(page_size) = self.viewer.page_size_at(index) else {
            return div();
        };

        let zoom = self.viewer.zoom;
        let width = geometry.width;
        let height = geometry.height;

        let Some(page) = self.viewer.page(index) else {
            return page_placeholder(geometry.left, geometry.top, width, height, theme);
        };
        if page.tiles.is_empty() || page.scale <= 0.0 {
            return page_placeholder(geometry.left, geometry.top, width, height, theme);
        }

        let mut overlays: Vec<gpui::AnyElement> = Vec::new();

        for highlight in self.viewer.highlights.for_page(index) {
            for rect in &highlight.rects {
                overlays.push(
                    rect_overlay(rect, page_size.height, zoom, theme.highlight).into_any_element(),
                );
            }
        }

        // 已钉住的词汇笺注：在原词下方画一条细下划线与淡底色，点击也能展开对应的查词卡。
        for gloss in self.viewer.highlights.glosses_for_page(index) {
            let gloss_id = gloss.id;
            for (rect_idx, rect) in gloss.rects.iter().enumerate() {
                let id = ElementId::NamedInteger(
                    "gloss-anchor-rect".into(),
                    gloss_id
                        .wrapping_mul(31)
                        .wrapping_add(rect_idx as u64),
                );
                overlays.push(
                    gloss_rect_overlay(
                        rect,
                        page_size.height,
                        zoom,
                        theme.accent.opacity(0.10),
                        theme.accent.opacity(0.65),
                    )
                    .id(id)
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _event: &gpui::ClickEvent, _window, cx| {
                            this.open_pinned_gloss(gloss_id, cx);
                        }),
                    )
                    .into_any_element(),
                );
            }
        }

        // 当前激活态查词锚点：在正文中标出正被查阅的词条。
        if self.dictionary.open && self.dictionary.anchor_page == index {
            for rect in &self.dictionary.anchor_rects {
                overlays.push(
                    gloss_rect_overlay(
                        rect,
                        page_size.height,
                        zoom,
                        theme.accent.opacity(0.18),
                        theme.accent,
                    )
                    .into_any_element(),
                );
            }
        }

        let search = &self.viewer.search;
        let active = search.active_page() == index;
        for (match_index, range) in search.matches_for(index).iter().enumerate() {
            let color = if active && match_index == search.current {
                theme.search_current
            } else {
                theme.search_hit
            };
            if let Some(chars) = page.chars.get(*range.start()..=*range.end()) {
                for rect in merge_rects(chars.iter().map(|character| character.rect).collect()) {
                    overlays.push(
                        rect_overlay(&rect, page_size.height, zoom, color).into_any_element(),
                    );
                }
            }
        }

        if self.viewer.selection.page == Some(index) {
            for rect in self.viewer.selection.rects(&page.chars) {
                overlays.push(
                    rect_overlay(&rect, page_size.height, zoom, theme.selection).into_any_element(),
                );
            }
        }

        for (link_index, link) in page.links.iter().enumerate() {
            let rect = link.rect;
            let target = link.target.clone();
            overlays.push(
                div()
                    .id(index * 100_000 + link_index)
                    .absolute()
                    .left(px(rect.left * zoom))
                    .top(px((page_size.height - rect.top) * zoom))
                    .w(px((rect.width() * zoom).max(1.0)))
                    .h(px((rect.height() * zoom).max(1.0)))
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _event: &gpui::ClickEvent, _window, cx| {
                            this.follow_link(target.clone(), cx);
                        }),
                    )
                    .into_any_element(),
            );
        }

        // 外层只负责把这页放到文档里的位置，内层才是页面本身（覆盖层的定位基准）。
        div()
            .absolute()
            .left(px(geometry.left))
            .top(px(geometry.top))
            .w(px(width))
            .h(px(height))
            .child(
                div()
                    .relative()
                    .size_full()
                    .bg(gpui::white())
                    .shadow_lg()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _window, cx| {
                            this.on_page_mouse_down(index, event, cx);
                        }),
                    )
                    .child(
                        div()
                            .relative()
                            .size_full()
                            .children(page.tiles.iter().map(|tile| {
                                let tile_left = tile.origin_x as f32 / page.scale * zoom;
                                let tile_top = tile.origin_y as f32 / page.scale * zoom;
                                let tile_width = tile.width as f32 / page.scale * zoom;
                                let tile_height = tile.height as f32 / page.scale * zoom;
                                img(tile.image.clone())
                                    .absolute()
                                    .left(px(tile_left))
                                    .top(px(tile_top))
                                    .w(px(tile_width))
                                    .h(px(tile_height))
                            }))
                            .children(overlays),
                    ),
            )
    }
}

fn page_placeholder(left: f32, top: f32, width: f32, height: f32, theme: &Theme) -> gpui::Div {
    div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.surface)
        .text_color(theme.text_muted)
        .text_size(px(13.))
        .child("正在渲染…")
}

fn rect_overlay(rect: &RectPt, page_height: f32, zoom: f32, color: gpui::Rgba) -> gpui::Div {
    div()
        .absolute()
        .left(px(rect.left * zoom))
        .top(px((page_height - rect.top) * zoom))
        .w(px((rect.width() * zoom).max(0.5)))
        .h(px((rect.height() * zoom).max(0.5)))
        .bg(color)
}

fn gloss_rect_overlay(
    rect: &RectPt,
    page_height: f32,
    zoom: f32,
    bg_color: gpui::Rgba,
    underline_color: gpui::Rgba,
) -> gpui::Div {
    rect_overlay(rect, page_height, zoom, bg_color)
        .border_b_2()
        .border_color(underline_color)
}
