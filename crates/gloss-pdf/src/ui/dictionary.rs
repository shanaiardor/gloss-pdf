//! 页边笺注（Marginalia）：在 PDF 纸张右侧的专属笺注栏里就地渲染查词卡片与已钉住的词汇笺注。
//!
//! 双击 PDF 正文查词时，激活态查词卡（Active Gloss）直接锚定在该页对应的词条高度；
//! 点击「钉住」可将当前词条浓缩为一张紧凑笺注卡（[`PinnedGloss`]）常驻在页边，并随高亮
//! sidecar 一起持久化。释义按 CSS 计算样式渲染：段落用 GPUI 的 [`StyledText`] 行内 run 画
//! 颜色/字重/下划线，块级属性落到 `div` 上；表格按行列布局，图片直接交给 GPUI 解码。

use std::sync::Arc;

use dict_core::{
    Align, AlignItems, AlignSelf, Block, BlockKind, BlockStyle, Color as DictColor, Container,
    DictEntry, FlexDirection, FlexWrap, FloatSide, ImageFormat as DictImageFormat, ImageSpan,
    InlineBox, Justify, LinkTarget, Paragraph, Span, SpanStyle, Table, TableCell,
};
use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, Font, FontFallbacks, FontFeatures, FontStyle,
    FontWeight, Hsla, Image, ImageFormat, InteractiveText, ObjectFit, Rgba, ScrollEvent,
    StrikethroughStyle, StyledText, TextAlign, TextRun, UnderlineStyle, Window, div, img,
    prelude::*, px, relative,
};

use crate::app::AppState;
use crate::dictionary::{DictLoadState, DictRequest};
use crate::highlights::PinnedGloss;
use crate::layout::{
    DocumentLayout, GLOSS_CARD_GAP, GLOSS_RAIL_GAP, GLOSS_RAIL_WIDTH, GlossSlotInput, PAGE_PAD,
    layout_gloss_slots,
};
use crate::theme::Theme;

/// 把释义内 `entry://` 链接的查词请求送回 `AppState` 的通道。
type LookupSender = futures::channel::mpsc::UnboundedSender<DictRequest>;

/// 钉住的紧凑笺注卡首帧还没有实测高度时的保守估算（逻辑像素）。
///
/// 真实高度按内容测量；估算值只用于第一帧，宁可多留一点也不能压住下一张卡。
const PINNED_CARD_ESTIMATED_HEIGHT: f32 = 120.0;
/// 激活态查词卡至少保留的高度；放不下时优先压缩正文滚动区。
const MIN_ACTIVE_CARD_HEIGHT: f32 = 220.0;
/// 激活态查词卡在 `GlossSlotInput` 中的保留 ID。
const ACTIVE_SLOT_ID: u64 = u64::MAX;

impl AppState {
    /// 渲染第 `page` 页右侧的页边笺注栏（含该页已钉住的紧凑笺注卡与激活态查词卡）。
    ///
    /// 若该页既没有已钉住的笺注、也不是当前激活态查词卡所在页，则返回 `None`。
    ///
    /// 卡片高度优先用上一帧的实测值（[`Self::gloss_card_height`]），首帧才退回估算。
    /// 所有卡片都放在一个 `overflow_hidden` 的裁剪层里：即使整列确实放不下，
    /// 也不会越过页底盖到下一页的笺注栏上。
    pub(crate) fn page_gloss_rail(
        &self,
        page: usize,
        layout: &DocumentLayout,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let geometry = layout.page_box(page)?;
        let active_here = self.dictionary.open && self.dictionary.anchor_page == page;
        let active_word = self.dictionary.word.trim();

        // 若某个已钉住词条正好在当前页处于激活展开态，不重复渲染折叠态小卡，
        // 由激活态查词卡右上角的「已钉」徽章直接表达其钉住状态。
        let pinned: Vec<&PinnedGloss> = self
            .viewer
            .highlights
            .glosses_for_page(page)
            .into_iter()
            .filter(|gloss| {
                !(active_here
                    && !active_word.is_empty()
                    && gloss.word.trim().eq_ignore_ascii_case(active_word))
            })
            .collect();

        if pinned.is_empty() && !active_here {
            return None;
        }

        let zoom = self.viewer.zoom;
        let pinned_heights: Vec<f32> = pinned
            .iter()
            .map(|gloss| {
                self.gloss_card_height(gloss.id, PINNED_CARD_ESTIMATED_HEIGHT)
                    .min(geometry.height)
            })
            .collect();
        // 激活卡最多占「页高 - 已钉卡」的剩余空间，保证两张卡不会互相压住。
        // 空间实在不够时保留一个最小可用高度，由裁剪层兜底。
        let pinned_extent = pinned_heights.iter().sum::<f32>()
            + GLOSS_CARD_GAP * pinned_heights.len().saturating_sub(1) as f32;
        let active_max_height = (geometry.height - pinned_extent - GLOSS_CARD_GAP)
            .max(MIN_ACTIVE_CARD_HEIGHT)
            .min(geometry.height);

        let mut slot_inputs = Vec::with_capacity(pinned.len() + usize::from(active_here));
        for (gloss, height) in pinned.iter().zip(pinned_heights.iter().copied()) {
            slot_inputs.push(GlossSlotInput {
                id: gloss.id,
                desired_top: (gloss.anchor_top * zoom).clamp(0.0, geometry.height),
                height,
            });
        }
        if active_here {
            let height = self
                .gloss_card_height(ACTIVE_SLOT_ID, active_max_height)
                .min(active_max_height)
                .max(1.0);
            slot_inputs.push(GlossSlotInput {
                id: ACTIVE_SLOT_ID,
                desired_top: (self.dictionary.anchor_top * zoom).clamp(0.0, geometry.height),
                height,
            });
        }

        let slots = layout_gloss_slots(&slot_inputs, geometry.height);

        // 视口放得下「纸张 + 栏」时栏在预留位；手动放大超出视口或视口太窄时，
        // 把整张卡夹回当前可见视口右侧内，确保笺注栏始终可见、不跑出窗口外。
        let viewport_width = f32::from(self.viewport.width);
        let scroll_left = (-f32::from(self.viewer.scroll.offset().x)).max(0.0);
        let max_rail_left = (scroll_left + viewport_width - GLOSS_RAIL_WIDTH - PAGE_PAD)
            .max(scroll_left + PAGE_PAD);
        let rail_left = geometry.rail_left().min(max_rail_left);
        // 卡片被夹回纸张上方时，引线会横穿正文，这时不画引线。
        let connected = rail_left + 0.5 >= geometry.rail_left();

        let mut rail = div()
            .absolute()
            .left(px(rail_left))
            .top(px(geometry.top))
            .w(px(GLOSS_RAIL_WIDTH))
            .h(px(geometry.height));
        let mut cards = div()
            .absolute()
            .left_0()
            .top_0()
            .w(px(GLOSS_RAIL_WIDTH))
            .h(px(geometry.height))
            .overflow_hidden();

        let view = cx.entity().downgrade();

        for slot in slots {
            if slot.id == ACTIVE_SLOT_ID {
                if connected {
                    let anchor_y = (self.dictionary.anchor_top * zoom + 10.0)
                        .clamp(8.0, geometry.height - 8.0);
                    rail = rail.child(gloss_connector(
                        anchor_y,
                        slot.top + 18.0,
                        theme.accent.opacity(0.55),
                    ));
                }
                cards = cards.child(measured_card(
                    view.clone(),
                    ACTIVE_SLOT_ID,
                    slot.top,
                    self.active_gloss_card(active_max_height, theme, cx),
                ));
            } else if let Some(gloss) = pinned.iter().copied().find(|g| g.id == slot.id) {
                if connected {
                    let anchor_y =
                        (gloss.anchor_top * zoom + 10.0).clamp(8.0, geometry.height - 8.0);
                    rail = rail.child(gloss_connector(
                        anchor_y,
                        slot.top + 16.0,
                        theme.accent.opacity(0.28),
                    ));
                }
                cards = cards.child(measured_card(
                    view.clone(),
                    gloss.id,
                    slot.top,
                    self.pinned_gloss_card(gloss, theme, cx),
                ));
            }
        }

        Some(rail.child(cards))
    }

