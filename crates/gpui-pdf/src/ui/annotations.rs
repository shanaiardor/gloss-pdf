//! 批注侧栏：按页列出所有高亮，点击跳到该页，右侧 ✕ 删除。

use gpui::{ClickEvent, Context, FontWeight, div, prelude::*, px};

use crate::app::AppState;
use crate::theme::Theme;

impl AppState {
    pub(crate) fn annotations_panel(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        if !self.highlight_panel_open || !self.viewer.has_document() {
            return None;
        }

        let items = self.viewer.highlights.sorted();
        let count = items.len();
        let current_page = self.viewer.current_page;

        let mut list = div()
            .id("annotations-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p_2();

        if items.is_empty() {
            list = list.child(
                div()
                    .p_2()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child("选中文字按 H 添加高亮"),
            );
        } else {
            let hover = theme.surface_hover;
            let muted = theme.text_muted;
            let text = theme.text;
            let accent = theme.accent;
            let mono = theme.mono_font.clone();
            let cx_list = &mut *cx;

            list = list.children(items.into_iter().enumerate().map(move |(index, item)| {
                let id = item.id;
                let page = item.page;
                let snippet = summarize(&item.text);
                let is_current = page == current_page;
                let label_color = if is_current { accent } else { muted };

                div()
                    .id(300_000 + index)
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(cx_list.listener(move |this, _event: &ClickEvent, _window, cx| {
                        this.set_page(page, cx);
                    }))
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(mono.clone())
                            .text_size(px(11.))
                            .text_color(label_color)
                            .child(format!("P{}", page + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(12.))
                            .text_color(text)
                            .child(snippet),
                    )
                    .child(
                        div()
                            .id(400_000 + index)
                            .flex_shrink_0()
                            .px(px(4.))
                            .rounded_md()
                            .text_size(px(11.))
                            .text_color(muted)
                            .hover(move |style| style.bg(hover))
                            .on_click(cx_list.listener(move |this, _event: &ClickEvent, _window, cx| {
                                cx.stop_propagation();
                                this.delete_highlight(id, cx);
                            }))
                            .child("✕"),
                    )
            }));
        }

        Some(
            div()
                .absolute()
                .right(px(12.))
                .top(px(56.))
                .bottom(px(70.))
                .w(px(240.))
                .flex()
                .flex_col()
                .rounded_lg()
                .bg(theme.surface.opacity(0.97))
                .border_1()
                .border_color(theme.border)
                .shadow_lg()
                .occlude()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(format!("批注 · {count}")),
                        )
                        .child(
                            div()
                                .id("annotations-close")
                                .px(px(4.))
                                .rounded_md()
                                .cursor_pointer()
                                .text_size(px(11.))
                                .text_color(theme.text_muted)
                                .hover({
                                    let hover = theme.surface_hover;
                                    move |style| style.bg(hover)
                                })
                                .on_click(cx.listener(
                                    |this, _event: &ClickEvent, _window, cx| {
                                        this.toggle_highlight_panel(cx);
                                    },
                                ))
                                .child("✕"),
                        ),
                )
                .child(list),
        )
    }
}

/// 列表里只显示一小段文字，避免一行被撑开。
fn summarize(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return "高亮".to_string();
    }
    let mut summary: String = text.chars().take(22).collect();
    if text.chars().count() > 22 {
        summary.push('…');
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::summarize;

    #[test]
    fn summarize_truncates_long_text() {
        let text = "一二三四五六七八九十一二三四五六七八九十一二三";
        let summary = summarize(text);
        assert!(summary.ends_with('…'));
        assert_eq!(summary.chars().count(), 23);
    }

    #[test]
    fn summarize_falls_back_for_empty_text() {
        assert_eq!(summarize("   "), "高亮");
    }
}
