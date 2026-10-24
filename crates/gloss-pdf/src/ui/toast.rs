//! 左下角瞬时提示。

use gpui::{div, prelude::*, px};

use crate::app::AppState;
use crate::theme::Theme;

impl AppState {
    pub(crate) fn toast(&self, theme: &Theme) -> Option<gpui::Div> {
        if self.status.is_empty() {
            return None;
        }

        Some(
            div()
                .absolute()
                .left(px(14.))
                .bottom(px(64.))
                .px_3()
                .py_2()
                .rounded_md()
                .bg(theme.surface.opacity(0.92))
                .border_1()
                .border_color(theme.border)
                .text_size(px(11.))
                .text_color(theme.text_muted)
                .child(self.status.clone()),
        )
    }
}
