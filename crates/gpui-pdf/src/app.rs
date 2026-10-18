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
mod update;
mod window;

pub use window::initial_window_bounds;

use std::collections::HashSet;
use std::time::Duration;

use dict_core::{Command as DictCommand, DictEngine, DictionaryConfig};
use futures::StreamExt;
use gpui::{
    AnyWindowHandle, App, Bounds, Context, Entity, FocusHandle, Focusable, Render, Size, Window,
    WindowHandle, div, prelude::*, px,
};
use pdf_core::{Command, PageSize, PdfEngine};

use crate::chrome_visibility::ChromeVisibility;
use crate::dict_history::DictHistory;
use crate::dictionary::{DictLoadState, DictRequest, DictionaryState};
use crate::home::{HomeNav, HomeState};
use crate::progress::ReadingProgress;
use crate::settings::{SettingsStore, WindowState};
use crate::theme::Theme;
use crate::ui::dictionary::DictionaryWindow;
use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::update::UpdateEngine;
use crate::viewer::Viewer;

pub(crate) use update::UpdatePhase;

/// 输入框需要焦点时，交给下一次 `render` 去设置（事件回调里拿不到 `Window`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FocusTarget {
    Root,
    Search,
    HomeSearch,
    Page,
}

pub struct AppState {
    pub(crate) theme: Theme,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) engine: std::sync::mpsc::Sender<Command>,
    pub(crate) viewer: Viewer,
    pub(crate) home: HomeState,
    pub(crate) search_input: Entity<TextInput>,
    pub(crate) home_input: Entity<TextInput>,
    /// 底部页码输入框；只有 `page_input_open` 为真时才出现在胶囊里。
    pub(crate) page_input: Entity<TextInput>,
    pub(crate) page_input_open: bool,
    pub(crate) pending_focus: Option<FocusTarget>,
    pub(crate) settings: SettingsStore,
    pub(crate) settings_open: bool,
    pub(crate) dict: DictEngine,
    /// 词典窗口里手动输入/点击历史时，把请求送回主窗口。
    dict_query: futures::channel::mpsc::UnboundedSender<DictRequest>,
    pub(crate) dictionary: DictionaryState,
    /// 持久化的查词历史，最新的在前。
    pub(crate) dict_history: DictHistory,
    /// 独立词典窗口；关闭后保留的句柄会在下一次同步时清掉。
    pub(crate) dictionary_window: Option<WindowHandle<DictionaryWindow>>,
    /// 词典浮窗相对主窗口的吸附锚点；`None` 表示没吸附、不跟随。
    dictionary_follow: Option<crate::window_layout::Anchor>,
    /// 主窗口与显示器工作区，用来把词典窗口放到主窗口右侧。
    main_window_bounds: Option<Bounds<gpui::Pixels>>,
    display_bounds: Option<Bounds<gpui::Pixels>>,
    pub(crate) viewport: Size<gpui::Pixels>,
    pub(crate) scale_factor: f32,
    /// 按住中键拖动时的上一次指针位置；`None` 表示没在拖。
    pub(crate) panning: Option<gpui::Point<gpui::Pixels>>,
    pub(crate) request_id: u64,
    /// 全文搜索的请求代号，用来丢掉过期结果。
    search_request_id: u64,
    pub(crate) outline_open: bool,
    /// 批注（高亮）侧栏是否展开。
    pub(crate) highlight_panel_open: bool,
    /// 浮动胶囊的自动隐身状态机；纯逻辑在 [`crate::chrome_visibility`] 里。
    pub(crate) chrome: ChromeVisibility,
    pub(crate) status: String,
    pub(crate) status_version: u64,
    /// 主窗口标题；文档标题在 `render` 里写入窗口，因为事件回调拿不到 `Window`。
    window_title: String,
    window_title_dirty: bool,
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
    /// 最近一次采样到的窗口几何；改动后在去抖计时器里落盘。
    pending_window_state: Option<WindowState>,
    /// 是否已经安排了一次窗口几何写盘。
    window_state_save_scheduled: bool,
    /// 更新检查/下载的 worker 与界面状态。
    pub(crate) update_engine: UpdateEngine,
    pub(crate) update_phase: UpdatePhase,
    /// 已注册到 GPUI 的词典字体签名，避免反复配置时重复注册。
    registered_dict_fonts: HashSet<(usize, u64)>,
}