    /// 上一帧实测到的卡片高度；没有测量结果时用 `fallback`。
    fn gloss_card_height(&self, id: u64, fallback: f32) -> f32 {
        self.gloss_card_heights
            .get(&id)
            .copied()
            .filter(|height| height.is_finite() && *height > 0.0)
            .unwrap_or(fallback)
    }

    /// 记录一张卡片的真实高度；变化时安排下一帧按新高度重新避让。
    fn record_gloss_card_height(&mut self, id: u64, height: f32, cx: &mut Context<Self>) {
        if !height.is_finite() || height <= 0.0 {
            return;
        }
        if self
            .gloss_card_heights
            .get(&id)
            .is_some_and(|measured| (*measured - height).abs() < 0.5)
        {
            return;
        }
        self.gloss_card_heights.insert(id, height);
        cx.notify();
    }

    /// 紧凑态页边笺注卡：展示词头、音标与 1~2 行核心释义摘要，点击展开为完整查词卡。
    fn pinned_gloss_card(
        &self,
        gloss: &PinnedGloss,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let id = gloss.id;
        let word = gloss.word.clone();
        let phonetic = gloss.phonetic.clone();
        let summary = gloss.summary.clone();
        let hover_bg = theme.surface_hover;

        let mut header = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w(px(0.))
            .child(
                div()
                    .w(px(3.))
                    .h(px(12.))
                    .rounded_full()
                    .flex_shrink_0()
                    .bg(theme.accent),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .truncate()
                    .child(word),
            );

        if let Some(phonetic) = phonetic.filter(|p| !p.is_empty()) {
            header = header.child(
                div()
                    .text_size(px(11.))
                    .font_family(theme.mono_font.clone())
                    .text_color(theme.text_muted)
                    .truncate()
                    .child(format!("/{phonetic}/")),
            );
        }

        header = header.child(
            div()
                .id(ElementId::NamedInteger("gloss-unpin".into(), id))
                .ml_auto()
                .flex_shrink_0()
                .px(px(5.))
                .py(px(1.))
                .rounded_md()
                .text_size(px(11.))
                .text_color(theme.text_muted)
                .hover(move |style| style.bg(hover_bg))
                .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.delete_highlight(id, cx);
                }))
                .child("✕"),
        );

        div()
            .id(ElementId::NamedInteger("gloss-pinned-card".into(), id))
            .w(px(GLOSS_RAIL_WIDTH))
            .px_3()
            .py_2()
            .rounded_lg()
            .bg(theme.surface.opacity(0.96))
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .occlude()
            .cursor_pointer()
            .hover(move |style| style.bg(hover_bg))
            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                this.open_pinned_gloss(id, cx);
            }))
            .child(header)
            .child(
                div()
                    .mt(px(4.))
                    .text_size(px(12.))
                    .line_height(relative(1.4))
                    .text_color(theme.text_muted)
                    .child(summary),
            )
    }

    /// 激活态页边查词卡：包含标题栏（返回/最近/钉住/关闭）、手动查词框、前缀建议、历史与完整 MDX 释义。
    ///
    /// `max_height` 是排版为它预留的最大高度：卡片自身也被限制在这个高度内，
    /// 释义区改为内部滚动，避免真实高度超出估算后压住下面的已钉卡片。
    fn active_gloss_card(
        &self,
        max_height: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let max_body_h = (max_height - 150.0).clamp(60.0, 320.0);
        let mut body = div()
            .id("gloss-active-body-scroll")
            .max_h(px(max_body_h))
            .min_h(px(0.))
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col();

        match &self.dictionary.load_state {
            DictLoadState::Unconfigured => {
                body = body
                    .child(muted("还没有配置词典", theme))
                    .child(px_gap(6.))
                    .child(muted("请在主窗口的设置中选择 .mdx 文件。", theme));
            }
            DictLoadState::Loading => {
                body = body.child(muted("正在加载词典…", theme));
            }
            DictLoadState::Failed(message) => {
                body = body
                    .child(muted("词典加载失败", theme))
                    .child(error_text(message, theme));
            }
            DictLoadState::Ready { title, entries } => {
                if self.dictionary.loading {
                    body = body.child(muted("正在查询…", theme));
                } else if let Some(error) = &self.dictionary.error {
                    body = body.child(error_text(error, theme));
                } else if self.dictionary.not_found {
                    body = body.child(muted(format!("未找到「{}」", self.dictionary.word), theme));
                } else if self.dictionary.entries.is_empty() {
                    body = body.child(muted("双击 PDF 中的单词或汉字开始查词", theme));
                } else {
                    for (entry_index, entry) in self.dictionary.entries.iter().enumerate() {
                        body = body.child(entry_element(
                            entry,
                            theme,
                            &self.dict_query,
                            mix_seed(0x9e37_79b9_7f4a_7c15, entry_index),
                        ));
                    }
                }

                if self.dictionary.entries.is_empty() {
                    body = body.child(px_gap(10.)).child(muted(
                        match title {
                            Some(title) => format!("词典：{title} · {entries} 条"),
                            None => format!("词典 · {entries} 条"),
                        },
                        theme,
                    ));
                }
            }
        }

        let mut card = div()
            .id("gloss-active-card")
            .w(px(GLOSS_RAIL_WIDTH))
            .max_h(px(max_height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded_lg()
            .bg(theme.surface.opacity(0.98))
            .border_1()
            .border_color(theme.accent.opacity(0.55))
            .shadow_lg()
            .occlude()
            .on_scroll(
                cx.listener(|_this, event: &ScrollEvent, _window: &mut Window, cx| {
                    // 词典释义区滚轮独立滚动，不带动外层 PDF 页面。
                    if !event.modifiers.control {
                        cx.stop_propagation();
                    }
                }),
            )
            .child(self.gloss_card_header(theme, cx))
            .child(self.gloss_query_row(theme, cx));

        if !self.dictionary.suggestions.is_empty() {
            card = card.child(self.gloss_suggestions_row(theme, cx));
        }

        let show_history = (self.dictionary.show_history || self.dictionary.entries.is_empty())
            && !self.dict_history.entries().is_empty();
        if show_history {
            card = card.child(self.gloss_history_row(theme, cx));
        }

        card.child(body)
    }

    /// 激活态查词卡顶栏：左侧词头与跳转返回，右侧「最近」「钉住/已钉」与关闭按钮。
    fn gloss_card_header(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let hover_bg = theme.surface_hover;
        let word_label = if self.dictionary.word.is_empty() {
            "笺注".to_string()
        } else {
            format!("笺注 · {}", self.dictionary.word)
        };

        let is_pinned = !self.dictionary.word.trim().is_empty()
            && self
                .viewer
                .highlights
                .find_gloss(self.dictionary.anchor_page, &self.dictionary.word)
                .is_some();
        let can_pin =
            !self.dictionary.word.trim().is_empty() && !self.dictionary.entries.is_empty();

        let mut header = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(36.))
            .px_3()
            .border_b_1()
            .border_color(theme.border);

        if !self.dictionary.back_stack.is_empty() {
            header = header.child(
                div()
                    .id("gloss-back")
                    .flex_shrink_0()
                    .px(px(6.))
                    .py(px(2.))
                    .rounded_md()
                    .text_size(px(11.))
                    .text_color(theme.accent)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover_bg))
                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.navigate_dict_back(cx);
                    }))
                    .child("‹ 返回"),
            );
        }

        header = header.child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(word_label),
        );

        if !self.dict_history.entries().is_empty() {
            let history_active = self.dictionary.show_history;
            let history_btn = div()
                .id("gloss-history-toggle")
                .flex_shrink_0()
                .px(px(6.))
                .py(px(2.))
                .rounded_md()
                .text_size(px(11.))
                .cursor_pointer()
                .text_color(if history_active {
                    theme.accent
                } else {
                    theme.text_muted
                })
                .hover(move |style| style.bg(hover_bg))
                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.dictionary.show_history = !this.dictionary.show_history;
                    cx.notify();
                }))
                .child("最近");
            header = header.child(history_btn);
        }

        if can_pin || is_pinned {
            let pin_label = if is_pinned { "已钉" } else { "钉住" };
            let mut pin_btn = div()
                .id("gloss-pin-toggle")
                .flex_shrink_0()
                .px(px(7.))
                .py(px(2.))
                .rounded_md()
                .text_size(px(11.))
                .cursor_pointer()
                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.toggle_pin_current_gloss(cx);
                }))
                .child(pin_label);

            if is_pinned {
                pin_btn = pin_btn
                    .bg(theme.accent.opacity(0.18))
                    .text_color(theme.accent)
                    .font_weight(FontWeight::SEMIBOLD);
            } else {
                pin_btn = pin_btn
                    .text_color(theme.text_muted)
                    .hover(move |style| style.bg(hover_bg));
            }
            header = header.child(pin_btn);
        }

        header.child(
            div()
                .id("gloss-close")
                .flex_shrink_0()
                .px(px(6.))
                .py(px(2.))
                .rounded_md()
                .cursor_pointer()
                .text_size(px(11.))
                .text_color(theme.text_muted)
                .hover(move |style| style.bg(hover_bg))
                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.close_dictionary(cx);
                }))
                .child("✕"),
        )
    }

    /// 查词输入行：手动输入 + 「查」按钮。
    fn gloss_query_row(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let hover = theme.accent_hover;

        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .h(px(26.))
                    .px_2()
                    .rounded_md()
                    .bg(theme.bg)
                    .border_1()
                    .border_color(theme.border)
                    .child(self.dict_input.clone()),
            )
            .child(
                div()
                    .id("dictionary-submit")
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .h(px(26.))
                    .px_3()
                    .rounded_md()
                    .bg(theme.accent)
                    .text_color(theme.accent_text)
                    .text_size(px(12.))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.submit_dict_query(cx);
                    }))
                    .child("查"),
            )
    }

    /// 前缀建议：跨词典、去重、最多 8 条，点击填入并查询。
    fn gloss_suggestions_row(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(11.));

        row = row.child(div().text_color(theme.text_muted).child("建议"));
        for (index, term) in self.dictionary.suggestions.clone().into_iter().enumerate() {
            let id = ElementId::NamedInteger("dict-suggestion".into(), index as u64);
            let label = term.clone();
            row = row.child(
                div()
                    .id(id)
                    .max_w(px(160.))
                    .truncate()
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .bg(theme.bg)
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover({
                        let hover = theme.accent_hover;
                        move |style| style.bg(hover)
                    })
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        this.query_dict_term(label.clone(), cx);
                    }))
                    .child(term),
            );
        }
        row
    }

    /// 查词历史：最新的在前，点击重查；右侧可一键清空。
    fn gloss_history_row(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(11.));

        row = row.child(div().text_color(theme.text_muted).child("最近"));
        for (index, term) in self.dict_history.entries().iter().cloned().enumerate() {
            let id = ElementId::NamedInteger("dict-history".into(), index as u64);
            let label = term.clone();
            row = row.child(
                div()
                    .id(id)
                    .max_w(px(130.))
                    .truncate()
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .bg(theme.bg)
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover({
                        let hover = theme.accent_hover;
                        move |style| style.bg(hover)
                    })
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        this.query_dict_term(label.clone(), cx);
                    }))
                    .child(term),
            );
        }

        row.child(
            div()
                .id("dict-history-clear")
                .ml_auto()
                .px_2()
                .py(px(2.))
                .rounded_md()
                .text_color(theme.text_muted)
                .cursor_pointer()
                .hover({
                    let hover = theme.accent_hover;
                    move |style| style.bg(hover)
                })
                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.clear_dict_history(cx);
                }))
                .child("清空"),
        )
    }
}

