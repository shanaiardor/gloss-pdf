//! 独立词典窗口：双击 PDF 查词后显示释义，不遮挡页面。
//!
//! 窗口没有系统标题栏：顶部自绘一条可拖拽的窄标题栏，拖到主窗口边缘时自动
//! 吸附。释义按 CSS 计算样式渲染：段落用 GPUI 的 [`StyledText`] 行内 run 画
//! 颜色/字重/下划线，块级属性落到 `div` 上；表格按行列布局，图片直接交给 GPUI 解码。

use std::sync::Arc;

use dict_core::{
    Align, AlignItems, AlignSelf, Block, BlockKind, BlockStyle, Color as DictColor, Container,
    DictEntry, FlexDirection, FlexWrap, ImageFormat as DictImageFormat, ImageSpan, Justify,
    LinkTarget, Paragraph, Span, SpanStyle, Table, TableCell,
};
use gpui::{
    AnyElement, Context, Div, ElementId, Entity, Font, FontFallbacks, FontFeatures, FontStyle,
    FontWeight, Hsla, Image, ImageFormat, InteractiveText, ObjectFit, Render, Rgba,
    StrikethroughStyle, StyledText, TextAlign, TextRun, UnderlineStyle, Window, div, img, point,
    prelude::*, px, relative,
};

#[cfg(target_os = "windows")]
use gpui::{App, Bounds, MouseButton, MouseDownEvent, Pixels, Point, WindowId};

use crate::dictionary::{DictLoadState, DictRequest, DictionaryView};
use crate::theme::Theme;
use crate::ui::text_input::{TextInput, TextInputEvent};
#[cfg(target_os = "windows")]
use crate::window_layout::{RectF, snap_origin};
#[cfg(target_os = "windows")]
use crate::window_move;

/// 把查词/清空历史请求送回主窗口的通道；释义里的链接复用查词部分。
type LookupSender = futures::channel::mpsc::UnboundedSender<DictRequest>;

pub(crate) struct DictionaryWindow {
    view: DictionaryView,
    theme: Theme,
    /// 查词输入框：双击查词和手动输入共用同一个框。
    input: Entity<TextInput>,
    /// 把查询词送回主窗口（查词逻辑和状态都在 `AppState`）。
    lookup: LookupSender,
    /// 拖动标题栏时的起点快照（仅 Windows 手动移动窗口时需要）。
    #[cfg(target_os = "windows")]
    drag: Option<WindowDrag>,
}

#[cfg(target_os = "windows")]
struct WindowDrag {
    /// 按下鼠标时，鼠标在窗口内的位置；拖动时让这个点一直跟着鼠标。
    cursor: Point<Pixels>,
    /// 按下时窗口左上角的位置。目标位置一律从它加鼠标位移算出来：
    /// 移动请求是异步执行、可能被丢弃的，不能拿当前（可能滞后的）位置累加。
    origin: Point<Pixels>,
    /// 按下时主窗口的位置；拖拽期间主窗口不会移动。
    main: Option<RectF>,
    work: RectF,
}

/// 用于启动窗口拖拽的占位 payload，本身不渲染任何东西。
#[cfg(target_os = "windows")]
struct DictionaryWindowDrag;

