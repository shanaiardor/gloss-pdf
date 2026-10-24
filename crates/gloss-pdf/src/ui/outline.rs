//! 浮动目录面板。

use gpui::{ClickEvent, Context, div, prelude::*, px};

use crate::app::AppState;
use crate::theme::Theme;

impl AppState {
    pub(crate) fn outline_panel(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.outline_open {
            return None;
        }

        let outline = self
            .viewer
            .doc
            .as_ref()
            .map(|doc| doc.outline.clone())
            .unwrap_or_default();

        let mut list = div()
            .id("outline-scroll")
            .flex_1()
            .overflow_y_scroll()
            .p_2();
        if outline.is_empty() {
            list = list.child(
                div()
                    .p_2()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child("这个 PDF 没有目录"),
            );
        } else {
            let hover_bg = theme.surface_hover;
            let cx_list = &mut *cx;
            list = list.children(outline.iter().enumerate().map(move |(index, item)| {
                let item = item.clone();
                div()
                    .id(200_000 + index)
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_size(px(13.))
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover_bg))
                    .child(format!("{}{}", "  ".repeat(item.depth), item.title))
                    .on_click(
                        cx_list.listener(move |this, _event: &ClickEvent, _window, cx| {
                            this.set_page(item.page, cx);
                        }),
                    )
            }));
        }

        Some(
            div()
                .absolute()
                .left(px(12.))
                .top(px(56.))
                .bottom(px(70.))
                .w(px(300.))
                .flex()
                .flex_col()
                .rounded_lg()
                .bg(theme.surface.opacity(0.96))
                .border_1()
                .border_color(theme.border)
                .shadow_lg()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .h(px(40.))
                        .px_3()
                        .border_b_1()
                        .border_color(theme.border)
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("目录")
                        .child(
                            div()
                                .id("outline-close")
                                .px_1()
                                .rounded_md()
                                .cursor_pointer()
                                .text_color(theme.text_muted)
                                .hover({
                                    let hover_bg = theme.surface_hover;
                                    move |style| style.bg(hover_bg)
                                })
                                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                    this.outline_open = false;
                                    cx.notify();
                                }))
                                .child("✕"),
                        ),
                )
                .child(list),
        )
    }
}