/// 把卡片包进一个只负责定位与测量的容器。
///
/// `on_children_prepainted` 在真正的绘制阶段拿到卡片的实际边界，下一帧排版
/// 就能用真实高度避让；不再依赖 88/340 这种容易失真的常量。
fn measured_card(
    view: gpui::WeakEntity<AppState>,
    id: u64,
    top: f32,
    card: impl IntoElement,
) -> Div {
    div()
        .absolute()
        .left_0()
        .top(px(top))
        .w(px(GLOSS_RAIL_WIDTH))
        .on_children_prepainted(move |bounds, _window, cx| {
            let Some(bounds) = bounds.first() else {
                return;
            };
            let height = f32::from(bounds.size.height);
            let _ = view.update(cx, |this, cx| this.record_gloss_card_height(id, height, cx));
        })
        .child(card)
}

/// 纸张右缘到页边笺注卡之间的引线标记：当卡片因避让发生纵向偏移时，用短折线保持锚点指向。
///
/// 纯装饰元素：不要给它加 `id`、`cursor`、点击监听或 `occlude()`，否则 GPUI 会为它
/// 插入 hitbox，左缘伸进纸张与栏之间 14px 缝隙的那一段就会挡住页面点击。
fn gloss_connector(anchor_y: f32, card_y: f32, color: Rgba) -> Div {
    let top = anchor_y.min(card_y);
    let height = (anchor_y - card_y).abs().max(1.5);
    div()
        .absolute()
        .left(px(-GLOSS_RAIL_GAP + 2.0))
        .top(px(top))
        .w(px(GLOSS_RAIL_GAP - 2.0))
        .h(px(height))
        .border_t_1()
        .border_l_1()
        .border_color(color)
}