impl DictionaryWindow {
    pub fn new(
        view: DictionaryView,
        theme: Theme,
        lookup: LookupSender,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new("输入字词，Enter 查词", theme.clone(), 13.0, cx);
            if !view.word.is_empty() {
                input.set_value(view.word.clone(), cx);
            }
            input
        });

        cx.subscribe(
            &input,
            |this, _input, event: &TextInputEvent, cx| match event {
                TextInputEvent::Submitted { .. } => this.submit_query(cx),
                TextInputEvent::Cancelled => {
                    // Esc 清空输入框；释义保持不动，方便继续看。
                    this.input.update(cx, |input, cx| input.clear(cx));
                }
                TextInputEvent::Changed => this.suggest(cx),
            },
        )
        .detach();

        Self {
            view,
            theme,
            input,
            lookup,
            #[cfg(target_os = "windows")]
            drag: None,
        }
    }

    /// 把输入框里的词送去主窗口查词。
    fn submit_query(&mut self, cx: &mut Context<Self>) {
        let term = self.input.read(cx).value().trim().to_string();
        if term.is_empty() {
            return;
        }
        if self
            .lookup
            .unbounded_send(DictRequest::Lookup(term))
            .is_err()
        {
            log::warn!("dictionary lookup channel closed");
        }
    }

    /// 输入变化：把最新内容发给主窗口做防抖联想。
    fn suggest(&mut self, cx: &mut Context<Self>) {
        let term = self.input.read(cx).value().to_string();
        if self
            .lookup
            .unbounded_send(DictRequest::Suggest(term))
            .is_err()
        {
            log::warn!("dictionary lookup channel closed");
        }
    }

    /// 点击历史/建议词条：回填输入框并立即重查。
    fn query_history(&mut self, term: String, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(term.clone(), cx));
        if self
            .lookup
            .unbounded_send(DictRequest::Lookup(term))
            .is_err()
        {
            log::warn!("dictionary lookup channel closed");
        }
    }

    fn clear_history(&mut self) {
        if self
            .lookup
            .unbounded_send(DictRequest::ClearHistory)
            .is_err()
        {
            log::warn!("dictionary lookup channel closed");
        }
    }

    pub fn apply(&mut self, view: DictionaryView, theme: Theme, cx: &mut Context<Self>) {
        // 只有“查询词变了”才回填输入框；联想/加载状态同步不能覆盖用户正在输入的内容。
        let word_changed = self.view.word != view.word;
        self.view = view;
        self.theme = theme.clone();
        self.input
            .update(cx, |input, cx| input.set_theme(theme, cx));

        // 双击查词或点击历史/建议时把词回填到输入框，方便接着改字继续查。
        let word = self.view.word.clone();
        if word_changed && !word.is_empty() && self.input.read(cx).value() != word {
            self.input.update(cx, |input, cx| input.set_value(word, cx));
        }

        cx.notify();
    }

    /// 记下拖拽起点，以及主窗口/工作区位置。
    #[cfg(target_os = "windows")]
    fn begin_window_drag(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let work = window
            .display(&**cx)
            .map(|display| rect_from_bounds(display.visible_bounds()))
            .unwrap_or_else(|| RectF::new(0.0, 0.0, 1920.0, 1080.0));
        let main = other_window_bounds(&mut **cx, window.window_handle().window_id())
            .map(rect_from_bounds);

        self.drag = Some(WindowDrag {
            cursor: event.position,
            origin: window.bounds().origin,
            main,
            work,
        });
    }

    /// 跟随鼠标移动窗口；靠近主窗口或工作区边缘时吸附。
    #[cfg(target_os = "windows")]
    fn drag_window(
        &mut self,
        event: &gpui::DragMoveEvent<DictionaryWindowDrag>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let Some(drag) = &self.drag else {
            return;
        };

        // 窗口跟着鼠标走：让按下时鼠标在窗口内的那个点一直跟着鼠标。
        let raw_origin = drag.origin + event.event.position - drag.cursor;
        let size = window.bounds().size;
        let (x, y) = snap_origin(
            (f32::from(raw_origin.x), f32::from(raw_origin.y)),
            (f32::from(size.width), f32::from(size.height)),
            drag.main,
            drag.work,
        );

        window_move::move_window(window, point(px(x), px(y)));

        // 告诉主窗口浮窗现在在哪：贴边状态（和间距）只有它那边算得准，
        // 拖动结束后主窗口移动时浮窗才会继续跟随。
        self.lookup
            .unbounded_send(DictRequest::FloatPlaced(RectF::new(
                x,
                y,
                f32::from(size.width),
                f32::from(size.height),
            )))
            .ok();
    }
}

#[cfg(target_os = "windows")]
fn rect_from_bounds(bounds: Bounds<Pixels>) -> RectF {
    RectF::new(
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    )
}

/// 找主窗口的 bounds：除了当前词典窗口以外最大的那个窗口。
#[cfg(target_os = "windows")]
fn other_window_bounds(cx: &mut App, current: WindowId) -> Option<Bounds<Pixels>> {
    let mut best: Option<Bounds<Pixels>> = None;
    for handle in cx.windows() {
        if handle.window_id() == current {
            continue;
        }
        let Ok(bounds) = handle.update(cx, |_view, window, _cx| window.bounds()) else {
            continue;
        };
        let area = {
            let size = bounds.size;
            f32::from(size.width) * f32::from(size.height)
        };
        let is_larger = best.is_none_or(|best| {
            let size = best.size;
            f32::from(size.width) * f32::from(size.height) < area
        });
        if is_larger {
            best = Some(bounds);
        }
    }
    best
}

