//! 浮动搜索框：输入框本体由 [`crate::ui::text_input`] 绘制，这里只做外框和命中状态。

use gpui::{div, prelude::*, px};

use crate::app::AppState;
use crate::theme::Theme;

impl AppState {
    pub(crate) fn search_overlay(&self, theme: &Theme) -> gpui::Div {
        let search = &self.viewer.search;

        let status = if search.query.is_empty() {
            String::new()
        } else if search.scanning {
            "搜索中…".to_string()
        } else if search.total == 0 {
            "无匹配".to_string()
        } else {
            match search.hit_position() {
                Some((index, total)) => format!("{index}/{total}"),
                None => format!("全文 {}", search.total),
            }
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
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(theme.text_muted)
                    .child("搜索"),
            )
            .child(self.search_input.clone())
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(status),
            );

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
