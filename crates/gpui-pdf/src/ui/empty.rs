//! 空状态：没打开文件时只有标题和打开按钮。

use gpui::{ClickEvent, Context, div, prelude::*, px};

use crate::app::AppState;
use crate::theme::Theme;

pub(crate) fn empty_state(theme: &Theme, cx: &mut Context<AppState>) -> gpui::Div {
    let hover = theme.accent_hover;

    div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_6()
        .child(
            div()
                .text_size(px(24.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme.text)
                .child("GPUI PDF"),
        )
        .child(
            div()
                .id("empty-open")
                .px_6()
                .py_2()
                .rounded_full()
                .bg(theme.accent)
                .text_color(theme.accent_text)
                .text_size(px(13.))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                    this.open_pdf(window, cx);
                }))
                .child("打开 PDF"),
        )
}