impl Render for DictionaryWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let view = &self.view;

        let word = if view.word.is_empty() {
            String::new()
        } else {
            format!("· {}", view.word)
        };

        let mut body = div()
            .id("dictionary-window-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col();

        match &view.load_state {
            DictLoadState::Unconfigured => {
                body = body
                    .child(muted("还没有配置词典", &theme))
                    .child(px_gap(6.))
                    .child(muted("请在主窗口的设置中选择 .mdx 文件。", &theme));
            }
            DictLoadState::Loading => {
                body = body.child(muted("正在加载词典…", &theme));
            }
            DictLoadState::Failed(message) => {
                body = body
                    .child(muted("词典加载失败", &theme))
                    .child(error_text(message, &theme));
            }
            DictLoadState::Ready { title, entries } => {
                if view.loading {
                    body = body.child(muted("正在查询…", &theme));
                } else if let Some(error) = &view.error {
                    body = body.child(error_text(error, &theme));
                } else if view.not_found {
                    body = body.child(muted(format!("未找到「{}」", view.word), &theme));
                } else if view.entries.is_empty() {
                    body = body.child(muted("双击 PDF 中的单词或汉字开始查词", &theme));
                } else {
                    for entry in &view.entries {
                        body = body.child(entry_element(entry, &theme, &self.lookup));
                    }
                }

                if view.entries.is_empty() {
                    body = body.child(px_gap(12.)).child(muted(
                        match title {
                            Some(title) => format!("词典：{title} · {entries} 条"),
                            None => format!("词典 · {entries} 条"),
                        },
                        &theme,
                    ));
                }
            }
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_family.clone())
            .child(self.titlebar(&theme, word, cx))
            .child(self.query_row(&theme, cx))
            .child(self.suggestions_row(&theme, cx))
            .child(self.history_row(&theme, cx))
            .child(body)
    }
}

impl DictionaryWindow {
    /// 自绘标题栏：左边是标题和当前词，整条可拖动，右边是关闭按钮。
    fn titlebar(&self, theme: &Theme, word: String, cx: &mut Context<Self>) -> gpui::Div {
        let title = div()
            .id("dictionary-titlebar")
            .flex()
            .flex_1()
            .min_w(px(0.))
            .items_center()
            .gap(px(8.))
            .h_full()
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("词典"),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(word),
            );
        let title = crate::ui::chrome::drag_cursor(title);

        // Windows 上 GPUI 没有移动窗口的 API，这里自己跟踪拖拽并调 SetWindowPos；
        // 其他平台交给系统的无边框标题栏拖拽。
        #[cfg(target_os = "windows")]
        let title = title
            .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_window_drag))
            .on_drag(
                DictionaryWindowDrag,
                |_drag: &DictionaryWindowDrag, _offset, _window, cx| cx.new(|_| gpui::EmptyView),
            )
            .on_drag_move::<DictionaryWindowDrag>(cx.listener(
                |this, event: &gpui::DragMoveEvent<DictionaryWindowDrag>, window, cx| {
                    this.drag_window(event, window, cx);
                },
            ));
        #[cfg(not(target_os = "windows"))]
        let title = title.window_control_area(gpui::WindowControlArea::Drag);

        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(40.))
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .child(title)
            .child(
                div()
                    .id("dictionary-close")
                    .flex_shrink_0()
                    .px(px(6.))
                    .py(px(2.))
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(theme.text_muted)
                    .hover({
                        let hover_bg = theme.surface_hover;
                        move |style| style.bg(hover_bg)
                    })
                    .on_click(
                        cx.listener(|_this, _event: &gpui::ClickEvent, window, _cx| {
                            window.remove_window();
                        }),
                    )
                    .child("✕"),
            )
    }

    /// 前缀建议：跨词典、去重、最多 8 条，点击填入并查询。
    fn suggestions_row(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
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

        if self.view.suggestions.is_empty() {
            return row;
        }

        row = row.child(div().text_color(theme.text_muted).child("建议"));
        for (index, term) in self.view.suggestions.clone().into_iter().enumerate() {
            let id = ElementId::NamedInteger("dict-suggestion".into(), index as u64);
            let label = term.clone();
            row = row.child(
                div()
                    .id(id)
                    .max_w(px(180.))
                    .truncate()
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover({
                        let hover = theme.accent_hover;
                        move |style| style.bg(hover)
                    })
                    .on_click(
                        cx.listener(move |this, _event: &gpui::ClickEvent, _window, cx| {
                            this.query_history(label.clone(), cx);
                        }),
                    )
                    .child(term),
            );
        }
        row
    }

    /// 查词历史：最新的在前，点击重查；右侧可一键清空。
    fn history_row(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .px_3()
            .pb_2()
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(11.));

        if self.view.history.is_empty() {
            return row;
        }

        row = row.child(div().text_color(theme.text_muted).child("最近"));
        for (index, term) in self.view.history.clone().into_iter().enumerate() {
            let id = ElementId::NamedInteger("dict-history".into(), index as u64);
            let label = term.clone();
            row = row.child(
                div()
                    .id(id)
                    .max_w(px(140.))
                    .truncate()
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover({
                        let hover = theme.accent_hover;
                        move |style| style.bg(hover)
                    })
                    .on_click(
                        cx.listener(move |this, _event: &gpui::ClickEvent, _window, cx| {
                            this.query_history(label.clone(), cx);
                        }),
                    )
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
                .on_click(
                    cx.listener(|this, _event: &gpui::ClickEvent, _window, _cx| {
                        this.clear_history();
                    }),
                )
                .child("清空"),
        )
    }

    /// 查词输入行：手动输入 + 「查」按钮。
    fn query_row(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
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
                    .h(px(28.))
                    .px_2()
                    .rounded_md()
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .child(self.input.clone()),
            )
            .child(
                div()
                    .id("dictionary-submit")
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .h(px(28.))
                    .px_3()
                    .rounded_md()
                    .bg(theme.accent)
                    .text_color(theme.accent_text)
                    .text_size(px(12.))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(|this, _event: &gpui::ClickEvent, _window, cx| {
                        this.submit_query(cx);
                    }))
                    .child("查"),
            )
    }
}

