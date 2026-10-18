//! 单行文本输入框。
//!
//! 实现 [`EntityInputHandler`]，所以走的是平台的文本输入通道：中文输入法的组合输入
//! （拼音候选、上屏）能正常收到 `WM_IME_COMPOSITION`，而不是靠 `key_char` 拼字。
//!
//! 结构分两层：
//! - [`TextField`]：纯文本编辑逻辑（内容、选区、组合区间、UTF-16 换算），不碰 GPUI，可单测。
//! - [`TextInput`]：GPUI 实体，负责焦点、按键动作、光标/选区绘制，并在 paint 时把
//!   `ElementInputHandler` 交给窗口。

use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::{
    Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, Hsla, InspectorElementId,
    KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, Render, ShapedLine, SharedString, Style, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::Theme;

/// 光标闪烁的一个完整周期：前半周期显示、后半周期隐藏，各 500ms。
const CARET_BLINK_PERIOD: Duration = Duration::from_millis(1000);

gpui::actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
    ]
);

/// 输入框对外的变化通知，外层用它同步自己的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextInputEvent {
    /// 文本或选区变了。
    Changed,
    /// 按了 `Enter`；`forward` 为 `false` 表示按住了 Shift。
    Submitted { forward: bool },
    /// 按了 `Esc`。
    Cancelled,
}

/// 纯文本编辑状态。所有偏移都是 UTF-8 字节偏移，对外接口按 UTF-16 换算。
pub(crate) struct TextField {
    content: String,
    selection: Range<usize>,
    reversed: bool,
    /// 输入法正在组合的区间（拼音未上屏那段），和 `content` 用同一套字节偏移。
    marked: Option<Range<usize>>,
}

impl Default for TextField {
    fn default() -> Self {
        Self {
            content: String::new(),
            selection: 0..0,
            reversed: false,
            marked: None,
        }
    }
}

impl TextField {
    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn reversed(&self) -> bool {
        self.reversed
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    /// 整体替换内容，光标落到末尾。返回是否真的变了。
    pub fn set_content(&mut self, text: &str) -> bool {
        if self.content == text {
            return false;
        }
        self.content.clear();
        self.content.push_str(text);
        self.selection = self.content.len()..self.content.len();
        self.reversed = false;
        self.marked = None;
        true
    }

    pub fn clear(&mut self) -> bool {
        self.set_content("")
    }

    pub fn move_to(&mut self, offset: usize) {
        let offset = offset.min(self.content.len());
        self.selection = offset..offset;
        self.reversed = false;
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.content.len();
        self.reversed = false;
    }

    pub fn select_to(&mut self, offset: usize) {
        let offset = offset.min(self.content.len());
        if self.reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }
    }

    /// 在光标处插入文本（用于“在首页直接打字就开始搜索”）。
    pub fn insert(&mut self, text: &str) -> bool {
        self.replace(self.selection.clone(), text)
    }

    /// 替换一个字节区间；返回是否发生了变化。
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> bool {
        let start = range.start.min(self.content.len());
        let end = range.end.min(self.content.len()).max(start);
        let before = self.content.clone();
        self.content = format!("{}{}{}", &before[..start], text, &before[end..]);
        let caret = start + text.len();
        self.selection = caret..caret;
        self.reversed = false;
        self.marked = None;
        self.content != before
    }

    /// 按 UTF-16 区间替换；`range` 为 `None` 时依次退回组合区间、选区。
    pub fn replace_utf16(&mut self, range: Option<Range<usize>>, text: &str) -> bool {
        let range = range
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        self.replace(range, text)
    }

    /// 输入法组合中：替换区间内容并标记为未上屏。
    pub fn replace_and_mark_utf16(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
    ) -> bool {
        let range = range
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());

        let start = range.start.min(self.content.len());
        let end = range.end.min(self.content.len()).max(start);
        let before = self.content.clone();
        self.content = format!("{}{}{}", &before[..start], text, &before[end..]);

        self.marked = (!text.is_empty()).then_some(start..start + text.len());
        self.selection = selected_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .map(|range| start + range.start..start + range.end)
            .unwrap_or_else(|| {
                let caret = start + text.len();
                caret..caret
            });
        self.reversed = false;