pub(crate) fn entry_element(entry: &DictEntry, theme: &Theme, lookup: &LookupSender, seed: u64) -> Div {
    let mut column = div().flex().flex_col().mb(px(12.)).min_w(px(0.)).w_full();

    if let Some(source) = &entry.source {
        column = column.child(
            div()
                .mb(px(2.))
                .text_size(px(10.))
                .text_color(theme.accent)
                .child(format!("来源 · {source}")),
        );
    }

    // 很多词典把词头写进正文（`<h1>` / `.hwd`），这里避免再叠一行。
    let duplicate_headword = entry.blocks.first().is_some_and(|block| {
        let text = block.text();
        let text = text.trim_start();
        let headword = entry.headword.trim();
        !headword.is_empty()
            && text.starts_with(headword)
            && text.chars().count() <= headword.chars().count() + 24
    });
    if !duplicate_headword {
        column = column.child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(entry.headword.clone()),
        );
    }

    if let Some(phonetic) = &entry.phonetic {
        column = column.child(
            div()
                .mt(px(2.))
                .text_size(px(12.))
                .font_family(theme.mono_font.clone())
                .text_color(theme.text_muted)
                .child(phonetic.clone()),
        );
    }

    if entry.blocks.is_empty() {
        column = column.child(px_gap(4.)).child(muted("（无释义）", theme));
    } else {
        for (block_index, block) in entry.blocks.iter().enumerate() {
            column = column.child(block_element(
                block,
                theme,
                lookup,
                mix_seed(seed, block_index),
            ));
        }
    }

    column
}

fn block_element(block: &Block, theme: &Theme, lookup: &LookupSender, seed: u64) -> AnyElement {
    match block {
        Block::Paragraph(paragraph) => paragraph_element(paragraph, theme, lookup, seed),
        Block::Table(table) => table_element(table, theme, lookup, seed).into_any_element(),
        Block::Container(container) => {
            container_element(container, theme, lookup, seed).into_any_element()
        }
    }
}

