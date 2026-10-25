//! 左下角瞬时提示。

use gpui::{div, prelude::*, px};

use crate::app::AppState;
use crate::theme::Theme;

impl AppState {
    pub(crate) fn toast(&self, theme: &Theme) -> Option<gpui::Div> {
        // 首页有左侧边栏（宽 248px），toast 需要在内容区内显示，不能挡住侧栏底部的设置与关于
        if self.status.is_empty() {
            return None;
        }

        let left = if self.viewer.has_document() {
            px(14.)
        } else {
            px(264.)
        };

        Some(
            div()
                .absolute()
                .left(left)
                .bottom(px(24.))
                .px_3()
                .py_2()
                .rounded_md()
                .bg(theme.surface.opacity(0.92))
                .border_1()
                .border_color(theme.border)
                .shadow_md()
                .text_size(px(12.))
                .text_color(theme.text)
                .child(self.status.clone()),
        )
    }
}