impl AppState {
    /// 构造应用状态。
    ///
    /// 设置由 `main` 先读好再传进来：窗口几何要在 `open_window` 之前用到，
    /// 两处各读一遍磁盘会不一致，也会白读一次。
    pub fn with_settings(cx: &mut Context<Self>, settings: SettingsStore) -> Self {
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

        let settings = settings;
        let auto_check_updates = settings.current.auto_check_updates;
        let theme = if settings.current.dark {
            Theme::dark()
        } else {
            Theme::light()
        };

        let (dict_tx, mut dict_rx) = futures::channel::mpsc::unbounded();
        let dict = DictEngine::spawn(dict_tx);
        let mut configure_request = 0;
        if settings.current.has_enabled_dictionary() {
            configure_request = 1;
            let _ = dict.commands.send(DictCommand::Configure {
                request: configure_request,
                dictionaries: settings
                    .current
                    .dictionaries
                    .iter()
                    .map(|dictionary| DictionaryConfig {
                        path: dictionary.path.clone(),
                        enabled: dictionary.enabled,
                    })
                    .collect(),
            });
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

        let dict_history = DictHistory::load();
        let mut dictionary = DictionaryState::default();
        dictionary.configure_request = configure_request;
        dictionary.statuses = settings
            .current
            .dictionaries
            .iter()
            .map(|dictionary| crate::dictionary::DictStatus {
                path: dictionary.path.clone(),
                state: if dictionary.enabled {
                    DictLoadState::Loading
                } else {
                    DictLoadState::Unconfigured
                },
            })
            .collect();
        if settings.current.has_enabled_dictionary() {
            dictionary.load_state = DictLoadState::Loading;
        }

        // 更新检查：worker 负责联网，事件回到主线程改状态。
        let (update_tx, mut update_rx) = futures::channel::mpsc::unbounded();
        let update_engine = UpdateEngine::spawn(update_tx);
        cx.spawn(async move |this, cx| {
            while let Some(event) = update_rx.next().await {
                if this
                    .update(cx, |this, cx| this.handle_update_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // 延迟几秒再查，避免和文档渲染抢启动时的资源；开发构建不自动联网。
        if auto_check_updates && !cfg!(debug_assertions) {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(4)).await;
                let _ = this.update(cx, |this, cx| this.check_for_updates(cx));
            })
            .detach();
        }

        // 词典窗口自己也有输入框：它只负责把词发回来，查词状态仍由主窗口维护。
        let (dict_query, mut dict_query_rx) = futures::channel::mpsc::unbounded::<DictRequest>();
        cx.spawn(async move |this, cx| {
            while let Some(request) = dict_query_rx.next().await {
                let updated = this.update(cx, |this, cx| match request {
                    DictRequest::Lookup(term) => this.lookup_word(term, cx),
                    DictRequest::Suggest(term) => this.request_suggestions(term, cx),
                    DictRequest::ClearHistory => this.clear_dict_history(cx),
                    DictRequest::FloatPlaced(rect) => this.dictionary_float_placed(rect, cx),
                });
                if updated.is_err() {
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
            this.flush_window_state();
            if let Some(handle) = this.dictionary_window.take() {
                let _ = handle.update(cx, |_view, window, _cx| window.remove_window());
            }
        })
        .detach();

        let search_input =
            cx.new(|cx| TextInput::new("输入关键词，Enter 下一个", theme.clone(), 13.0, cx));
        let home_input = cx.new(|cx| TextInput::new("极简 PDF", theme.clone(), 13.0, cx));
        let page_input = cx.new(|cx| TextInput::new("页码", theme.clone(), 12.0, cx));

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

        cx.subscribe(
            &page_input,
            |this, _input, event: &TextInputEvent, cx| match event {
                TextInputEvent::Changed => {}
                TextInputEvent::Submitted { .. } => this.commit_page_input(cx),
                TextInputEvent::Cancelled => this.close_page_input(cx),
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
            page_input,
            page_input_open: false,
            pending_focus: None,
            settings,
            settings_open: false,
            dict,
            dict_query,
            dictionary,
            dict_history,
            dictionary_window: None,
            dictionary_follow: None,
            main_window_bounds: None,
            display_bounds: None,
            viewport: Size {
                width: px(1040.),
                height: px(720.),
            },
            scale_factor: 1.0,
            panning: None,
            request_id: 0,
            search_request_id: 0,
            outline_open: false,
            highlight_panel_open: false,
            chrome: ChromeVisibility::new(),
            status: String::new(),
            status_version: 0,
            window_title: "极简 PDF".into(),
            window_title_dirty: true,
            last_progress: None,
            progress_flush_scheduled: false,
            pending_window_fit: None,
            pending_window_resize: None,
            reader_window_size: None,
            pending_window_state: None,
            window_state_save_scheduled: false,
            update_engine,
            update_phase: UpdatePhase::default(),
            registered_dict_fonts: HashSet::new(),
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
            .update(cx, |input, cx| input.set_theme(theme.clone(), cx));
        self.page_input
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
        if self.window_title_dirty {
            window.set_window_title(&self.window_title);
            self.window_title_dirty = false;
        }

        let viewport_changed = self.sync_window_metrics(window, &**cx);
        if viewport_changed && self.viewer.has_document() {
            // 适宽/适页在窗口变化后要重算缩放，阅读位置保持在同一个锚点上。
            self.refit();
        }

        if self.viewer.has_document() {
            // 滚动条每帧都在动：先把「读到第几页」对齐到最新偏移，再补渲染可视页。
            self.sync_current_page();
            self.ensure_visible_pages();

            // 浮动胶囊的自动隐身：鼠标位置靠窗口级监听（`chrome_mouse_watch`，只能在
            // 绘制阶段注册），这里只负责“鼠标没动但参数变了”和首帧的结算。
            // 为什么需要窗口级监听：顶部 46px 拖拽条是 `WindowControlArea::Drag`，Windows
            // 上是 HTCAPTION（非客户区），鼠标在它上面收不到元素的 hover/move；
            // gpui_windows 会把 `WM_NCMOUSEMOVE` 翻译成同一种 `MouseMoveEvent` 走统一输入管线。
            self.settle_chrome(window.mouse_position(), false, cx);
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
                .child(self.floating_bottom(&theme, cx))
                // 零尺寸，只在绘制阶段注册窗口级鼠标监听（胶囊隐身用）。
                .child(crate::ui::chrome::chrome_mouse_watch(cx));
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
                    FocusTarget::Page => this.page_input.read(cx).focus_handle(cx),
                };
                window.focus(&handle, cx);
            });
        }

        root
    }
}