fn entry_element(entry: &DictEntry, theme: &Theme, lookup: &LookupSender) -> Div {
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
        for block in &entry.blocks {
            column = column.child(block_element(block, theme, lookup));
        }
    }

    column
}

fn block_element(block: &Block, theme: &Theme, lookup: &LookupSender) -> AnyElement {
    match block {
        Block::Paragraph(paragraph) => paragraph_element(paragraph, theme, lookup),
        Block::Table(table) => table_element(table, theme, lookup).into_any_element(),
        Block::Container(container) => {
            container_element(container, theme, lookup).into_any_element()
        }
    }
}

fn paragraph_element(paragraph: &Paragraph, theme: &Theme, lookup: &LookupSender) -> AnyElement {
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
        for span in &paragraph.spans {
            match span {
                Span::Text(text) => {
                    for chunk in wrap_chunks(&text.text) {
                        element = element.child(chunk_element(
                            chunk,
                            &text.style,
                            size,
                            color,
                            &block_font,
                            theme,
                        ));
                    }
                }
                Span::Image(image) => {
                    element = element.child(image_element(image));
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
                link_text(&text, styled, links, lookup)
            };
            element = element.child(div().flex_1().min_w(px(0.)).child(content));
        }
    }

    element.into_any_element()
}

/// 把带链接的文本包成 `InteractiveText`：悬停变手型，点击查词或开浏览器。
fn link_text(
    text: &str,
    styled: StyledText,
    links: Vec<(std::ops::Range<usize>, &LinkTarget)>,
    lookup: &LookupSender,
) -> AnyElement {
    let mut ranges = Vec::with_capacity(links.len());
    let mut targets = Vec::with_capacity(links.len());
    for (range, target) in links {
        ranges.push(range);
        targets.push(target.clone());
    }
    let lookup = lookup.clone();
    InteractiveText::new(
        ElementId::NamedInteger("dict-link".into(), hash_text(text)),
        styled,
    )
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

/// 元素 id 只需要在同一个窗口内唯一；用正文内容做哈希足够区分不同段落。
fn hash_text(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn table_element(table: &Table, theme: &Theme, lookup: &LookupSender) -> Div {
    let mut root = div().flex().flex_col().w_full().min_w(px(0.));
    root = root
        .mt(px(table.style.margin_top.unwrap_or(6.0)))
        .mb(px(table.style.margin_bottom.unwrap_or(0.0)));
    root = styled_box(root, &table.style, theme);

    for row in &table.rows {
        let mut row_element = div()
            .flex()
            .flex_row()
            .items_stretch()
            .w_full()
            .min_w(px(0.))
            .mt(px(row.style.margin_top.unwrap_or(0.0)))
            .mb(px(row.style.margin_bottom.unwrap_or(0.0)));
        row_element = styled_box(row_element, &row.style, theme);
        for cell in &row.cells {
            row_element = row_element.child(cell_element(cell, theme, lookup));
        }
        root = root.child(row_element);
    }

    root
}

fn cell_element(cell: &TableCell, theme: &Theme, lookup: &LookupSender) -> Div {
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

    for block in &cell.blocks {
        element = element.child(block_element(block, theme, lookup));
    }
    element
}

/// `display: flex` 容器：直接映射到 GPUI 的 flex 布局。
fn container_element(container: &Container, theme: &Theme, lookup: &LookupSender) -> Div {
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

    for item in &container.items {
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

        for block in &item.blocks {
            element = element.child(block_element(block, theme, lookup));
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
) -> Div {
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
    element.child(chunk)
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