fn paragraph_element(
    paragraph: &Paragraph,
    theme: &Theme,
    lookup: &LookupSender,
    seed: u64,
) -> AnyElement {
    let style = &paragraph.style;
    if paragraph.kind == BlockKind::Rule {
        return div()
            .w_full()
            .h(px(1.))
            .my(px(8.))
            .bg(theme.border)
            .into_any_element();
    }

    let size = paragraph_size(paragraph);
    let color = style
        .color
        .map(hsla)
        .unwrap_or_else(|| default_color(paragraph.kind, theme));
    let align = style.align.map(text_align).unwrap_or(TextAlign::Left);
    let block_font = make_font(style.family.as_deref(), theme);
    let margin_top = style
        .margin_top
        .unwrap_or_else(|| default_margin_top(paragraph.kind));
    let margin_bottom = style.margin_bottom.unwrap_or(0.0);

    let mut element = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_baseline()
        .w_full()
        .min_w(px(0.))
        .mt(px(margin_top))
        .mb(px(margin_bottom));
    element = styled_box(element, style, theme);
    element = element
        .text_size(px(size))
        .text_color(color)
        .font(block_font.clone())
        .line_height(relative(style.line_height.unwrap_or(1.55)))
        .text_align(align);

    if matches!(style.float, Some(FloatSide::Right)) {
        element = element.justify_end();
    }

    if paragraph.kind == BlockKind::Quote {
        element = element
            .pl(px(style.padding.left + style.indent + 8.0))
            .border_l(px(2.0))
            .border_color(theme.border);
    }

    if let Some(marker) = &style.marker {
        element = element.child(
            div()
                .flex_shrink_0()
                .mr(px(4.0))
                .text_color(color)
                .child(marker.clone()),
        );
    }

    let has_image = paragraph
        .spans
        .iter()
        .any(|span| matches!(span, Span::Image(_)));
    if has_image || needs_chunk_layout(paragraph) {
        for (span_index, span) in paragraph.spans.iter().enumerate() {
            match span {
                Span::Text(text) => {
                    for (chunk_index, chunk) in wrap_chunks(&text.text).into_iter().enumerate() {
                        element = element.child(chunk_element(
                            chunk,
                            &text.style,
                            size,
                            color,
                            &block_font,
                            theme,
                            lookup,
                            mix_seed(mix_seed(seed, span_index), chunk_index),
                        ));
                    }
                }
                Span::Image(image) => {
                    element = element.child(image_element(image));
                }
                Span::Box(inline) => {
                    element = element.child(inline_box_element(
                        inline,
                        theme,
                        lookup,
                        mix_seed(seed, span_index),
                    ));
                }
            }
        }
    } else {
        let mut text = String::new();
        let mut runs = Vec::new();
        let mut links: Vec<(std::ops::Range<usize>, &LinkTarget)> = Vec::new();
        for span in &paragraph.spans {
            let Span::Text(span) = span else {
                continue;
            };
            if span.text.is_empty() {
                continue;
            }
            let start = text.len();
            text.push_str(&span.text);
            runs.push(text_run(
                span.text.len(),
                &span.style,
                &block_font,
                color,
                theme,
            ));
            if let Some(link) = &span.style.link {
                links.push((start..text.len(), link));
            }
        }
        if !text.is_empty() {
            let styled = StyledText::new(text.clone()).with_runs(runs);
            let content: AnyElement = if links.is_empty() {
                styled.into_any_element()
            } else {
                link_text(&text, styled, links, lookup, seed)
            };
            element = element.child(div().flex_1().min_w(px(0.)).child(content));
        }
    }

    element.into_any_element()
}

/// 把带链接的文本包成 `InteractiveText`：悬停变手型，点击查词或开浏览器。
fn link_text(
    _text: &str,
    styled: StyledText,
    links: Vec<(std::ops::Range<usize>, &LinkTarget)>,
    lookup: &LookupSender,
    seed: u64,
) -> AnyElement {
    let mut ranges = Vec::with_capacity(links.len());
    let mut targets = Vec::with_capacity(links.len());
    for (range, target) in links {
        ranges.push(range);
        targets.push(target.clone());
    }
    let lookup = lookup.clone();
    InteractiveText::new(ElementId::NamedInteger("dict-link".into(), seed), styled)
        .on_click(ranges, move |index, _window, cx| {
            let Some(target) = targets.get(index) else {
                return;
            };
            match target {
                LinkTarget::Entry(term) => {
                    if lookup
                        .unbounded_send(DictRequest::Lookup(term.clone()))
                        .is_err()
                    {
                        log::warn!("dictionary lookup channel closed");
                    }
                }
                LinkTarget::Uri(uri) => cx.open_url(uri),
            }
        })
        .into_any_element()
}

/// 把父节点的稳定种子和子节点下标混成一个新种子，保证同一窗口内 id 唯一。
fn mix_seed(seed: u64, index: usize) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    index.hash(&mut hasher);
    hasher.finish()
}

fn table_element(table: &Table, theme: &Theme, lookup: &LookupSender, seed: u64) -> Div {
    let mut root = div().flex().flex_col().w_full().min_w(px(0.));
    root = root
        .mt(px(table.style.margin_top.unwrap_or(6.0)))
        .mb(px(table.style.margin_bottom.unwrap_or(0.0)));
    root = styled_box(root, &table.style, theme);

    for (row_index, row) in table.rows.iter().enumerate() {
        let row_seed = mix_seed(seed, row_index);
        let mut row_element = div()
            .flex()
            .flex_row()
            .items_stretch()
            .w_full()
            .min_w(px(0.))
            .mt(px(row.style.margin_top.unwrap_or(0.0)))
            .mb(px(row.style.margin_bottom.unwrap_or(0.0)));
        row_element = styled_box(row_element, &row.style, theme);
        for (cell_index, cell) in row.cells.iter().enumerate() {
            row_element = row_element.child(cell_element(
                cell,
                theme,
                lookup,
                mix_seed(row_seed, cell_index),
            ));
        }
        root = root.child(row_element);
    }

    root
}

fn cell_element(cell: &TableCell, theme: &Theme, lookup: &LookupSender, seed: u64) -> Div {
    let mut style = cell.style.clone();
    style.padding.top += 3.0;
    style.padding.bottom += 3.0;
    style.padding.left += 4.0;
    style.padding.right += 4.0;

    let mut element = div().flex().flex_col().min_w(px(0.));
    if cell.colspan > 1 {
        element = element.flex_grow(cell.colspan as f32);
    } else {
        element = element.flex_1();
    }
    if let Some(width) = style.width {
        element = element.flex_none().w(px(width));
    }
    element = styled_box(element, &style, theme);

    let size = style.size.unwrap_or(13.0);
    let color = style.color.map(hsla).unwrap_or(theme.text.into());
    let block_font = make_font(style.family.as_deref(), theme);
    element = element
        .text_size(px(size))
        .text_color(color)
        .font(block_font)
        .line_height(relative(style.line_height.unwrap_or(1.5)));
    if let Some(align) = style.align {
        element = element.text_align(text_align(align));
    }

    for (block_index, block) in cell.blocks.iter().enumerate() {
        element = element.child(block_element(
            block,
            theme,
            lookup,
            mix_seed(seed, block_index),
        ));
    }
    element
}

