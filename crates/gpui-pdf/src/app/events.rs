//! 事件分派：PDF 引擎事件、词典事件、键盘、鼠标与链接。
//!
//! 本模块只做「事件 → AppState 上的一个明确动作」的翻译：一步能完成的直接调用，
//! 需要多步状态迁移的交给 `document` / `render` / `window`。鼠标命中换算
//! （屏幕坐标 → 页内字符下标）也放在这里，因为它是鼠标事件的私有前置步骤，
//! 只有本模块使用。

use std::borrow::Cow;

use dict_core::Event as DictEvent;
use gpui::{
    ClipboardItem, Context, Focusable, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Point, ScrollDelta, ScrollWheelEvent, Window,
};
use pdf_core::{CharBox, Event, LinkTarget};

use super::FocusTarget;
use crate::dictionary::DictLoadState;

impl super::AppState {
    // ---------- 引擎事件 ----------

    pub(super) fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Opened {
                path,
                pages,
                outline,
                metadata,
            } => self.on_document_opened(path, pages, outline, metadata, cx),
            Event::RenderedTile {
                page,
                request,
                origin_x,
                origin_y,
                width,
                height,
                bgra,
            } => self.on_tile_rendered(page, request, origin_x, origin_y, width, height, bgra, cx),
            Event::PageElements {
                page,
                request,
                chars,
                links,
            } => self.on_page_elements(page, request, chars, links, cx),
            Event::SearchResults {
                request,
                query,
                pages,
                total,
            } => self.on_search_results(request, query, pages, total, cx),
            Event::SearchProgress {
                request,
                scanned,
                total,
            } => self.on_search_progress(request, scanned, total),
            Event::Error(message) => {
                log::error!("{message}");
                self.set_status(message, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn handle_dict_event(&mut self, event: DictEvent, cx: &mut Context<Self>) {
        match event {
            DictEvent::Configured {
                request,
                dictionaries,
            } => {
                if request != self.dictionary.configure_request {
                    return;
                }

                let mut ready_entries = 0u64;
                let mut ready_count = 0usize;
                let mut first_title = None;
                let mut first_failure = None;
                let mut statuses = Vec::with_capacity(self.settings.current.dictionaries.len());
                for setting in &self.settings.current.dictionaries {
                    let info = dictionaries.iter().find(|info| info.path == setting.path);
                    let state = if !setting.enabled {
                        DictLoadState::Unconfigured
                    } else {
                        match info {
                            Some(info) => match &info.error {
                                Some(error) => {
                                    if first_failure.is_none() {
                                        first_failure = Some(error.clone());
                                    }
                                    DictLoadState::Failed(error.clone())
                                }
                                None => {
                                    ready_entries += info.entries;
                                    ready_count += 1;
                                    if first_title.is_none() {
                                        first_title = info.title.clone();
                                    }
                                    DictLoadState::Ready {
                                        title: info.title.clone(),
                                        entries: info.entries,
                                    }
                                }
                            },
                            None => DictLoadState::Loading,
                        }
                    };
                    statuses.push(crate::dictionary::DictStatus {
                        path: setting.path.clone(),
                        state,
                    });
                }
                self.dictionary.statuses = statuses;

                let mut fonts = Vec::new();
                for info in &dictionaries {
                    for font in &info.fonts {
                        let signature = font_signature(font);
                        if self.registered_dict_fonts.insert(signature) {
                            fonts.push(Cow::Owned(font.clone()));
                        }
                    }
                }
                if !fonts.is_empty() {
                    let count = fonts.len();
                    match cx.text_system().add_fonts(fonts) {
                        Ok(()) => log::info!("registered {count} dictionary fonts"),
                        Err(error) => log::warn!("注册词典字体失败: {error:#}"),
                    }
                }

                if !self.settings.current.has_enabled_dictionary() {
                    self.dictionary.load_state = DictLoadState::Unconfigured;
                    self.dictionary.loading = false;
                } else if ready_count > 0 {
                    let title = if ready_count == 1 {
                        first_title
                    } else {
                        Some(format!("{ready_count} 本词典"))
                    };
                    self.dictionary.load_state = DictLoadState::Ready {
                        title,
                        entries: ready_entries,
                    };
                    self.dictionary.loading = false;
                    if let Some(term) = self.dictionary.pending_term.take() {
                        self.lookup_word(term, cx);
                        return;
                    }
                    self.set_status(
                        format!("词典已加载 · {ready_count} 本 · {ready_entries} 条"),
                        cx,
                    );
                } else if let Some(error) = first_failure {
                    self.dictionary.load_state = DictLoadState::Failed(error.clone());
                    self.dictionary.loading = false;
                    self.dictionary.error = Some(error.clone());
                    self.set_status(error, cx);
                } else {
                    self.dictionary.load_state = DictLoadState::Loading;
                }
            }
            DictEvent::Found {
                request,
                term,
                entries,
                redirected,
            } => {
                if request != self.dictionary.request {
                    return;
                }
                self.dictionary.loading = false;
                self.dictionary.not_found = false;
                self.dictionary.error = None;
                self.dictionary.entries = entries;
                if redirected {
                    self.dictionary.word = term;
                }
            }
            DictEvent::NotFound { request, term } => {
                if request != self.dictionary.request {
                    return;
                }
                self.dictionary.loading = false;
                self.dictionary.entries.clear();
                self.dictionary.not_found = true;
                self.dictionary.word = term;
            }
            DictEvent::PrefixSuggestions {
                request,
                suggestions,
                ..
            } => {
                if request != self.dictionary.suggest_request {
                    return;
                }
                self.dictionary.suggestions = crate::dict_suggest::trim_suggestions(suggestions);
                self.sync_dictionary_window(cx);
            }
            DictEvent::Error { request, message } => {
                if let Some(request) = request
                    && request != self.dictionary.request
                {
                    return;
                }
                self.dictionary.loading = false;
                if request.is_none() {
                    if !self.settings.current.has_enabled_dictionary() {
                        return;
                    }
                    self.dictionary.load_state = DictLoadState::Failed(message.clone());
                }
                self.dictionary.error = Some(message.clone());
                self.set_status(message, cx);
            }
        }
        self.sync_dictionary_window(cx);
        cx.notify();
    }

    // ---------- 坐标 ----------

    /// 鼠标位置 → 某一页的页内坐标（像素，原点在页面左上角）。
    ///
    /// 连续滚动下页面不再是屏幕原点，位置要减去「滚动偏移 + 这一页在文档里的位置」。
    /// 超出页面时夹到页边：拖选越过页边界时仍能在这一页里取到最近的字符。
    fn page_point(&self, page: usize, position: Point<gpui::Pixels>) -> Option<(f32, f32)> {
        let layout = self.doc_layout()?;
        let page_box = layout.page_box(page)?;
        let offset = self.viewer.scroll.offset();
        let x = f32::from(position.x) - f32::from(offset.x) - page_box.left;
        let y = f32::from(position.y) - f32::from(offset.y) - page_box.top;
        Some((x.clamp(0.0, page_box.width), y.clamp(0.0, page_box.height)))
    }

    fn hit_char(&self, page: usize, x: f32, y: f32) -> Option<usize> {
        let mut best: Option<(f32, usize)> = None;

        for (index, character) in self.viewer.chars_of(page).iter().enumerate() {
            let rect = character.rect;
            if x >= rect.left && x <= rect.right && y >= rect.bottom && y <= rect.top {
                return Some(index);
            }
            let (cx, cy) = rect.center();
            let distance = ((cx - x).powi(2) + (cy - y).powi(2)).sqrt();
            if best.is_none_or(|(best_distance, _)| distance < best_distance) {
                best = Some((distance, index));
            }
        }

        best.and_then(|(distance, index)| (distance < 24.0).then_some(index))
    }

    /// 屏幕位置换算到某一页的 PDF 坐标（pt，原点在左下角，y 向上）。
    fn char_at_point(&self, page: usize, position: Point<gpui::Pixels>) -> Option<usize> {
        let (local_x, local_y) = self.page_point(page, position)?;
        let size = self.viewer.page_size_at(page)?;
        let x = local_x / self.viewer.zoom;
        let y = size.height - local_y / self.viewer.zoom;
        self.hit_char(page, x, y)
    }

    /// 选区所在页的字符数据；没有选区时为空。
    fn selection_chars(&self) -> &[CharBox] {
        self.viewer
            .selection
            .page
            .map(|page| self.viewer.chars_of(page))
            .unwrap_or_default()
    }

    // ---------- 鼠标与链接 ----------

    /// 在某一页上按下鼠标：起点归到这一页，之后拖动只在这一页里扩展。
    pub(crate) fn on_page_mouse_down(
        &mut self,
        page: usize,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        // 点正文即离开页码输入；否则输入框会一直占着页码的位置。
        if self.page_input_open {
            self.close_page_input(cx);
        }
        let Some(index) = self.char_at_point(page, event.position) else {
            return;
        };
        if event.click_count >= 2 {
            self.viewer.select_word_at(page, index);
            self.viewer.selection.dragging = true;
            self.lookup_selection(cx);
        } else {
            self.viewer.selection.start_at(page, index);
        }
        cx.notify();
    }

    /// Ctrl + 滚轮缩放。滚轮一格约 3 行，每行 1%（一格约 3%）；
    /// 触控板的小增量按比例折算，不量化成整百分点。
    pub(crate) fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.modifiers.control {
            return;
        }
        let line_height = f32::from(window.line_height()).max(1.0);
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => f32::from(delta.y) / line_height,
        };
        if lines.abs() < 0.05 {
            return;
        }
        let percent = crate::layout::wheel_zoom_percent(lines);
        if percent == 0.0 {
            return;
        }
        let scrolled = event.delta.pixel_delta(window.line_height());
        self.zoom_at_pointer(percent, event.position, scrolled, cx);
        cx.stop_propagation();
    }

