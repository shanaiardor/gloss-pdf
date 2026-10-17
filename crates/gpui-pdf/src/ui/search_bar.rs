//! 浮动搜索框。

use gpui::{div, prelude::*, px};

use crate::app::AppState;
use crate::theme::Theme;

impl AppState {
    pub(crate) fn search_overlay(&self, theme: &Theme) -> gpui::Div {
        let search = &self.viewer.search;
        let result = if search.matches.is_empty() {
            String::new()
        } else {
            format!("{}/{}", search.current + 1, search.matches.len())
        };
        let display = if search.query.is_empty() {
            "输入关键词，Enter 下一个".to_string()
        } else {
            search.query.clone()
        };
        let color = if search.query.is_empty() {
            theme.text_muted
        } else {
            theme.text
        };

        let bar = div()
            .flex()
            .items_center()
            .gap_2()
            .w(px(520.))
            .h(px(38.))
            .px_3()
            .rounded_full()
            .bg(theme.surface.opacity(0.96))
            .border_1()
            .border_color(theme.accent)
            .shadow_lg()
            .text_size(px(13.))
            .child(div().text_color(theme.text_muted).child("搜索"))
            .child(div().flex_1().text_color(color).child(display))
            .child(div().text_color(theme.text_muted).child(result));

        div()
            .absolute()
            .top(px(54.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(bar)
    }
}