/// `display: flex` 容器：直接映射到 GPUI 的 flex 布局。
fn container_element(
    container: &Container,
    theme: &Theme,
    lookup: &LookupSender,
    seed: u64,
) -> Div {
    let flex = container.flex;
    let is_row = flex.direction.is_row();

    let mut root = div().flex().w_full().min_w(px(0.));
    root = match flex.direction {
        FlexDirection::Row => root.flex_row(),
        FlexDirection::RowReverse => root.flex_row_reverse(),
        FlexDirection::Column => root.flex_col(),
        FlexDirection::ColumnReverse => root.flex_col_reverse(),
    };
    root = match flex.wrap {
        FlexWrap::NoWrap => root,
        FlexWrap::Wrap => root.flex_wrap(),
        FlexWrap::WrapReverse => root.flex_wrap_reverse(),
    };
    root = match flex.justify {
        Justify::Start => root.justify_start(),
        Justify::End => root.justify_end(),
        Justify::Center => root.justify_center(),
        Justify::SpaceBetween => root.justify_between(),
        Justify::SpaceAround => root.justify_around(),
        Justify::SpaceEvenly => root.justify_evenly(),
    };
    root = match flex.align_items {
        AlignItems::Start => root.items_start(),
        AlignItems::End => root.items_end(),
        AlignItems::Center => root.items_center(),
        AlignItems::Baseline => root.items_baseline(),
        AlignItems::Stretch => root.items_stretch(),
    };
    if flex.gap > 0.0 {
        root = root.gap(px(flex.gap));
    }
    root = root
        .mt(px(container.style.margin_top.unwrap_or(6.0)))
        .mb(px(container.style.margin_bottom.unwrap_or(0.0)));
    root = styled_box(root, &container.style, theme);

    // 双栏对照几乎都不写宽度：横向且没人声明尺寸时，让子项均分宽度，
    // 否则文本会按内容宽度排，右栏可能被挤到看不见。
    let sized = container.items.iter().any(|item| {
        item.style.grow.is_some() || item.style.width.is_some() || item.style.basis.is_some()
    });

    for (item_index, item) in container.items.iter().enumerate() {
        let item_seed = mix_seed(seed, item_index);
        let mut element = div().flex().flex_col().min_w(px(0.));
        if let Some(grow) = item.style.grow {
            element = element.flex_grow(grow);
        } else if is_row && !sized {
            element = element.flex_1();
        }
        if let Some(shrink) = item.style.shrink {
            element = element.flex_shrink(shrink);
        }
        if let Some(basis) = item.style.basis {
            element = element.flex_basis(px(basis));
        }
        if let Some(width) = item.style.width {
            element = element.flex_none().w(px(width));
        }
        element = match item.style.align_self {
            None | Some(AlignSelf::Auto) => element,
            Some(AlignSelf::Start) => element.self_start(),
            Some(AlignSelf::End) => element.self_end(),
            Some(AlignSelf::Center) => element.self_center(),
            Some(AlignSelf::Baseline) => element.self_baseline(),
            Some(AlignSelf::Stretch) => element.self_stretch(),
        };
        element = element
            .mt(px(item.style.margin_top.unwrap_or(0.0)))
            .mb(px(item.style.margin_bottom.unwrap_or(0.0)));
        element = styled_box(element, &item.style, theme);
        element = element
            .text_size(px(item.style.size.unwrap_or(13.0)))
            .text_color(item.style.color.map(hsla).unwrap_or(theme.text.into()))
            .font(make_font(item.style.family.as_deref(), theme))
            .line_height(relative(item.style.line_height.unwrap_or(1.55)));

        for (block_index, block) in item.blocks.iter().enumerate() {
            element = element.child(block_element(
                block,
                theme,
                lookup,
                mix_seed(item_seed, block_index),
            ));
        }
        root = root.child(element);
    }

    root
}

fn chunk_element(
    chunk: String,
    style: &SpanStyle,
    block_size: f32,
    block_color: Hsla,
    block_font: &Font,
    theme: &Theme,
    lookup: &LookupSender,
    seed: u64,
) -> AnyElement {
    let size = style.size.unwrap_or(block_size);
    let color = span_color(style, block_color, theme);
    let font = span_font(style, block_font, theme);

    let mut element = div()
        .min_w(px(0.))
        .text_size(px(size))
        .text_color(color)
        .font(font);
    if span_underlined(style) {
        element = element.underline();
        if let Some(underline) = style.underline_color {
            element = element.text_decoration_color(hsla(underline));
        }
    }
    if style.strike {
        element = element.line_through();
    }
    if let Some(background) = style.background {
        element = element.text_bg(hsla(background));
    }
    let element = element.child(chunk.clone());

    match &style.link {
        Some(target) => {
            let lookup = lookup.clone();
            let target = target.clone();
            element
                .id(ElementId::NamedInteger("dict-chunk-link".into(), seed))
                .cursor_pointer()
                .on_click(
                    move |_event: &gpui::ClickEvent, _window, cx| match &target {
                        LinkTarget::Entry(term) => {
                            if lookup
                                .unbounded_send(DictRequest::Lookup(term.clone()))
                                .is_err()
                            {
                                log::warn!("dictionary lookup channel closed");
                            }
                        }
                        LinkTarget::Uri(uri) => cx.open_url(uri),
                    },
                )
                .into_any_element()
        }
        None => element.into_any_element(),
    }
}

fn text_run(
    len: usize,
    style: &SpanStyle,
    block_font: &Font,
    block_color: Hsla,
    theme: &Theme,
) -> TextRun {
    let color = span_color(style, block_color, theme);
    let underline = span_underlined(style).then(|| UnderlineStyle {
        thickness: px(1.0),
        color: Some(style.underline_color.map(hsla).unwrap_or(color)),
        wavy: false,
    });
    let strikethrough = style.strike.then(|| StrikethroughStyle {
        thickness: px(1.0),
        color: Some(color),
    });
    TextRun {
        len,
        font: span_font(style, block_font, theme),
        color,
        background_color: style.background.map(hsla),
        underline,
        strikethrough,
    }
}