    /// 按住中键（滚轮键）：进入拖动平移，指针变抓手。
    pub(crate) fn on_page_middle_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.viewer.has_document() {
            return;
        }
        self.panning = Some(event.position);
        // 中键按下不再往下传，避免别的元素拿它当「粘贴/快捷滚」一类快捷键。
        cx.stop_propagation();
        cx.notify();
    }

    /// 中键拖动：按指针位移平移页面（半速），松手后停住。
    fn pan_with(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) -> bool {
        let Some(last) = self.panning else {
            return false;
        };
        // 中键在窗口外松开时收不到 mouse_up，这里捎带把状态清掉。
        if event.pressed_button != Some(MouseButton::Middle) {
            self.panning = None;
            cx.notify();
            return false;
        }
        self.panning = Some(event.position);
        self.pan_by(event.position - last, cx);
        true
    }

    pub(crate) fn on_page_middle_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.panning.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn on_page_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pan_with(event, cx) {
            cx.stop_propagation();
            return;
        }
        if !self.viewer.selection.dragging || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some(page) = self.viewer.selection.page else {
            return;
        };
        if let Some(index) = self.char_at_point(page, event.position) {
            self.viewer.selection.extend_to(index);
            cx.notify();
        }
    }

    pub(crate) fn on_page_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_dragging = self.viewer.selection.dragging;
        self.viewer.selection.dragging = false;
        // 选择结束（拖选或双击选词）后打印选区，便于测试选中文字是否正确。
        if was_dragging {
            self.print_selection();
        }
        cx.notify();
    }

    pub(crate) fn follow_link(&mut self, target: LinkTarget, cx: &mut Context<Self>) {
        match target {
            LinkTarget::Uri(uri) => {
                self.set_status(format!("打开链接 {uri}"), cx);
                cx.open_url(&uri);
            }
            LinkTarget::Page(page) => {
                self.set_page(page, cx);
            }
        }
        cx.notify();
    }

    // ---------- 选区与剪贴板 ----------

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        match self.viewer.selection.text(self.selection_chars()) {
            Some(text) if !text.is_empty() => {
                let count = text.chars().count();
                log::info!("copied {count} chars: {text:?}");
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.set_status(format!("已复制 {count} 个字符"), cx);
            }
            _ => {
                self.set_status("没有选中文本", cx);
            }
        }
        cx.notify();
    }

    /// 调试图：把当前选中的文字打到终端（`RUST_LOG` 默认 info），方便核对选区内容。
    fn print_selection(&self) {
        let Some(range) = self.viewer.selection.range() else {
            return;
        };
        let Some(text) = self.viewer.selection.text(self.selection_chars()) else {
            return;
        };
        log::info!(
            "selection page {} chars {}..={} ({}): {text:?}",
            self.viewer.selection.page.unwrap_or(0) + 1,
            range.start(),
            range.end(),
            text.chars().count(),
        );
    }

    /// 双击选词后直接查词。
    fn lookup_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.viewer.selection.text(self.selection_chars()) else {
            return;
        };
        self.lookup_word(text, cx);
    }

    // ---------- 页码跳转 ----------

    /// 点底部页码：把当前页填进输入框并请求焦点。
    pub(crate) fn open_page_input(&mut self, cx: &mut Context<Self>) {
        if !self.viewer.has_document() {
            return;
        }
        let page = (self.viewer.current_page + 1).to_string();
        self.page_input
            .update(cx, |input, cx| input.set_value(page, cx));
        self.page_input_open = true;
        self.pending_focus = Some(FocusTarget::Page);
        cx.notify();
    }

    /// Enter：解析、夹取并跳页；非法输入留在输入框里，不改变阅读位置。
    pub(crate) fn commit_page_input(&mut self, cx: &mut Context<Self>) {
        let text = self.page_input.read(cx).value().to_string();
        let Some(page) = crate::ui::chrome::parse_page_number(&text, self.viewer.page_count())
        else {
            self.set_status("请输入有效页码", cx);
            self.pending_focus = Some(FocusTarget::Page);
            return;
        };
        self.page_input_open = false;
        self.set_page(page, cx);
        self.pending_focus = Some(FocusTarget::Root);
        cx.notify();
    }

    /// Esc 或点到别处：收起输入框，把焦点还给阅读器。
    pub(crate) fn close_page_input(&mut self, cx: &mut Context<Self>) {
        self.page_input_open = false;
        self.pending_focus = Some(FocusTarget::Root);
        cx.notify();
    }

    // ---------- 键盘 ----------

    fn on_home_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let control = keystroke.modifiers.control || keystroke.modifiers.platform;

        // 搜索框有焦点时，字符输入归 TextInput，这里只处理 Ctrl+F（全选便于改写）；
        // 带 Ctrl 的全局快捷键继续放行。
        if self.home_input.read(cx).focus_handle(cx).is_focused(window) {
            if control && keystroke.key.eq_ignore_ascii_case("f") {
                self.home_input.update(cx, |input, cx| input.select_all(cx));
                return;
            }
            if !control {
                return;
            }
        }

        if control {
            match keystroke.key.as_str() {
                "o" => self.open_pdf(window, cx),
                "d" => self.toggle_theme(cx),
                "," => self.toggle_settings(cx),
                "q" => {
                    self.flush_progress();
                    cx.quit();
                }
                "f" => {
                    self.home.search_focused = true;
                    self.pending_focus = Some(FocusTarget::HomeSearch);
                    self.set_status("输入关键词筛选最近阅读，Esc 清空", cx);
                    cx.notify();
                }
                _ => {}
            }
            return;
        }

        match keystroke.key.as_str() {
            "escape" => {
                if self.settings_open {
                    self.toggle_settings(cx);
                } else if self.dictionary_window.is_some() {
                    self.close_dictionary_window(cx);
                } else {
                    self.clear_home_query(cx);
                    self.home.menu_open = None;
                    self.pending_focus = Some(FocusTarget::Root);
                }
            }
            "enter" => {
                if let Some(index) = self.home.filtered().first().copied() {
                    self.open_recent(index, cx);
                }
            }
            _ => {
                // 输入框没焦点时直接打字：先聚焦，再把这一下字符补进去。
                if let Some(key_char) = &keystroke.key_char
                    && !key_char.chars().all(char::is_control)
                {
                    let text = key_char.clone();
                    self.home.search_focused = true;
                    self.home.menu_open = None;
                    self.home_input
                        .update(cx, |input, cx| input.insert_text(&text, cx));
                    self.pending_focus = Some(FocusTarget::HomeSearch);
                }
            }
        }
    }

    pub(crate) fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.viewer.has_document() {
            self.on_home_key_down(event, window, cx);
            return;
        }

        let keystroke = &event.keystroke;
        let control = keystroke.modifiers.control || keystroke.modifiers.platform;

        // 页码输入框有焦点时，数字、退格、Enter、Esc 归 TextInput；否则
        // PageDown / H 之类的快捷键会把正在输入的数字当成翻页指令。
        if self.page_input_open
            && self.page_input.read(cx).focus_handle(cx).is_focused(window)
            && !control
        {
            return;
        }

        // 搜索框有焦点时，字符输入和编辑键都由 TextInput 处理，这里不能重复处理，
        // 也不能让 PageDown / H 之类的页面快捷键抢走空格、回车。带 Ctrl 的全局
        // 快捷键（打开、保存、缩放……）仍然放行。
        if self.viewer.search.open
            && self
                .search_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            if control && keystroke.key.eq_ignore_ascii_case("f") {
                self.close_search(cx);
                return;
            }
            if !control {
                return;
            }
        }

        if control && keystroke.key.eq_ignore_ascii_case("f") {
            if self.viewer.search.open {
                self.close_search(cx);
            } else {
                self.open_search(cx);
            }
            return;
        }

        if self.viewer.search.open {
            match keystroke.key.as_str() {
                "escape" => {
                    self.close_search(cx);
                    return;
                }
                "enter" => {
                    self.next_match(!keystroke.modifiers.shift, cx);
                    return;
                }
                _ => {}
            }
        }

        if control && keystroke.modifiers.shift && keystroke.key.eq_ignore_ascii_case("d") {
            self.toggle_dictionary_window(cx);
            return;
        }

        if control {
            match keystroke.key.as_str() {
                "o" => self.open_pdf(window, cx),
                "c" => self.copy_selection(cx),
                "=" | "+" => {
                    self.step_zoom(crate::layout::ZOOM_STEP_PERCENT, cx);
                    return;
                }
                "-" => {
                    self.step_zoom(-crate::layout::ZOOM_STEP_PERCENT, cx);
                    return;
                }
                "0" => {
                    self.fit_to_page(cx);
                    return;
                }
                "1" => {
                    self.fit_to_width(cx);
                    return;
                }
                "d" => {
                    self.toggle_theme(cx);
                    return;
                }
                "," => {
                    self.toggle_settings(cx);
                    return;
                }
                "b" => {
                    self.outline_open = !self.outline_open;
                    cx.notify();
                    return;
                }
                "s" => {
                    self.save_highlights(cx);
                    return;
                }
                "h" => {
                    self.toggle_highlight_panel(cx);
                    return;
                }
                "q" => {
                    self.flush_progress();
                    cx.quit();
                    return;
                }
                _ => {}
            }
        } else {
            match keystroke.key.as_str() {
                // 连续滚动下 PageDown / 空格滚动一屏，左右键才是整页跳。
                "pagedown" | "space" => {
                    self.scroll_by_viewport(1.0, cx);
                    return;
                }
                "pageup" => {
                    self.scroll_by_viewport(-1.0, cx);
                    return;
                }
                "right" => {
                    let next = self.viewer.current_page + 1;
                    self.set_page(next, cx);
                    return;
                }
                "left" => {
                    if self.viewer.current_page > 0 {
                        let previous = self.viewer.current_page - 1;
                        self.set_page(previous, cx);
                    }
                    return;
                }
                "home" => {
                    self.set_page(0, cx);
                    return;
                }
                "end" => {
                    let last = self.viewer.page_count().saturating_sub(1);
                    self.set_page(last, cx);
                    return;
                }
                "h" => {
                    self.add_highlight(cx);
                    return;
                }
                "escape" => {
                    if self.settings_open {
                        self.toggle_settings(cx);
                    } else if self.dictionary_window.is_some() {
                        self.close_dictionary_window(cx);
                    } else {
                        self.viewer.selection.clear();
                        self.outline_open = false;
                        cx.notify();
                    }
                    return;
                }
                _ => {}
            }
        }
    }
}

/// 字体字节的轻量签名：长度 + 内容哈希，用来避免反复配置时重复注册字体。
fn font_signature(bytes: &[u8]) -> (usize, u64) {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    (bytes.len(), hasher.finish())
}
