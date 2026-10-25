//! AI 释义卡：滚在笺注栏里、词典卡下方的那张独立卡。
//!
//! 只负责画：状态与节流在 `ai_query.rs`，Markdown 在 `ui/ai_markdown.rs`。
//! 卡片自带滚动，所以流式增长不会把测高顶到失控（超出 max_height 的部分内部滚动）。

use gpui::{AnyElement, ClickEvent, FontWeight, Window, div, prelude::*, px, relative};

use crate::ai_query::AiPhase;
use crate::app::AppState;
use crate::layout::GLOSS_RAIL_WIDTH;
use crate::theme::Theme;
use crate::ui::ai_markdown;

impl AppState {
    pub(crate) fn ai_card(
        &self,
        top: f32,
        max_height: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.ai_card_visible() {
            return None;
        }

        let body_h = (max_height - 64.0).max(48.0);
        let mut body = div()
            .id("ai-card-body")
            .w_full()
            .min_w(px(0.))
            .max_h(px(body_h))
            .min_h(px(0.))
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap(px(6.));

        match &self.ai.phase {
            AiPhase::Idle => return None,
            AiPhase::Thinking => {
                body = body.child(muted("正在思考…", theme));
            }
            AiPhase::Streaming { text, tool } => {
                if let Some(tool) = tool {
                    body = body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .text_size(px(10.))
                            .text_color(theme.text_muted)
                            .child("⚙")
                            .child(div().min_w(px(0.)).truncate().child(tool.label())),
                    );
                }
                body = answer_sections(body, text, theme, true);
            }
            AiPhase::Done { text, model } => {
                body = answer_sections(body, text, theme, false);
                if !model.is_empty() {
                    body = body.child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.text_muted)
                            .child(format!("· {model}")),
                    );
                }
            }
            AiPhase::Failed { kind, message } => {
                body = body.child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.accent)
                        .child(failure_title(*kind)),
                );
                body = body.child(
                    div()
                        .text_size(px(10.))
                        .text_color(theme.text_muted)
                        .child(message.clone()),
                );
                if kind.retryable() {
                    body = body.child(
                        div()
                            .id("ai-card-retry")
                            .self_start()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(theme.accent)
                            .text_color(theme.accent_text)
                            .text_size(px(10.))
                            .hover({
                                let hover = theme.accent_hover;
                                move |style| style.bg(hover)
                            })
                            .on_click(cx.listener(
                                |this, _event: &ClickEvent, _window, cx| {
                                    this.retry_ai_lookup(cx);
                                },
                            ))
                            .child("重试"),
                    );
                }
            }
        }

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(34.))
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("AI 释义"),
                    )
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(10.))
                            .text_color(theme.text_muted)
                            .child(self.ai.term.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(self.ai_card_pin_button(theme, cx))
                    .child(
                        div()
                            .id("ai-card-close")
                            .px_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .hover({
                        let hover_bg = theme.surface_hover;
                        move |style| style.bg(hover_bg)
                    })
                            .on_click(cx.listener(
                                |this, _event: &ClickEvent, _window: &mut Window, cx| {
                                    this.close_ai_card(cx);
                                },
                            ))
                            .child("✕"),
                    ),
            );

        Some(
            div()
                .absolute()
                .left_0()
                .top(px(top))
                .w(px(GLOSS_RAIL_WIDTH))
                .max_h(px(max_height))
                .flex()
                .flex_col()
                .overflow_hidden()
                .occlude()
                .rounded_lg()
                .bg(theme.surface.opacity(0.98))
                .border_1()
                .border_color(theme.border)
                .shadow_lg()
                .child(header)
                .child(body)
                .into_any_element(),
        )
    }
}

impl AppState {
    /// AI 卡的「钉住」：把这条答案钉成页边笺注（长句也会按选区矩形高亮）。
    fn ai_card_pin_button(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let has_text = match &self.ai.phase {
            AiPhase::Done { text, .. } | AiPhase::Streaming { text, .. } => {
                !ai_core::parse_answer_sections(text).is_empty()
            }
            _ => false,
        };
        if !has_text || self.ai.term.trim().is_empty() {
            return div().into_any_element();
        }
        div()
            .id("ai-card-pin")
            .px_1()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(11.))
            .text_color(theme.text_muted)
            .hover({
                let hover = theme.surface_hover;
                move |style| style.bg(hover)
            })
            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                this.pin_current_ai(cx);
            }))
            .child("钉住")
            .into_any_element()
    }
}

/// 三段式回答：标题负责「一眼是什么」，内容负责解释，原文单独放在引用块里。
fn answer_sections(
    mut body: gpui::Stateful<gpui::Div>,
    raw: &str,
    theme: &Theme,
    streaming: bool,
) -> gpui::Stateful<gpui::Div> {
    let sections = ai_core::parse_answer_sections(raw);

    if !sections.title.is_empty() {
        body = body.child(
            div()
                .text_size(px(13.))
                .line_height(relative(1.35))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(ai_markdown::plain_text(&sections.title)),
        );
    }
    if !sections.content.is_empty() {
        body = body.child(ai_markdown::render(&sections.content, theme, 12.0));
    }
    if !sections.quote.is_empty() {
        body = body.child(
            div()
                .mt(px(2.))
                .pl(px(8.))
                .border_l(px(2.0))
                .border_color(theme.accent.opacity(0.45))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(9.))
                        .text_color(theme.text_muted)
                        .child("原文"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .line_height(relative(1.5))
                        .text_color(theme.text_muted)
                        .child(sections.quote.clone()),
                ),
        );
    }

    if streaming {
        if sections.is_empty() {
            body = body.child(muted("正在生成…", theme));
        } else {
            // 流式光标：让「还在写」这件事在视觉上成立。
            body = body.child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.accent)
                    .child("▍"),
            );
        }
    } else if sections.is_empty() {
        body = body.child(muted("没有返回内容", theme));
    }

    body
}

fn failure_title(kind: ai_core::ErrorKind) -> &'static str {
    match kind {
        ai_core::ErrorKind::Auth => "API Key 无效或无权限，请到设置里检查",
        ai_core::ErrorKind::RateLimited => "请求过于频繁，稍后再试",
        ai_core::ErrorKind::Network => "网络不可用",
        ai_core::ErrorKind::Other => "AI 查询失败",
    }
}

fn muted(text: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .text_size(px(11.))
        .text_color(theme.text_muted)
        .child(text)
}
