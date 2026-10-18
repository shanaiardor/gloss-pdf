//! 应用状态与装配：字段定义、后台订阅、顶层渲染。
//!
//! 具体职责按边界拆在四个子模块里，全部是 `impl AppState`：
//!
//! - [`events`]：输入与引擎事件的分派入口；
//! - [`document`]：文档会话与内容资源的生命周期；
//! - [`render`]：排版几何、缩放与位图调度；
//! - [`window`]：主窗口尺寸与词典浮窗。
//!
//! `app.rs` 只保留四者共享的状态和「谁都需要一点」的装配代码（构造、订阅、
//! 顶层 `Render`、状态栏）。这样任何一个子模块都不必反向依赖另一个子模块的
//! 私有实现，阅读某一条链路时只需要打开一个文件。

mod document;
mod events;
mod render;
mod window;

use std::time::Duration;

use dict_core::{Command as DictCommand, DictEngine};
use futures::StreamExt;
use gpui::{
    AnyWindowHandle, App, Bounds, Context, Entity, FocusHandle, Focusable, Render, Size, Window,
    WindowHandle, div, prelude::*, px,
};
use pdf_core::{Command, PageSize, PdfEngine};

use crate::dictionary::{DictLoadState, DictionaryState};
use crate::home::{HomeNav, HomeState};
use crate::progress::ReadingProgress;
use crate::settings::SettingsStore;
use crate::theme::Theme;
use crate::ui::dictionary::DictionaryWindow;
use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::viewer::Viewer;

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
                    .timer(document::PROGRESS_SAMPLE_INTERVAL)
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

        let search_input =
            cx.new(|cx| TextInput::new("输入关键词，Enter 下一个", theme.clone(), 13.0, cx));
        let home_input = cx.new(|cx| TextInput::new("极简 PDF", theme.clone(), 13.0, cx));

        cx.subscribe(
            &search_input,
            |this, _input, event: &TextInputEvent, cx| match event {
                TextInputEvent::Changed => {
                    this.viewer.search.query = this.search_input.read(cx).value().to_string();
                    this.on_search_query_changed(cx);
                }
                TextInputEvent::Submitted { forward } => this.next_match(*forward, cx),
                TextInputEvent::Cancelled => this.close_search(cx),
            },
        )
        .detach();

        cx.subscribe(
            &home_input,
            |this, _input, event: &TextInputEvent, cx| match event {
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
            },
        )
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

    // ---------- 应用级状态 ----------

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

        let viewport_changed = self.sync_window_metrics(window, &**cx);
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