        self.content != before
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    pub fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    /// 光标左移一格；有选区时先收起到选区起点。
    pub fn move_left(&mut self) {
        if self.selection.is_empty() {
            let target = self.previous_boundary(self.cursor());
            self.move_to(target);
        } else {
            self.move_to(self.selection.start);
        }
    }

    pub fn move_right(&mut self) {
        if self.selection.is_empty() {
            let target = self.next_boundary(self.cursor());
            self.move_to(target);
        } else {
            self.move_to(self.selection.end);
        }
    }

    pub fn select_left(&mut self) {
        let target = self.previous_boundary(self.cursor());
        self.select_to(target);
    }

    pub fn select_right(&mut self) {
        let target = self.next_boundary(self.cursor());
        self.select_to(target);
    }

    pub fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    pub fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    pub fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        let start = self.offset_from_utf16(range.start);
        let end = self.offset_from_utf16(range.end).max(start);
        start..end
    }

    /// 当前选中的文本（没有选区时为空）。
    pub fn selected_text(&self) -> String {
        self.content[self.selection.clone()].to_string()
    }
}

pub(crate) struct TextInput {
    focus_handle: FocusHandle,
    field: TextField,
    placeholder: SharedString,
    theme: Theme,
    font_size: f32,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// 闪烁相位起点：每次输入/移动光标都重置，保证「刚打完字光标一定是亮的」。
    caret_reset_at: Instant,
}

