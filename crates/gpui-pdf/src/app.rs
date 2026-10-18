//! 应用控制器：把 PDF 引擎事件、键盘/鼠标输入接到 Viewer 状态和 UI 上。

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dict_core::{Command as DictCommand, DictEngine, Event as DictEvent};
use futures::StreamExt;
use gpui::{
    AnyWindowHandle, App, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions,
    Point, Render, RenderImage, Size, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions, div, point, prelude::*, px, size,
};
use pdf_core::{Command, Event, LinkTarget, PageSize, PdfEngine};

use crate::dictionary::{DictLoadState, DictionaryState, DictionaryView};
use crate::highlights::sidecar_path;
use crate::home::{HomeNav, HomeState};
use crate::layout::{
    DocumentLayout, FitArea, FitMode, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH, PREFETCH_PAGES,
    SCREEN_MARGIN, WINDOW_FILL, window_size_for_page,
};
use crate::progress::{ReadingProgress, unix_now};
use crate::settings::SettingsStore;
use crate::theme::Theme;
use crate::ui::dictionary::DictionaryWindow;
use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::viewer::{Document, PageData, Viewer};

/// 页内滚动没有独立事件，用固定间隔采样阅读位置。
const PROGRESS_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// 连续滚动时合并写盘，避免每一帧都碰磁盘。
const PROGRESS_FLUSH_DELAY: Duration = Duration::from_millis(800);
/// PageDown/PageUp 一次滚过视口高度的比例，留一点重叠方便接续阅读。
const PAGE_SCROLL_OVERLAP: f32 = 0.9;

/// 窗口当前还能长到多大（内容尺寸）。
///
/// 不能只按屏幕大小算：GPUI 的 `resize` 不会改窗口左上角位置，窗口靠下时
/// 再按整屏高度长就会伸出屏幕，所以要扣掉当前位置。
fn window_room(window: &Window, cx: &App) -> (f32, f32) {
    let work = window
        .display(cx)
        .map(|display| display.visible_bounds())
        .unwrap_or_else(|| Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(800.))));
    let current = window.bounds();

    let room_width = (f32::from(work.right()) - f32::from(current.origin.x) - SCREEN_MARGIN)
        .min(f32::from(work.size.width) * WINDOW_FILL)
        .max(MIN_WINDOW_WIDTH);
    let room_height = (f32::from(work.bottom()) - f32::from(current.origin.y) - SCREEN_MARGIN)
        .min(f32::from(work.size.height) * WINDOW_FILL)
        .max(MIN_WINDOW_HEIGHT);

    (room_width, room_height)
}

fn normalize_term(raw: &str) -> String {
    let term = raw.lines().next().unwrap_or_default().trim();
    let term = term.trim_matches(is_edge_punctuation);
    term.chars().take(64).collect()
}

fn rect_f(bounds: Bounds<gpui::Pixels>) -> crate::window_layout::RectF {
    crate::window_layout::RectF::new(
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    )
}

/// 词典窗口的尺寸：优先 380×720，屏幕放不下时按工作区收窄。
fn dictionary_window_size(work: crate::window_layout::RectF) -> Size<gpui::Pixels> {
    use crate::window_layout::SCREEN_MARGIN;

    let width = 380.0_f32.min((work.width - SCREEN_MARGIN * 2.0).max(280.0));
    let height = 720.0_f32.min((work.height - SCREEN_MARGIN * 2.0).max(360.0));
    size(px(width), px(height))
}

fn is_edge_punctuation(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '.' | ','
                | ';'
                | ':'
                | '!'
                | '?'
                | '"'
                | '\''
                | '“'
                | '”'
                | '‘'
                | '’'
                | '('
                | ')'
                | '（'
                | '）'
                | '['
                | ']'
                | '{'
                | '}'
                | '《'
                | '》'
                | '「'
                | '」'
                | '【'
                | '】'
                | '。'
                | '，'
                | '、'
                | '；'
                | '：'
                | '！'
                | '？'
                | '…'
                | '—'
                | '–'
        )
}

/// 输入框需要焦点时，交给下一次 `render` 去设置（事件回调里拿不到 `Window`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FocusTarget {
    Root,
    Search,
    HomeSearch,
}

pub struct AppState {
    pub(crate) theme: Theme,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) engine: std::sync::mpsc::Sender<Command>,
    pub(crate) viewer: Viewer,
    pub(crate) home: HomeState,
    pub(crate) search_input: Entity<TextInput>,
    pub(crate) home_input: Entity<TextInput>,
    pub(crate) pending_focus: Option<FocusTarget>,
    pub(crate) settings: SettingsStore,
    pub(crate) settings_open: bool,
    pub(crate) dict: DictEngine,
    /// 词典窗口里手动输入查词时，把词送回主窗口。
    dict_query: futures::channel::mpsc::UnboundedSender<String>,
    pub(crate) dictionary: DictionaryState,
    /// 独立词典窗口；关闭后保留的句柄会在下一次同步时清掉。
    pub(crate) dictionary_window: Option<WindowHandle<DictionaryWindow>>,
    /// 主窗口与显示器工作区，用来把词典窗口放到主窗口右侧。
    main_window_bounds: Option<Bounds<gpui::Pixels>>,
    display_bounds: Option<Bounds<gpui::Pixels>>,
    pub(crate) viewport: Size<gpui::Pixels>,
    pub(crate) scale_factor: f32,
    pub(crate) request_id: u64,
    /// 全文搜索的请求代号，用来丢掉过期结果。
    search_request_id: u64,
    pub(crate) outline_open: bool,
    /// 批注（高亮）侧栏是否展开。
    pub(crate) highlight_panel_open: bool,
    pub(crate) status: String,
    pub(crate) status_version: u64,
    /// 当前文档上一次采样到的阅读位置，用来跳过重复写盘。
    last_progress: Option<ReadingProgress>,
    /// 是否已经安排了一次延迟写盘。
    progress_flush_scheduled: bool,
    /// 打开文档后按纸张比例调整窗口；在 `render` 里执行（那里能拿到 `Window`）。
    pending_window_fit: Option<PageSize>,
    /// 回到首页时恢复阅读前的窗口尺寸。
    pending_window_resize: Option<Size<gpui::Pixels>>,
    /// 进入阅读器之前的窗口尺寸，关闭文档时恢复。
    reader_window_size: Option<Size<gpui::Pixels>>,
}