fn image_element(image: &ImageSpan) -> AnyElement {
    let format = match image.format {
        DictImageFormat::Png => ImageFormat::Png,
        DictImageFormat::Jpeg => ImageFormat::Jpeg,
        DictImageFormat::Gif => ImageFormat::Gif,
        DictImageFormat::Webp => ImageFormat::Webp,
        DictImageFormat::Bmp => ImageFormat::Bmp,
    };
    let source = Image::from_bytes(format, image.data.to_vec());
    let (width, height) = image_size(image);
    img(Arc::new(source))
        .w(px(width))
        .h(px(height))
        .max_w_full()
        .object_fit(ObjectFit::Contain)
        .into_any_element()
}

/// `display:inline-block` 的原子盒子：保留宽高、内外边距、边框与垂直对齐。
fn inline_box_element(
    inline: &InlineBox,
    theme: &Theme,
    lookup: &LookupSender,
    seed: u64,
) -> AnyElement {
    let style = &inline.style;
    let mut element = div().flex().flex_col().flex_shrink_0().min_w(px(0.));
    element = styled_box(element, style, theme);
    if let Some(width) = style.width {
        element = element.w(px(width));
    }
    if let Some(height) = style.height {
        element = element.h(px(height));
    }
    if let Some(margin) = style.margin_left {
        element = element.ml(px(margin));
    }
    if let Some(margin) = style.margin_right {
        element = element.mr(px(margin));
    }
    if let Some(margin) = style.margin_top {
        element = element.mt(px(margin));
    }
    if let Some(margin) = style.margin_bottom {
        element = element.mb(px(margin));
    }
    if let Some(align) = style.vertical_align {
        element = match align {
            AlignSelf::Start => element.self_start(),
            AlignSelf::Center => element.self_center(),
            AlignSelf::End => element.self_end(),
            AlignSelf::Baseline => element.self_baseline(),
            AlignSelf::Stretch | AlignSelf::Auto => element,
        };
    }
    for (block_index, block) in inline.blocks.iter().enumerate() {
        element = element.child(block_element(
            block,
            theme,
            lookup,
            mix_seed(seed, block_index),
        ));
    }
    element.into_any_element()
}

fn image_size(image: &ImageSpan) -> (f32, f32) {
    let natural_width = image.width.max(1) as f32;
    let natural_height = image.height.max(1) as f32;
    let (mut width, mut height) = match (image.display_width, image.display_height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, width * natural_height / natural_width),
        (None, Some(height)) => (height * natural_width / natural_height, height),
        (None, None) => (natural_width, natural_height),
    };
    const MAX_WIDTH: f32 = 520.0;
    const MAX_HEIGHT: f32 = 360.0;
    let scale = (MAX_WIDTH / width).min(MAX_HEIGHT / height).min(1.0);
    if scale < 1.0 {
        width *= scale;
        height *= scale;
    }
    (width.max(1.0), height.max(1.0))
}

fn styled_box(mut element: Div, style: &BlockStyle, theme: &Theme) -> Div {
    element = element
        .pt(px(style.padding.top))
        .pr(px(style.padding.right))
        .pb(px(style.padding.bottom))
        .pl(px(style.padding.left + style.indent.max(0.0)));
    if let Some(background) = style.background {
        element = element.bg(hsla(background));
    }

    let mut has_border = false;
    let mut color = None;
    if let Some(border) = style.border_top.filter(|border| border.width > 0.0) {
        element = element.border_t(px(border.width));
        has_border = true;
        color = color.or(border.color);
    }
    if let Some(border) = style.border_right.filter(|border| border.width > 0.0) {
        element = element.border_r(px(border.width));
        has_border = true;
        color = color.or(border.color);
    }
    if let Some(border) = style.border_bottom.filter(|border| border.width > 0.0) {
        element = element.border_b(px(border.width));
        has_border = true;
        color = color.or(border.color);
    }
    if let Some(border) = style.border_left.filter(|border| border.width > 0.0) {
        element = element.border_l(px(border.width));
        has_border = true;
        color = color.or(border.color);
    }
    if has_border {
        element = element.border_color(color.map(hsla).unwrap_or_else(|| theme.border.into()));
    }
    element
}

fn muted(text: impl Into<String>, theme: &Theme) -> Div {
    div()
        .text_size(px(12.))
        .text_color(theme.text_muted)
        .child(text.into())
}

fn error_text(text: impl Into<String>, theme: &Theme) -> Div {
    div()
        .mt(px(4.))
        .text_size(px(12.))
        .text_color(theme.text_muted)
        .child(text.into())
}

fn px_gap(height: f32) -> Div {
    div().h(px(height))
}

fn hsla(color: DictColor) -> Hsla {
    Hsla::from(Rgba {
        r: color.r as f32 / 255.0,
        g: color.g as f32 / 255.0,
        b: color.b as f32 / 255.0,
        a: color.a as f32 / 255.0,
    })
}

fn map_family(name: &str, theme: &Theme) -> String {
    let trimmed = name.trim().trim_matches(['"', '\'']);
    match trimmed.to_ascii_lowercase().as_str() {
        "monospace" | "courier" | "courier new" | "consolas" => theme.mono_font.to_string(),
        "sans-serif" | "system-ui" => theme.font_family.to_string(),
        "serif" => "Times New Roman".to_string(),
        "宋体" | "simsun" => "SimSun".to_string(),
        "黑体" | "simhei" => "SimHei".to_string(),
        "楷体" | "kaiti" => "KaiTi".to_string(),
        "仿宋" | "fangsong" => "FangSong".to_string(),
        "微软雅黑" | "microsoft yahei" => "Microsoft YaHei".to_string(),
        _ => trimmed.to_string(),
    }
}

fn mapped_families(families: Option<&[String]>, theme: &Theme) -> Vec<String> {
    let mut output = Vec::new();
    if let Some(families) = families {
        for family in families {
            let mapped = map_family(family, theme);
            if !mapped.is_empty() && !output.iter().any(|existing| existing == &mapped) {
                output.push(mapped);
            }
        }
    }
    if output.is_empty() {
        output.push(theme.font_family.to_string());
    }
    output
}