impl TextInput {
    pub fn new(
        placeholder: impl Into<SharedString>,
        theme: Theme,
        font_size: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            field: TextField::default(),
            placeholder: placeholder.into(),
            theme,
            font_size,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            caret_reset_at: Instant::now(),
        }
    }

    pub fn value(&self) -> &str {
        self.field.content()
    }

    /// 外层同步内容（例如词典窗口回填当前词），不会发出 `Changed`。
    pub fn set_value(&mut self, value: impl AsRef<str>, cx: &mut Context<Self>) {
        if self.field.set_content(value.as_ref()) {
            self.reset_caret();
            cx.notify();
        }
    }

    /// 主题切换后同步配色。
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme.dark != theme.dark {
            self.theme = theme;
            cx.notify();
        }
    }

    /// 直接插入文本（外层在没有焦点时也能往里写）。
    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.field.insert(text) {
            self.reset_caret();
            cx.notify();
            cx.emit(TextInputEvent::Changed);
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.field.clear() {
            self.reset_caret();
            cx.notify();
            cx.emit(TextInputEvent::Changed);
        }
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.field.select_all();
        self.reset_caret();
        cx.notify();
    }

    /// 让闪烁相位从「可见」重新开始；输入、移动光标、点击定位都要调。
    fn reset_caret(&mut self) {
        self.caret_reset_at = Instant::now();
    }

    /// 当前闪烁相位（0..1）。
    fn caret_phase(&self) -> f32 {
        caret_phase(self.caret_reset_at.elapsed())
    }

    fn notify_changed(&mut self, cx: &mut Context<Self>) {
        self.reset_caret();
        cx.notify();
        cx.emit(TextInputEvent::Changed);
    }

    // ---------- 按键动作 ----------

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.field.selection().is_empty() {
            let previous = self.field.previous_boundary(self.field.cursor());
            if previous == self.field.cursor() {
                window.play_system_bell();
                return;
            }
            self.field.select_to(previous);
        }
        if self.field.replace_utf16(None, "") {
            self.notify_changed(cx);
        }
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.field.selection().is_empty() {
            let next = self.field.next_boundary(self.field.cursor());
            if next == self.field.cursor() {
                window.play_system_bell();
                return;
            }
            self.field.select_to(next);
        }
        if self.field.replace_utf16(None, "") {
            self.notify_changed(cx);
        }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.field.move_left();
        self.reset_caret();
        cx.notify();
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.field.move_right();
        self.reset_caret();
        cx.notify();
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.field.select_left();
        self.reset_caret();
        cx.notify();
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.field.select_right();
        self.reset_caret();
        cx.notify();
    }

    fn select_all_action(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.field.select_all();
        self.reset_caret();
        cx.notify();
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.field.move_to(0);
        self.reset_caret();
        cx.notify();
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let end = self.field.content().len();
        self.field.move_to(end);
        self.reset_caret();
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace(['\n', '\r'], " ");
            if self.field.replace_utf16(None, &text) {
                self.notify_changed(cx);
            }
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.field.selected_text();
        if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.field.selected_text();
        if selected.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(selected));
        if self.field.replace_utf16(None, "") {
            self.notify_changed(cx);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                let forward = !event.keystroke.modifiers.shift;
                cx.emit(TextInputEvent::Submitted { forward });
                cx.stop_propagation();
            }
            "escape" => {
                cx.emit(TextInputEvent::Cancelled);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    // ---------- 鼠标 ----------

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if self.field.is_empty() {
            return 0;
        }
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.field.content().len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.is_selecting = true;
        let index = self.index_for_position(event.position);
        if event.modifiers.shift {
            self.field.select_to(index);
        } else {
            self.field.move_to(index);
        }
        self.reset_caret();
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_selecting {
            return;
        }
        let index = self.index_for_position(event.position);
        self.field.select_to(index);
        self.reset_caret();
        cx.notify();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = false;
        cx.notify();
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<TextInputEvent> for TextInput {}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.field.range_from_utf16(&range_utf16);
        actual_range.replace(self.field.range_to_utf16(&range));
        Some(self.field.content()[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.field.range_to_utf16(&self.field.selection()),
            reversed: self.field.reversed(),
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.field
            .marked()
            .as_ref()
            .map(|range| self.field.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.field.unmark();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.field.replace_utf16(range_utf16, text) {
            self.notify_changed(cx);
        } else {
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed =
            self.field
                .replace_and_mark_utf16(range_utf16, new_text, new_selected_range_utf16);
        if changed {
            self.notify_changed(cx);
        } else {
            cx.notify();
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.field.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.last_bounds?;
        let index = self.index_for_position(position);
        Some(self.field.offset_to_utf16(index))
    }
}

struct TextElement {
    input: gpui::Entity<TextInput>,
}

/// 从相位起点算起过了 `elapsed` 时的闪烁相位。
fn caret_phase(elapsed: Duration) -> f32 {
    (elapsed.as_secs_f32() / CARET_BLINK_PERIOD.as_secs_f32()) % 1.0
}

/// 闪烁相位 → 光标是否可见：前半周期显示，后半周期隐藏。
fn caret_visible(phase: f32) -> bool {
    phase < 0.5
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll_x: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let focused = input.focus_handle.is_focused(window);
        let reduce_motion = cx.reduce_motion();
        if focused && !reduce_motion {
            // 只把输入框自己标脏，避免为了闪烁重绘整个窗口。
            let entity = self.input.clone();
            window.on_next_frame(move |_window, cx| cx.notify(entity.entity_id()));
        }
        // 系统开了「减少动态效果」时不做闪烁，光标常亮。
        let caret_on = reduce_motion || caret_visible(input.caret_phase());
        let content = input.field.content().to_string();
        let empty = content.is_empty();
        let selection = input.field.selection();
        let cursor = input.field.cursor();
        let marked = input.field.marked();

        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let (display_text, text_color): (SharedString, Hsla) = if empty {
            (
                input.placeholder.clone(),
                Hsla::from(input.theme.text_muted).opacity(0.55),
            )
        } else {
            (SharedString::from(content.clone()), input.theme.text.into())
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        let runs: Vec<TextRun> = match marked {
            // 组合中的音节画一条下划线，和系统输入法的观感一致。
            Some(marked) if !empty && marked.end <= display_text.len() => vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked.end,
                    ..run.clone()
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            _ => vec![run],
        };

        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        // 光标位置决定横向滚动：内容比框长时把光标贴在右边缘。
        let cursor_x = line.x_for_index(cursor.min(content.len()));
        let max_scroll = (line.width - bounds.size.width + px(10.)).max(px(0.));
        let scroll_x = (cursor_x - bounds.size.width + px(10.)).clamp(px(0.), max_scroll);

        let line_height = window.line_height();
        let (selection_quad, cursor_quad) = if !focused {
            (None, None)
        } else if selection.is_empty() {
            let quad = caret_on.then(|| {
                fill(
                    Bounds::new(
                        point(
                            bounds.left() + cursor_x - scroll_x,
                            bounds.top() + (line_height - font_size) / 2.,
                        ),
                        size(px(1.5), font_size),
                    ),
                    input.theme.accent,
                )
            });
            (None, quad)
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selection.start) - scroll_x,
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selection.end) - scroll_x,
                            bounds.bottom(),
                        ),
                    ),
                    input.theme.selection,
                )),
                None,
            )
        };

        PrepaintState {
            line: Some(line),
            cursor: cursor_quad,
            selection: selection_quad,
            scroll_x,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );

        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }

        let line = prepaint
            .line
            .take()
            .expect("text input laid out in prepaint");
        let origin = point(bounds.left() - prepaint.scroll_x, bounds.top());
        if let Err(error) = line.paint(
            origin,
            window.line_height(),
            TextAlign::Left,
            None,
            window,
            cx,
        ) {
            log::warn!("paint text input failed: {error:#}");
        }

        if let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .items_center()
            .overflow_hidden()
            .key_context("TextInput")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .text_size(px(self.font_size))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all_action))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement { input: cx.entity() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field_with(text: &str) -> TextField {
        let mut field = TextField::default();
        field.set_content(text);
        field
    }

    #[test]
    fn caret_blinks_once_per_half_cycle() {
        // 前半周期可见、后半周期隐藏，切换点正好在 0.5。
        assert!(caret_visible(0.0));
        assert!(caret_visible(0.49));
        assert!(!caret_visible(0.5));
        assert!(!caret_visible(0.99));
    }

    #[test]
    fn caret_phase_wraps_every_second() {
        assert_eq!(caret_phase(Duration::ZERO), 0.0);
        assert!((caret_phase(Duration::from_millis(500)) - 0.5).abs() < f32::EPSILON);
        assert_eq!(caret_phase(CARET_BLINK_PERIOD), 0.0);
        assert!((caret_phase(Duration::from_millis(1250)) - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn set_content_puts_cursor_at_end() {
        let field = field_with("abc");
        assert_eq!(field.content(), "abc");
        assert_eq!(field.cursor(), 3);
    }

    #[test]
    fn insert_replaces_selection() {
        let mut field = field_with("hello");
        field.move_to(0);
        field.select_to(2);
        assert_eq!(field.selection(), 0..2);
        field.insert("hé");
        assert_eq!(field.content(), "héllo");
        assert_eq!(field.cursor(), "hé".len());
    }

    #[test]
    fn backspace_boundaries_respect_utf8() {
        let field = field_with("汉字");
        assert_eq!(field.previous_boundary(6), 3);
        assert_eq!(field.previous_boundary(3), 0);
        assert_eq!(field.next_boundary(0), 3);
        assert_eq!(field.next_boundary(6), 6);
    }

    #[test]
    fn utf16_conversion_handles_surrogates_and_chinese() {
        let field = field_with("a中😀");
        assert_eq!(field.offset_to_utf16(0), 0);
        assert_eq!(field.offset_to_utf16(1), 1);
        assert_eq!(field.offset_to_utf16(4), 2);
        assert_eq!(field.offset_to_utf16(8), 4);
        assert_eq!(field.offset_from_utf16(4), 8);
        assert_eq!(field.range_to_utf16(&(0..4)), 0..2);
    }

    #[test]
    fn marked_range_tracks_composition() {
        let mut field = field_with("");
        field.replace_and_mark_utf16(None, "nihao", None);
        assert_eq!(field.content(), "nihao");
        assert_eq!(field.marked(), Some(0..5));

        // 输入法上屏：在组合区间里替换成汉字，标记消失。
        field.replace_utf16(Some(0..5), "你好");
        assert_eq!(field.content(), "你好");
        assert_eq!(field.marked(), None);
        assert_eq!(field.cursor(), 6);
    }

    #[test]
    fn move_left_collapses_selection_first() {
        let mut field = field_with("abc");
        field.move_to(0);
        field.select_to(3);
        field.move_left();
        assert_eq!(field.selection(), 0..0);
        field.move_right();
        assert_eq!(field.cursor(), 1);
    }

    #[test]
    fn set_content_clears_when_unchanged() {
        let mut field = field_with("abc");
        assert!(!field.set_content("abc"));
        assert!(field.set_content("abcd"));
    }
}