impl AppState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (event_tx, mut event_rx) = futures::channel::mpsc::unbounded();
        let engine = PdfEngine::spawn(event_tx).commands;

        cx.spawn(async move |this, cx| {
            while let Some(event) = event_rx.next().await {
                if this
                    .update(cx, |this, cx| this.handle_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let settings = SettingsStore::load();
        let theme = if settings.current.dark {
            Theme::dark()
        } else {
            Theme::light()
        };

        let (dict_tx, mut dict_rx) = futures::channel::mpsc::unbounded();
        let dict = DictEngine::spawn(dict_tx);
        if let Some(path) = settings.current.dictionary_path.clone() {
            let _ = dict.commands.send(DictCommand::Load { path });
        }

        cx.spawn(async move |this, cx| {
            while let Some(event) = dict_rx.next().await {
                if this
                    .update(cx, |this, cx| this.handle_dict_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let mut dictionary = DictionaryState::default();
        if settings.current.dictionary_path.is_some() {
            dictionary.load_state = DictLoadState::Loading;
        }

        // 词典窗口自己也有输入框：它只负责把词发回来，查词状态仍由主窗口维护。
        let (dict_query, mut dict_query_rx) = futures::channel::mpsc::unbounded::<String>();
        cx.spawn(async move |this, cx| {
            while let Some(term) = dict_query_rx.next().await {
                if this
                    .update(cx, |this, cx| this.lookup_word(term, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // 页内滚动不会触发专门的事件，按固定间隔采样，空闲时立即停手。
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(PROGRESS_SAMPLE_INTERVAL)
                    .await;
                if this
                    .update(cx, |this, cx| this.sample_progress(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // 词典窗口自己关掉时（✕ 或 Alt+F4），同步主窗口里的状态和工具栏标签。
        //
        // 关闭回调可能在主窗口自己触发的 `remove_window` 里同步执行，这时直接
        // `AppState::update` 会撞上还借着的实体；推迟到本轮事件处理结束后再改。
        let app = cx.entity();
        cx.on_window_closed(move |cx, window_id| {
            let app = app.clone();
            cx.defer(move |cx| {
                let _ = app.update(cx, |this, cx| {
                    let closed = this.dictionary_window.is_some_and(|handle| {
                        AnyWindowHandle::from(handle).window_id() == window_id
                    });
                    if closed {
                        this.dictionary_window = None;
                        this.dictionary.open = false;
                        cx.notify();
                    }
                });
            });
        })
        .detach();

        // 关窗/退出时兜底保存一次，避免最后不到一秒的滚动丢失。
        cx.on_release(|this, cx| {
            this.flush_progress();
            if let Some(handle) = this.dictionary_window.take() {
                let _ = handle.update(cx, |_view, window, _cx| window.remove_window());
            }
        })
        .detach();

        let search_input = cx.new(|cx| {
            TextInput::new("输入关键词，Enter 下一个", theme.clone(), 13.0, cx)
        });
        let home_input = cx.new(|cx| TextInput::new("极简 PDF", theme.clone(), 13.0, cx));

        cx.subscribe(&search_input, |this, _input, event: &TextInputEvent, cx| {
            match event {
                TextInputEvent::Changed => {
                    this.viewer.search.query = this.search_input.read(cx).value().to_string();
                    this.on_search_query_changed(cx);
                }
                TextInputEvent::Submitted { forward } => this.next_match(*forward, cx),
                TextInputEvent::Cancelled => this.close_search(cx),
            }
        })
        .detach();

        cx.subscribe(&home_input, |this, _input, event: &TextInputEvent, cx| {
            match event {
                TextInputEvent::Changed => {
                    this.home.query = this.home_input.read(cx).value().to_string();
                    this.home.search_focused = true;
                    this.home.menu_open = None;
                    cx.notify();
                }
                TextInputEvent::Submitted { .. } => {
                    if let Some(index) = this.home.filtered().first().copied() {
                        this.open_recent(index, cx);
                    }
                }
                TextInputEvent::Cancelled => this.clear_home_query(cx),
            }
        })
        .detach();

        Self {
            theme,
            focus_handle: cx.focus_handle(),
            engine,
            viewer: Viewer::new(),
            home: HomeState::load(),
            search_input,
            home_input,
            pending_focus: None,
            settings,
            settings_open: false,
            dict,
            dict_query,
            dictionary,
            dictionary_window: None,
            main_window_bounds: None,
            display_bounds: None,
            viewport: Size {
                width: px(1040.),
                height: px(720.),
            },
            scale_factor: 1.0,
            request_id: 0,
            search_request_id: 0,
            outline_open: false,
            highlight_panel_open: false,
            status: String::new(),
            status_version: 0,
            last_progress: None,
            progress_flush_scheduled: false,
            pending_window_fit: None,
            pending_window_resize: None,
            reader_window_size: None,
        }
    }

    // ---------- 阅读进度 ----------

    /// 采样当前阅读位置；有变化时更新内存记录并安排延迟写盘。
    fn sample_progress(&mut self, cx: &mut Context<Self>) {
        if self.capture_progress() {
            self.schedule_progress_flush(cx);
        }
    }

    /// 把当前页、页内滚动比例和缩放写进最近阅读（只改内存）。
    ///
    /// 返回 `false` 表示没有文档或位置没变。
    fn capture_progress(&mut self) -> bool {
        let Some(progress) = self.current_progress() else {
            return false;
        };
        if self
            .last_progress
            .is_some_and(|last| last.same_position(&progress))
        {
            return false;
        }
        self.last_progress = Some(progress);
        if let Some(path) = self.viewer.doc.as_ref().map(|doc| doc.path.clone()) {
            self.home.set_progress(&path, progress);
        }
        true
    }

    fn current_progress(&self) -> Option<ReadingProgress> {
        let doc = self.viewer.doc.as_ref()?;
        let layout = self.doc_layout()?;
        let (page, ratio_y) = self.reading_anchor(&layout);
        // serde_json 不接受 NaN/Infinity，缩放异常时退回 1.0。
        let zoom = if self.viewer.zoom.is_finite() {
            self.viewer.zoom
        } else {
            1.0
        };
        Some(ReadingProgress {
            page,
            pages: doc.pages.len(),
            zoom,
            // 新记录总是带模式，包括手动缩放（free）；缺字段表示旧版本。
            fit: Some(self.viewer.fit_mode),
            scroll_x: self.scroll_ratio_x(&layout),
            scroll_y: ratio_y,
            updated_at: unix_now(),
        })
    }

    /// 立即把进度落盘（切页、缩放、回到首页、退出时调用）。
    pub(crate) fn flush_progress(&mut self) {
        self.capture_progress();
        self.home.flush();
    }

    fn schedule_progress_flush(&mut self, cx: &mut Context<Self>) {
        if self.progress_flush_scheduled {
            return;
        }
        self.progress_flush_scheduled = true;

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PROGRESS_FLUSH_DELAY).await;
            let _ = this.update(cx, |this, _cx| {
                this.progress_flush_scheduled = false;
                this.home.flush();
            });
        })
        .detach();
    }

    /// 用当前视口把恢复出来的页内比例换算成滚动偏移。
    fn apply_pending_scroll(&mut self) {
        let Some((page, ratio_x, ratio_y)) = self.viewer.take_pending_scroll() else {
            return;
        };
        let Some(layout) = self.doc_layout() else {
            return;
        };
        self.scroll_to_anchor(&layout, page, ratio_y);

        // 横向：页面比视口宽时按比例还原，否则保持最左。
        let max_x = (layout.content_width - f32::from(self.viewport.width)).max(0.0);
        if max_x > 0.0 {
            let mut offset = self.viewer.scroll.offset();
            offset.x = px(-ratio_x.clamp(0.0, 1.0) * max_x);
            self.viewer.scroll.set_offset(offset);
        }
    }

    // ---------- 滚动几何 ----------

    /// 当前缩放和视口下的整篇排版。
    ///
    /// 每页只算几下浮点，一帧调用两三次（同步页码、补渲染、画页面）不构成瓶颈；
    /// 不缓存是有意的：缩放、窗口宽度、文档任一变化都不必操心失效。
    pub(crate) fn doc_layout(&self) -> Option<DocumentLayout> {
        let doc = self.viewer.doc.as_ref()?;
        Some(DocumentLayout::new(
            &doc.pages,
            self.viewer.zoom,
            f32::from(self.viewport.width),
        ))
    }

    /// 滚动内容上边缘在内容坐标里的位置；0 表示贴顶。
    pub(crate) fn scroll_top(&self) -> f32 {
        -f32::from(self.viewer.scroll.offset().y)
    }

    /// 设置滚动顶部位置，夹在内容范围内。
    fn set_scroll_top(&self, layout: &DocumentLayout, top: f32) {
        let top = layout.clamp_scroll_top(top, f32::from(self.viewport.height));
        let mut offset = self.viewer.scroll.offset();
        offset.y = px(-top);
        self.viewer.scroll.set_offset(offset);
    }

    /// 阅读锚点：视口中线落在第几页、页内比例多少。
    ///
    /// 页码判定、进度保存、缩放前后对齐位置都用这一个锚点。
    fn reading_anchor(&self, layout: &DocumentLayout) -> (usize, f32) {
        layout.anchor_at(self.scroll_top(), f32::from(self.viewport.height))
    }

    /// 把锚点挪到「第 `page` 页的 `ratio` 处」。
    fn scroll_to_anchor(&self, layout: &DocumentLayout, page: usize, ratio: f32) {
        let top = layout.scroll_top_for_anchor(page, ratio, f32::from(self.viewport.height));
        self.set_scroll_top(layout, top);
    }

    /// 横向滚动比例：0 是最左，1 是最右。
    fn scroll_ratio_x(&self, layout: &DocumentLayout) -> f32 {
        let max = (layout.content_width - f32::from(self.viewport.width)).max(0.0);
        if max <= 0.0 {
            return 0.0;
        }
        let ratio = (-f32::from(self.viewer.scroll.offset().x) / max).clamp(0.0, 1.0);
        if ratio == 0.0 { 0.0 } else { ratio }
    }

    /// 按视口高度滚动一屏（PageDown / PageUp / 空格）。
    fn scroll_by_viewport(&mut self, direction: f32, cx: &mut Context<Self>) {
        let Some(layout) = self.doc_layout() else {
            return;
        };
        let step = f32::from(self.viewport.height) * PAGE_SCROLL_OVERLAP;
        self.set_scroll_top(&layout, self.scroll_top() + direction * step);
        self.ensure_visible_pages();
        self.sync_current_page();
        if self.capture_progress() {
            self.schedule_progress_flush(cx);
        }
        cx.notify();
    }

    // ---------- 文档与渲染 ----------

    pub(crate) fn open_pdf(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 PDF 文件".into()),
        });
        let engine = self.engine.clone();

        cx.spawn(async move |_this, _cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = engine.send(Command::Open(path));
            }
        })
        .detach();
    }

    /// 从命令行/外部直接打开一个 PDF。
    pub fn open_path(&mut self, path: PathBuf) {
        let _ = self.engine.send(Command::Open(path));
    }

    /// 打开「最近阅读」中的一条；文件已被移动时自动移除。
    pub(crate) fn open_recent(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(recent) = self.home.recents.get(index).cloned() else {
            return;
        };
        self.home.menu_open = None;
        self.home.search_focused = false;
        if !recent.path.is_file() {
            self.home.remove(index);
            self.set_status("文件已不存在，已从最近阅读移除", cx);
            cx.notify();
            return;
        }

        self.open_path(recent.path);
    }

    /// 清空首页筛选（内含让输入框同步清空）。
    pub(crate) fn clear_home_query(&mut self, cx: &mut Context<Self>) {
        self.home_input.update(cx, |input, cx| input.clear(cx));
        self.home.query.clear();
        self.home.search_focused = false;
        self.home.menu_open = None;
        cx.notify();
    }

    /// 处理拖进窗口的文件，只接受第一个 PDF。
    pub(crate) fn open_dropped(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let pdf = paths
            .iter()
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
            })
            .cloned();

        match pdf {
            Some(path) => self.open_path(path),
            None => {
                self.set_status("请拖入 PDF 文件", cx);
                cx.notify();
            }
        }
    }

    pub(crate) fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.theme = self.theme.clone().toggled();
        self.settings.current.dark = self.theme.dark;
        self.settings.save();
        let theme = self.theme.clone();
        self.search_input
            .update(cx, |input, cx| input.set_theme(theme.clone(), cx));
        self.home_input
            .update(cx, |input, cx| input.set_theme(theme, cx));
        self.sync_dictionary_window(cx);
        self.set_status(format!("主题：{}", self.theme.name()), cx);
        cx.notify();
    }

    pub(crate) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = !self.settings_open;
        if self.settings_open {
            self.home.menu_open = None;
            self.home.search_focused = false;
        } else if !self.viewer.has_document() {
            self.home.nav = HomeNav::Home;
        }
        cx.notify();
    }

    // ---------- 词典 ----------

    pub(crate) fn handle_dict_event(&mut self, event: DictEvent, cx: &mut Context<Self>) {
        match event {
            DictEvent::Loaded {
                path: _,
                title,
                entries,
                fonts,
            } => {
                if self.settings.current.dictionary_path.is_none() {
                    return;
                }
                if !fonts.is_empty() {
                    let count = fonts.len();
                    let fonts: Vec<Cow<'static, [u8]>> =
                        fonts.into_iter().map(Cow::Owned).collect();
                    match cx.text_system().add_fonts(fonts) {
                        Ok(()) => log::info!("registered {count} dictionary fonts"),
                        Err(error) => log::warn!("注册词典字体失败: {error:#}"),
                    }
                }
                self.dictionary.load_state = DictLoadState::Ready { title, entries };
                self.dictionary.loading = false;
                if let Some(term) = self.dictionary.pending_term.take() {
                    self.lookup_word(term, cx);
                    return;
                }
                self.set_status(format!("词典已加载 · {entries} 条"), cx);
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
            DictEvent::Error { request, message } => {
                if let Some(request) = request
                    && request != self.dictionary.request
                {
                    return;
                }
                self.dictionary.loading = false;
                if request.is_none() {
                    if self.settings.current.dictionary_path.is_none() {
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

    pub(crate) fn lookup_word(&mut self, term: String, cx: &mut Context<Self>) {
        let term = normalize_term(&term);
        if term.is_empty() {
            return;
        }

        if self.settings.current.dictionary_path.is_none() {
            self.settings_open = true;
            self.set_status("请先在设置中选择 MDX 词典", cx);
            cx.notify();
            return;
        }

        match self.dictionary.load_state.clone() {
            DictLoadState::Loading => {
                self.dictionary.pending_term = Some(term);
                self.dictionary.open = true;
                self.ensure_dictionary_window(cx);
                self.set_status("词典正在加载…", cx);
                cx.notify();
                return;
            }
            DictLoadState::Unconfigured => {
                self.settings_open = true;
                self.set_status("请先在设置中选择 MDX 词典", cx);
                cx.notify();
                return;
            }
            DictLoadState::Failed(message) => {
                self.dictionary.error = Some(message.clone());
                self.dictionary.open = true;
                self.ensure_dictionary_window(cx);
                self.set_status(message, cx);
                cx.notify();
                return;
            }
            DictLoadState::Ready { .. } => {}
        }

        self.dictionary.request = self.dictionary.request.wrapping_add(1);
        let request = self.dictionary.request;
        self.dictionary.word = term.clone();
        self.dictionary.loading = true;
        self.dictionary.not_found = false;
        self.dictionary.error = None;
        self.dictionary.entries.clear();
        self.dictionary.pending_term = None;
        self.dictionary.open = true;
        self.ensure_dictionary_window(cx);

        if let Err(error) = self
            .dict
            .commands
            .send(DictCommand::Lookup { request, term })
        {
            self.dictionary.loading = false;
            self.dictionary.error = Some(format!("词典线程不可用: {error}"));
            self.sync_dictionary_window(cx);
        }
        cx.notify();
    }

    fn lookup_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.viewer.selection.text(self.selection_chars()) else {
            return;
        };
        self.lookup_word(text, cx);
    }

    pub(crate) fn toggle_dictionary_window(&mut self, cx: &mut Context<Self>) {
        let alive = self
            .dictionary_window
            .is_some_and(|handle| handle.update(cx, |_view, _window, _cx| ()).is_ok());

        if alive {
            self.close_dictionary_window(cx);
            return;
        }

        self.dictionary_window = None;
        if self.settings.current.dictionary_path.is_none() {
            self.settings_open = true;
            self.set_status("请先在设置中选择 MDX 词典", cx);
            cx.notify();
            return;
        }

        self.ensure_dictionary_window(cx);
        cx.notify();
    }

    pub(crate) fn ensure_dictionary_window(&mut self, cx: &mut Context<Self>) {
        let view = DictionaryView::from(&self.dictionary);
        let theme = self.theme.clone();

        if let Some(handle) = self.dictionary_window {
            match handle.update(cx, |window, _window, cx| window.apply(view, theme, cx)) {
                Ok(()) => return,
                Err(_) => self.dictionary_window = None,
            }
        }

        let bounds = match (self.main_window_bounds, self.display_bounds) {
            (Some(main), Some(display)) => {
                let work = rect_f(display);
                let window_size = dictionary_window_size(work);
                let (x, y) = crate::window_layout::adjacent_origin(
                    rect_f(main),
                    (f32::from(window_size.width), f32::from(window_size.height)),
                    work,
                    12.0,
                );
                Bounds::new(point(px(x), px(y)), window_size)
            }
            _ => Bounds::centered(
                None,
                dictionary_window_size(crate::window_layout::RectF::new(0.0, 0.0, 1920.0, 1080.0)),
                cx,
            ),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("词典".into()),
                // 和主窗口一样自绘标题栏，不要系统标题栏。
                appears_transparent: true,
                ..Default::default()
            }),
            is_minimizable: false,
            // 不抢主窗口的键盘焦点：查词时阅读器快捷键仍然可用。
            focus: false,
            window_min_size: Some(size(px(280.), px(320.))),
            ..Default::default()
        };

        let view = DictionaryView::from(&self.dictionary);
        let theme = self.theme.clone();
        let dict_query = self.dict_query.clone();

        match cx.open_window(options, move |_window, cx| {
            cx.new(|cx| DictionaryWindow::new(view, theme, dict_query, cx))
        }) {
            Ok(handle) => {
                self.dictionary_window = Some(handle);
                self.dictionary.open = true;
                log::info!("dictionary window opened");
            }
            Err(error) => {
                self.dictionary.open = false;
                self.set_status(format!("无法打开词典窗口: {error}"), cx);
            }
        }
    }

    fn sync_dictionary_window(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.dictionary_window else {
            return;
        };
        let view = DictionaryView::from(&self.dictionary);
        let theme = self.theme.clone();

        if handle
            .update(cx, |window, _window, cx| window.apply(view, theme, cx))
            .is_err()
        {
            self.dictionary_window = None;
            self.dictionary.open = false;
        }
    }

    pub(crate) fn close_dictionary_window(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.dictionary_window.take() {
            let _ = handle.update(cx, |_window, window, _cx| window.remove_window());
        }
        self.dictionary.open = false;
        cx.notify();
    }

    pub(crate) fn choose_dictionary(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 MDX 词典".into()),
        });

        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| this.set_dictionary_path(path, cx));
            }
        })
        .detach();
    }

    fn set_dictionary_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let is_mdx = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mdx"));
        if !is_mdx || !path.is_file() {
            self.set_status("请选择 .mdx 词典文件", cx);
            cx.notify();
            return;
        }

        self.settings.current.dictionary_path = Some(path.clone());
        self.settings.save();
        self.dictionary.clear_results();
        self.dictionary.load_state = DictLoadState::Loading;
        self.settings_open = false;
        self.sync_dictionary_window(cx);

        let _ = self.dict.commands.send(DictCommand::Load { path });
        self.set_status("正在加载词典…", cx);
        cx.notify();
    }

    pub(crate) fn clear_dictionary(&mut self, cx: &mut Context<Self>) {
        self.settings.current.dictionary_path = None;
        self.settings.save();
        let _ = self.dict.commands.send(DictCommand::Unload);
        self.dictionary.clear_results();
        self.dictionary.load_state = DictLoadState::Unconfigured;
        self.sync_dictionary_window(cx);
        self.set_status("已清除词典", cx);
        cx.notify();
    }

    /// 左下角浮动提示，4 秒后自动消失。
    pub(crate) fn set_status(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.status = text.into();
        self.status_version = self.status_version.wrapping_add(1);
        let version = self.status_version;

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;

            let _ = this.update(cx, |this, cx| {
                if this.status_version == version {
                    this.status.clear();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // ---------- 渲染调度 ----------

    /// 请求渲染一页；同一缩放已在跑或已有同位图时直接跳过。
    fn ensure_page(&mut self, page: usize) {
        if page >= self.viewer.page_count() {
            return;
        }
        let scale = (self.viewer.zoom * self.scale_factor).clamp(0.1, 8.0);
        let request = self.request_id + 1;
        if !self.viewer.begin_request(page, request, scale) {
            return;
        }
        self.request_id = request;
        if let Err(error) = self.engine.send(Command::Render {
            page,
            scale,
            request,
        }) {
            log::warn!("渲染请求发送失败: {error}");
        }
    }

    /// 按可视范围补渲染，并淘汰离得远的位图。
    ///
    /// 每一帧都会调用，靠 `Viewer::begin_request` 去重，所以不会重复排队。
    fn ensure_visible_pages(&mut self) {
        let Some(layout) = self.doc_layout() else {
            return;
        };
        let range = layout.visible_range(
            self.scroll_top(),
            f32::from(self.viewport.height),
            PREFETCH_PAGES,
        );
        for page in range.clone() {
            self.ensure_page(page);
        }
        self.viewer.evict(range);
    }

    /// 滚动后按锚线更新当前页：读到哪里，页码和搜索高亮就跟到哪里。
    fn sync_current_page(&mut self) {
        let Some(layout) = self.doc_layout() else {
            return;
        };
        let (page, _) = self.reading_anchor(&layout);
        if page == self.viewer.current_page {
            return;
        }
        self.viewer.current_page = page;
        self.refresh_matches(page);
    }

    /// 重算某一页的搜索匹配：当前页更新 `matches`，其他页只更新逐页缓存。
    fn refresh_matches(&mut self, page: usize) {
        self.viewer.refresh_search(page);
    }

    /// 跳到某一页：滚到页顶，并立刻安排这一页的渲染。
    pub(crate) fn set_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if !self.viewer.set_page(page) {
            return;
        }
        if let Some(layout) = self.doc_layout() {
            self.scroll_to_anchor(&layout, page, 0.0);
        }
        self.ensure_visible_pages();
        self.refresh_matches(page);
        self.flush_progress();
        cx.notify();
    }

    pub(crate) fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        // 缩放前后保持锚点不动，读者视线不会因缩放跳走。
        let anchor = self.doc_layout().map(|layout| self.reading_anchor(&layout));
        self.viewer.set_zoom(zoom);
        if let (Some(layout), Some((page, ratio))) = (self.doc_layout(), anchor) {
            self.scroll_to_anchor(&layout, page, ratio);
        }
        self.ensure_visible_pages();
        self.flush_progress();
        cx.notify();
    }

    // ---------- 适宽 / 适页 / 窗口尺寸 ----------

    fn fit_area(&self) -> FitArea {
        FitArea::from_viewport(
            f32::from(self.viewport.width),
            f32::from(self.viewport.height),
        )
    }

    /// 视口变化后按当前适宽/适页模式重算缩放，并把阅读位置留在原来的锚点上。
    ///
    /// 返回 `true` 表示缩放或模式变了，调用方需要重新检查可视页。
    fn refit(&mut self) -> bool {
        let mode = self.viewer.fit_mode;
        if mode == FitMode::Free {
            return false;
        }

        let anchor = self.doc_layout().map(|layout| self.reading_anchor(&layout));
        if !self.viewer.fit(mode, self.fit_area()) {
            return false;
        }
        if let (Some(layout), Some((page, ratio))) = (self.doc_layout(), anchor) {
            self.scroll_to_anchor(&layout, page, ratio);
        }
        true
    }

    /// 用户主动切换适宽/适页：重算缩放并立即重渲染。
    pub(crate) fn apply_fit(&mut self, mode: FitMode, cx: &mut Context<Self>) {
        let anchor = self.doc_layout().map(|layout| self.reading_anchor(&layout));
        if !self.viewer.fit(mode, self.fit_area()) {
            return;
        }
        if let (Some(layout), Some((page, ratio))) = (self.doc_layout(), anchor) {
            self.scroll_to_anchor(&layout, page, ratio);
        }
        self.ensure_visible_pages();
        self.flush_progress();
        cx.notify();
    }

    pub(crate) fn fit_to_page(&mut self, cx: &mut Context<Self>) {
        self.apply_fit(FitMode::Page, cx);
    }

    pub(crate) fn fit_to_width(&mut self, cx: &mut Context<Self>) {
        self.apply_fit(FitMode::Width, cx);
    }

    /// 打开文档后按纸张比例调窗口，或回到首页时恢复原窗口尺寸。
    ///
    /// 窗口尺寸要在 `Render` 里改，因为事件处理拿不到 `Window`。
    fn apply_pending_window_resize(&mut self, window: &mut Window, cx: &App) {
        let page = self.pending_window_fit.take();
        let restore = self.pending_window_resize.take();

        // 最大化/全屏时不要改窗口尺寸，整页适应会直接用当前视口。
        if window.is_maximized() || window.is_fullscreen() {
            return;
        }

        if let Some(page) = page {
            if self.reader_window_size.is_none() {
                // 记内容尺寸，`resize` 接收的也是内容尺寸，避免把边框算进去。
                self.reader_window_size = Some(window.viewport_size());
            }

            let (room_width, room_height) = window_room(window, cx);
            let (width, height) = window_size_for_page(page, room_width, room_height);
            log::info!(
                "fit window to page {:.0}x{:.0}: {width:.0}x{height:.0}",
                page.width,
                page.height
            );
            window.resize(size(px(width), px(height)));
        } else if let Some(target) = restore {
            window.resize(target);
        }
    }

    pub(crate) fn close_document(&mut self, cx: &mut Context<Self>) {
        self.flush_progress();
        self.viewer.close();
        self.search_input.update(cx, |input, cx| input.clear(cx));
        self.pending_focus = Some(FocusTarget::Root);
        self.last_progress = None;
        self.outline_open = false;
        self.settings_open = false;
        self.dictionary.clear_results();
        self.close_dictionary_window(cx);
        self.home.menu_open = None;
        self.home.search_focused = false;
        self.home.query.clear();
        self.home_input.update(cx, |input, cx| input.clear(cx));
        self.pending_window_fit = None;
        self.pending_window_resize = self.reader_window_size.take();
        cx.notify();
    }

    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Opened {
                path,
                pages,
                outline,
            } => {
                // 切换文档前先记下上一份文档的最后位置，record 会顺带落盘。
                self.capture_progress();

                let count = pages.len();
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                let saved = self.home.progress_for(&path);

                let area = self.fit_area();
                self.home.record(path.clone(), name.clone());
                self.home.query.clear();
                self.home.search_focused = false;
                self.dictionary.clear_results();
                self.sync_dictionary_window(cx);
                self.search_input.update(cx, |input, cx| input.clear(cx));
                self.viewer.reset(
                    Document {
                        path,
                        pages,
                        outline,
                    },
                    area,
                );

                let restored =
                    saved.is_some_and(|progress| self.viewer.restore_progress(&progress, area));
                self.apply_pending_scroll();

                // 等 render 拿到当前显示器工作区后，按这张纸的比例调整窗口；
                // 随后视口变化会再按适宽/适页模式重算缩放。
                self.pending_window_fit = self.viewer.page_size_at(0).filter(|page| {
                    page.width.is_finite()
                        && page.height.is_finite()
                        && page.width > 0.0
                        && page.height > 0.0
                });
                self.pending_window_resize = None;
                // 恢复成功时以保存的位置为基线，等布局完成后采样再修正；
                // 失败时清空基线，让下一次采样覆盖掉过期的旧进度。
                self.last_progress = if restored { saved } else { None };

                if let Some(doc) = &self.viewer.doc {
                    let path = sidecar_path(&doc.path);
                    self.viewer.highlights.load(&path);
                }

                self.ensure_visible_pages();
                if restored {
                    self.set_status(
                        format!(
                            "已恢复到第 {} / {} 页 · {name}",
                            self.viewer.current_page + 1,
                            count
                        ),
                        cx,
                    );
                } else {
                    self.set_status(format!("{name} · {count} 页"), cx);
                }
                log::info!(
                    "opened {} ({} pages, {} outline items, restored: {restored})",
                    name,
                    count,
                    self.viewer.doc.as_ref().map_or(0, |doc| doc.outline.len())
                );
            }
            Event::Rendered {
                page,
                request,
                width,
                height,
                bgra,
                chars,
                links,
            } => {
                // 只认还没作废的请求：缩放变了或页已经滚走，旧位图直接丢掉。
                let Some(scale) = self.viewer.take_request(page, request) else {
                    return;
                };
                let Some(buffer) = image::RgbaImage::from_raw(width, height, bgra) else {
                    self.set_status("页面位图格式错误", cx);
                    cx.notify();
                    return;
                };
                let render_image = RenderImage::new([image::Frame::new(buffer)]);
                log::info!(
                    "rendered page {}: {}x{}, {} chars, {} links",
                    page,
                    width,
                    height,
                    chars.len(),
                    links.len()
                );
                self.viewer.store_page(
                    page,
                    PageData {
                        image: Arc::new(render_image),
                        chars,
                        links,
                        scale,
                    },
                );
                self.refresh_matches(page);
            }
            Event::SearchResults {
                request,
                query,
                pages,
                total,
            } => {
                if request != self.search_request_id || query != self.viewer.search.query {
                    return;
                }
                self.viewer.search.set_doc_hits(pages, total);
                // 从当前阅读位置往前找：第一次 Enter 跳到本页或之后的第一处命中。
                self.viewer.search.rewind_to(self.viewer.current_page);
                log::debug!(
                    "full-text search for {:?}: {} hits on {} pages",
                    query,
                    total,
                    self.viewer.search.pages.len()
                );
                cx.notify();
            }
            Event::Error(message) => {
                log::error!("{message}");
                self.set_status(message, cx);
            }
        }
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
    fn selection_chars(&self) -> &[pdf_core::CharBox] {
        self.viewer
            .selection
            .page
            .map(|page| self.viewer.chars_of(page))
            .unwrap_or_default()
    }

    // ---------- 选择 / 高亮 / 搜索 ----------

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

    fn select_word_at(&mut self, page: usize, index: usize) {
        self.viewer.select_word_at(page, index);
    }

    pub(crate) fn add_highlight(&mut self, cx: &mut Context<Self>) {
        // 高亮跟着选区走：在哪一页选的，就写到哪一页。
        let Some(page) = self.viewer.selection.page else {
            self.set_status("先选中要标注的文本", cx);
            cx.notify();
            return;
        };
        let chars = self.viewer.chars_of(page);
        let rects = self.viewer.selection.rects(chars);
        if rects.is_empty() {
            self.set_status("先选中要标注的文本", cx);
            cx.notify();
            return;
        }

        let text = self.viewer.selection.text(chars).unwrap_or_default();
        let added = self.viewer.highlights.add(page, rects, text).is_some();
        if added {
            self.set_status("已添加高亮，Ctrl+S 保存", cx);
        }
        cx.notify();
    }

    /// 批注侧栏开关。
    pub(crate) fn toggle_highlight_panel(&mut self, cx: &mut Context<Self>) {
        self.highlight_panel_open = !self.highlight_panel_open;
        if self.highlight_panel_open {
            self.outline_open = false;
        }
        cx.notify();
    }

    /// 删除一条高亮并立即写盘，避免下次打开又冒出来。
    pub(crate) fn delete_highlight(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.viewer.highlights.remove(id) {
            return;
        }
        match self.persist_highlights() {
            Ok(()) => self.set_status("已删除高亮", cx),
            Err(error) => self.set_status(format!("删除后保存失败: {error}"), cx),
        }
        cx.notify();
    }

    /// 写 sidecar，不弹提示。
    fn persist_highlights(&self) -> Result<(), String> {
        let Some(doc) = &self.viewer.doc else {
            return Ok(());
        };
        let path = sidecar_path(&doc.path);
        self.viewer
            .highlights
            .save(&path)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn save_highlights(&mut self, cx: &mut Context<Self>) {
        if self.viewer.highlights.is_empty() {
            self.set_status("还没有高亮", cx);
            cx.notify();
            return;
        }

        match self.persist_highlights() {
            Ok(()) => {
                let path = self
                    .viewer
                    .doc
                    .as_ref()
                    .map(|doc| sidecar_path(&doc.path))
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                self.set_status(format!("高亮已保存到 {path}"), cx);
            }
            Err(error) => self.set_status(format!("保存高亮失败: {error}"), cx),
        }
        cx.notify();
    }

    /// 重新计算当前页的搜索匹配（查询变了、切页后、页面刚渲染完都要调）。
    fn recompute_matches(&mut self, cx: &mut Context<Self>) {
        let page = self.viewer.current_page;
        self.refresh_matches(page);
        cx.notify();
    }

    /// 打开搜索浮层并把焦点交给输入框。
    pub(crate) fn open_search(&mut self, cx: &mut Context<Self>) {
        self.viewer.search.open = true;
        self.outline_open = false;
        let value = self.viewer.search.query.clone();
        self.search_input.update(cx, |input, cx| {
            input.set_value(value, cx);
            input.select_all(cx);
        });
        self.pending_focus = Some(FocusTarget::Search);
        cx.notify();
    }

    /// 关闭搜索：清空查询、把焦点还给页面，页面快捷键恢复。
    pub(crate) fn close_search(&mut self, cx: &mut Context<Self>) {
        self.viewer.search.close();
        self.search_input.update(cx, |input, cx| input.clear(cx));
        self.pending_focus = Some(FocusTarget::Root);
        cx.notify();
    }

    /// 查询串变化：逐页缓存全部作废，已渲染的页立刻重算，再请 worker 统计全文。
    pub(crate) fn on_search_query_changed(&mut self, cx: &mut Context<Self>) {
        self.viewer.search.clear_page_matches();
        for page in self.viewer.cached_pages() {
            self.refresh_matches(page);
        }
        self.refresh_matches(self.viewer.current_page);
        self.request_doc_search(cx);
    }

    fn request_doc_search(&mut self, cx: &mut Context<Self>) {
        let query = self.viewer.search.query.clone();
        self.search_request_id = self.search_request_id.wrapping_add(1);

        if query.is_empty() {
            self.viewer.search.clear_results();
            cx.notify();
            return;
        }

        self.viewer.search.scanning = true;
        let _ = self.engine.send(Command::Search {
            query,
            request: self.search_request_id,
        });
        cx.notify();
    }

    fn next_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        // 全文结果优先：能跨页跳。
        if let Some((page, _local)) = self.viewer.search.next_doc(forward) {
            if page != self.viewer.current_page {
                self.set_page(page, cx);
            } else {
                self.recompute_matches(cx);
            }
            cx.notify();
            return;
        }

        if self.viewer.search.next(forward) {
            cx.notify();
            return;
        }

        if self.viewer.search.query.is_empty() {
            return;
        }
        self.set_status("没有匹配", cx);
        cx.notify();
    }

    // ---------- 交互 ----------

    /// 在某一页上按下鼠标：起点归到这一页，之后拖动只在这一页里扩展。
    pub(crate) fn on_page_mouse_down(
        &mut self,
        page: usize,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.char_at_point(page, event.position) else {
            return;
        };
        if event.click_count >= 2 {
            self.select_word_at(page, index);
            self.viewer.selection.dragging = true;
            self.lookup_selection(cx);
        } else {
            self.viewer.selection.start_at(page, index);
        }
        cx.notify();
    }

    /// 拖选：挂在滚动容器上，指针滑出页面（甚至滑到别的页）也能继续扩展选区。
    pub(crate) fn on_page_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        if self
            .home_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
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

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.viewer.has_document() {
            self.on_home_key_down(event, window, cx);
            return;
        }

        let keystroke = &event.keystroke;
        let control = keystroke.modifiers.control || keystroke.modifiers.platform;

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
                    let zoom = self.viewer.zoom * 1.15;
                    self.set_zoom(zoom, cx);
                    return;
                }
                "-" => {
                    let zoom = self.viewer.zoom / 1.15;
                    self.set_zoom(zoom, cx);
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

impl Focusable for AppState {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 窗口尺寸只能在拿到 Window 的地方改；改完等平台的 resize 事件再重算缩放。
        self.apply_pending_window_resize(window, &**cx);

        let viewport = window.viewport_size();
        let viewport_changed = self.viewport != viewport;
        self.viewport = viewport;
        self.scale_factor = window.scale_factor();
        self.main_window_bounds = Some(window.bounds());
        self.display_bounds = window
            .display(&**cx)
            .map(|display| display.visible_bounds());

        if viewport_changed && self.viewer.has_document() {
            // 适宽/适页在窗口变化后要重算缩放，阅读位置保持在同一个锚点上。
            self.refit();
        }

        if self.viewer.has_document() {
            // 滚动条每帧都在动：先把「读到第几页」对齐到最新偏移，再补渲染可视页。
            self.sync_current_page();
            self.ensure_visible_pages();
        }

        let theme = self.theme.clone();

        let content: gpui::AnyElement = if self.viewer.has_document() {
            self.page_content(&theme, cx).into_any_element()
        } else {
            self.home_view(&theme, cx).into_any_element()
        };

        let mut root = div()
            .id("root")
            .role(gpui::Role::Document)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_family.clone())
            .child(content);

        // 首页整条顶栏已经是拖拽区；阅读器仍只保留顶部 14px 的拖拽条。
        if self.viewer.has_document() {
            root = root.child(crate::ui::chrome::drag_strip());
        }

        if let Some(toast) = self.toast(&theme) {
            root = root.child(toast);
        }

        root = root.child(self.window_controls(&theme));

        if self.viewer.has_document() {
            root = root
                .child(self.floating_top(&theme, cx))
                .child(self.floating_bottom(&theme, cx));
        }

        if let Some(panel) = self.outline_panel(&theme, cx) {
            root = root.child(panel);
        }
        if let Some(panel) = self.annotations_panel(&theme, cx) {
            root = root.child(panel);
        }
        if let Some(panel) = self.settings_panel(&theme, cx) {
            root = root.child(panel);
        }
        if self.viewer.search.open {
            root = root.child(self.search_overlay(&theme));
        }

        // 焦点只能在拿到 Window 的地方设置；放在帧尾延后执行，避免打断本次绘制。
        if let Some(target) = self.pending_focus.take() {
            cx.defer_in(&*window, move |this, window, cx| {
                let handle = match target {
                    FocusTarget::Root => this.focus_handle.clone(),
                    FocusTarget::Search => this.search_input.read(cx).focus_handle(cx),
                    FocusTarget::HomeSearch => this.home_input.read(cx).focus_handle(cx),
                };
                window.focus(&handle, cx);
            });
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_term_trims_punctuation_and_takes_first_line() {
        assert_eq!(normalize_term("  \"apple\",\nsecond"), "apple");
        assert_eq!(normalize_term("（你好）"), "你好");
        assert_eq!(normalize_term("..."), "");
    }

    #[test]
    fn normalize_term_caps_length() {
        let term = normalize_term(&"a".repeat(100));
        assert_eq!(term.chars().count(), 64);
    }
}