fn make_font(families: Option<&[String]>, theme: &Theme) -> Font {
    let mapped = mapped_families(families, theme);
    Font {
        family: mapped[0].clone().into(),
        features: FontFeatures::default(),
        fallbacks: (mapped.len() > 1).then(|| FontFallbacks::from_fonts(mapped[1..].to_vec())),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

fn span_font(style: &SpanStyle, block_font: &Font, theme: &Theme) -> Font {
    let mut font = match &style.family {
        Some(family) => make_font(Some(std::slice::from_ref(family)), theme),
        None if style.mono => Font {
            family: theme.mono_font.clone(),
            ..block_font.clone()
        },
        None => block_font.clone(),
    };
    font.weight = if style.bold {
        FontWeight::BOLD
    } else {
        FontWeight::NORMAL
    };
    font.style = if style.italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    };
    font
}

/// 行内颜色：CSS 指定优先；链接在 CSS 完全没提装饰时用主题强调色。
fn span_color(style: &SpanStyle, block_color: Hsla, theme: &Theme) -> Hsla {
    if let Some(color) = style.color {
        return hsla(color);
    }
    if style.link.is_some() && !style.decorated {
        return theme.accent.into();
    }
    block_color
}

/// 链接默认带下划线，但 CSS 声明过 `text-decoration` 时以 CSS 为准
/// （许多词典用 `text-decoration: none` 让页码链接融入正文）。
fn span_underlined(style: &SpanStyle) -> bool {
    style.underline || (style.link.is_some() && !style.decorated)
}

fn text_align(align: Align) -> TextAlign {
    match align {
        Align::Left => TextAlign::Left,
        Align::Center => TextAlign::Center,
        Align::Right => TextAlign::Right,
    }
}

/// 段落字号：块级 CSS 优先；否则用行内样式的统一字号，避免行内字号被忽略。
fn paragraph_size(paragraph: &Paragraph) -> f32 {
    let default = default_size(paragraph.kind);
    if let Some(size) = paragraph.style.size {
        return size;
    }
    let mut size = None;
    for span in &paragraph.spans {
        let Span::Text(text) = span else {
            continue;
        };
        let Some(value) = text.style.size else {
            continue;
        };
        match size {
            None => size = Some(value),
            Some(existing) if (existing - value).abs() <= 0.25 => {}
            Some(_) => return default,
        }
    }
    size.unwrap_or(default)
}

/// 字号差得足够大（词头、音标对比正文）才逐片段渲染；
/// 普通行内字号微调仍走单段文本，换行更自然。
fn needs_chunk_layout(paragraph: &Paragraph) -> bool {
    let mut min = f32::INFINITY;
    let mut max = 0.0f32;
    if let Some(size) = paragraph.style.size {
        min = min.min(size);
        max = max.max(size);
    }
    for span in &paragraph.spans {
        if matches!(span, Span::Box(_)) {
            return true;
        }
        let Span::Text(text) = span else {
            continue;
        };
        if let Some(size) = text.style.size {
            min = min.min(size);
            max = max.max(size);
        }
    }
    min.is_finite() && max > 0.0 && max / min >= 1.6
}

fn default_size(kind: BlockKind) -> f32 {
    match kind {
        BlockKind::Heading(1) => 20.0,
        BlockKind::Heading(2) => 17.0,
        BlockKind::Heading(_) => 15.0,
        BlockKind::ListItem => 13.5,
        BlockKind::Quote => 13.5,
        BlockKind::Paragraph => 13.5,
        BlockKind::Rule => 1.0,
    }
}

fn default_margin_top(kind: BlockKind) -> f32 {
    match kind {
        BlockKind::Heading(_) => 10.0,
        BlockKind::ListItem => 2.0,
        BlockKind::Quote => 6.0,
        BlockKind::Paragraph => 6.0,
        BlockKind::Rule => 8.0,
    }
}

fn default_color(kind: BlockKind, theme: &Theme) -> Hsla {
    match kind {
        BlockKind::Quote => theme.text_muted.into(),
        _ => theme.text.into(),
    }
}

/// 把长片段切成小块，保证 flex 布局能在图片旁正常换行。
fn wrap_chunks(text: &str) -> Vec<String> {
    const MAX_UNITS: usize = 36;

    if text.is_empty() {
        return Vec::new();
    }
    if units(text) <= MAX_UNITS {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_units = 0;

    for ch in text.chars() {
        let width = if is_wide(ch) { 2 } else { 1 };
        if current_units + width > MAX_UNITS && !current.is_empty() {
            if let Some(index) = current.rfind(' ') {
                let tail = current.split_off(index + 1);
                chunks.push(std::mem::take(&mut current));
                current = tail;
                current_units = units(&current);
            } else {
                chunks.push(std::mem::take(&mut current));
                current_units = 0;
            }
        }
        current.push(ch);
        current_units += width;
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn units(text: &str) -> usize {
    text.chars().map(|ch| if is_wide(ch) { 2 } else { 1 }).sum()
}

fn is_wide(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x3FFFD
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(wrap_chunks("hello"), vec!["hello".to_string()]);
    }

    #[test]
    fn long_text_is_split() {
        let chunks = wrap_chunks(&"a".repeat(100));
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), "a".repeat(100));
    }

    #[test]
    fn long_text_splits_at_spaces_when_possible() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
        let chunks = wrap_chunks(text);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), text);
        assert!(
            chunks[..chunks.len() - 1]
                .iter()
                .any(|chunk| chunk.ends_with(' '))
        );
    }

    #[test]
    fn cjk_text_counts_as_two_units() {
        let chunks = wrap_chunks(&"你".repeat(30));
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), "你".repeat(30));
    }

    #[test]
    fn image_size_scales_large_images_down() {
        let image = ImageSpan {
            data: Arc::from(Vec::<u8>::new()),
            format: DictImageFormat::Png,
            width: 1200,
            height: 800,
            display_width: None,
            display_height: None,
            alt: String::new(),
        };
        let (width, height) = image_size(&image);
        assert!(width <= 520.0, "{width}");
        assert!((width / height - 1.5).abs() < 0.01);
    }
}
