//! 页面区域：滚动容器、页面位图、选择/搜索/高亮/链接覆盖层。

use gpui::{Context, MouseButton, div, img, prelude::*, px};
use pdf_core::RectPt;

use crate::app::{AppState, BOTTOM_INSET, PAGE_PAD};
use crate::geometry::merge_rects;
use crate::theme::Theme;
use crate::ui::empty::empty_state;

impl AppState {
    pub(crate) fn page_content(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let page_width = self.viewer.page_size().map_or(0.0, |page| page.width) * self.viewer.zoom;
        let available = f32::from(self.viewport.width) - PAGE_PAD * 2.0;
        let center = ((available - page_width) / 2.0).max(0.0);

        div()
            .id("page-scroll")
            .size_full()
            .overflow_scroll()
            .track_scroll(&self.viewer.scroll)
            .child(
                div()
                    .p(px(PAGE_PAD))
                    .pb(px(BOTTOM_INSET))
                    .child(div().ml(px(center)).child(self.page_view(theme, cx))),
            )
    }

    pub(crate) fn page_view(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let Some(page_size) = self.viewer.page_size() else {
            return empty_state(theme, cx);
        };

        let zoom = self.viewer.zoom;
        let width = page_size.width * zoom;
        let height = page_size.height * zoom;

        let Some(page) = &self.viewer.page else {
            return div()
                .w(px(width))
                .h(px(height))
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.surface)
                .text_color(theme.text_muted)
                .text_size(px(13.))
                .child("正在渲染…");
        };

        let mut overlays: Vec<gpui::AnyElement> = Vec::new();

        for highlight in self.viewer.highlights.for_page(self.viewer.current_page) {
            for rect in &highlight.rects {
                overlays.push(
                    rect_overlay(rect, page_size.height, zoom, theme.highlight).into_any_element(),
                );
            }
        }

        let search = &self.viewer.search;
        for (index, range) in search.matches.iter().enumerate() {
            let color = if index == search.current {
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

        for rect in self.viewer.selection.rects(&page.chars) {
            overlays.push(
                rect_overlay(&rect, page_size.height, zoom, theme.selection).into_any_element(),
            );
        }

        for (index, link) in page.links.iter().enumerate() {
            let rect = link.rect;
            let target = link.target.clone();
            overlays.push(
                div()
                    .id(100_000 + index)
                    .absolute()
                    .left(px(rect.left * zoom))
                    .top(px((page_size.height - rect.top) * zoom))
                    .w(px((rect.width() * zoom).max(1.0)))
                    .h(px((rect.height() * zoom).max(1.0)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _event: &gpui::ClickEvent, _window, cx| {
                        this.follow_link(target.clone(), cx);
                    }))
                    .into_any_element(),
            );
        }

        div()
            .relative()
            .w(px(width))
            .h(px(height))
            .bg(gpui::white())
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_page_mouse_down))
            .on_mouse_move(cx.listener(Self::on_page_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_page_mouse_up))
            .child(img(page.image.clone()).w_full().h_full())
            .children(overlays)
    }
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
