//! 应用状态与装配：字段定义、后台订阅、顶层渲染。
//!
//! 具体职责按边界拆在四个子模块里，全部是 `impl AppState`：
//!
//! - [`events`]：输入与引擎事件的分派入口；
//! - [`document`]：文档会话与内容资源的生命周期；
//! - [`render`]：排版几何、缩放与位图调度；
//! - [`window`]：主窗口尺寸与几何持久化。
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

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use dict_core::{Command as DictCommand, DictEngine, DictionaryConfig};
use futures::StreamExt;
use gpui::{
    App, Bounds, Context, Entity, FocusHandle, Focusable, Render, Size, Window, div, prelude::*, px,
};
use pdf_core::{PageSize, PdfEngine, PdfiumGate, ThumbnailEngine};

use crate::chrome_visibility::ChromeVisibility;
use crate::dict_history::DictHistory;
use crate::dictionary::{DictLoadState, DictRequest, DictionaryState};
use crate::home::{HomeNav, HomeState};
use crate::progress::ReadingProgress;
use crate::settings::{SettingsStore, WindowState};
use crate::theme::Theme;
use crate::thumbnails::Thumbnails;
use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::update::UpdateEngine;
use crate::viewer::Viewer;

pub(crate) use update::UpdatePhase;

/// 设置改动的去抖窗口：和窗口几何的 400ms 对齐，用户停手后基本感觉不到延迟。
const SETTINGS_SAVE_DELAY: Duration = Duration::from_millis(500);

/// 输入框需要焦点时，交给下一次 `render` 去设置（事件回调里拿不到 `Window`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FocusTarget {
    Root,
    Search,
    HomeSearch,
    Page,
    Dictionary,
}

pub struct AppState {
    pub(crate) theme: Theme,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) engine: PdfEngine,
    /// 与缩略图 worker 共用的 PDFium 闸门；首页据此判断是否该暂停缩略图。
    pub(crate) pdfium_gate: PdfiumGate,
    /// 首页缩略图专用 worker；与阅读器引擎分开，避免互相拖慢。
    pub(crate) thumbnail_engine: ThumbnailEngine,
    pub(crate) viewer: Viewer,
    pub(crate) thumbnails: Thumbnails,
    pub(crate) home: HomeState,
    pub(crate) search_input: Entity<TextInput>,
    pub(crate) home_input: Entity<TextInput>,
    /// 底部页码输入框；只有 `page_input_open` 为真时才出现在胶囊里。
    pub(crate) page_input: Entity<TextInput>,
    pub(crate) page_input_open: bool,
    /// 页边查词卡里的查词输入框。
    pub(crate) dict_input: Entity<TextInput>,
    pub(crate) pending_focus: Option<FocusTarget>,
    pub(crate) settings: SettingsStore,
    pub(crate) settings_open: bool,
    pub(crate) dict: DictEngine,
    /// 释义内 `entry://` 链接点击时把查词请求送回 `AppState`。
    pub(crate) dict_query: futures::channel::mpsc::UnboundedSender<DictRequest>,
    pub(crate) dictionary: DictionaryState,
    /// 页边笺注卡上一帧实测高度，按卡片 id（钉住的 id / 激活卡占位 id）缓存。
    /// 排版用真实高度避让，比「估算 88/340」可靠；首帧还没有数据时用估算值。
    pub(crate) gloss_card_heights: HashMap<u64, f32>,
    /// 持久化的查词历史，最新的在前。
    pub(crate) dict_history: DictHistory,
    pub(crate) main_window_bounds: Option<Bounds<gpui::Pixels>>,
    pub(crate) display_bounds: Option<Bounds<gpui::Pixels>>,
    pub(crate) viewport: Size<gpui::Pixels>,
    pub(crate) scale_factor: f32,
    /// 按住中键拖动时的上一次指针位置；`None` 表示没在拖。
    pub(crate) panning: Option<gpui::Point<gpui::Pixels>>,
    pub(crate) request_id: u64,
    /// 全文搜索的请求代号，用来丢掉过期结果。
    search_request_id: u64,
    pub(crate) outline_open: bool,
    /// 批注（高亮与页边笺注）侧栏是否展开。
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
    /// 是否已经安排了一次设置写盘；AI 输入框逐字符改动，不能每键落盘。
    settings_save_scheduled: bool,
    /// 更新检查/下载的 worker 与界面状态。
    pub(crate) update_engine: UpdateEngine,
    pub(crate) update_phase: UpdatePhase,
    /// 已注册到 GPUI 的词典字体签名，避免反复配置时重复注册。
    registered_dict_fonts: HashSet<(usize, u64)>,
    /// AI 设置页的瞬时状态（展开哪张卡、输入框句柄），不落盘。
    pub(crate) ai_ui: crate::ai_settings::AiUiState,
    /// AI 工具取页的请求入口：工具侧拿到它，向引擎要页文本。
    pub(crate) page_text_requests: futures::channel::mpsc::UnboundedSender<
        crate::ai_context::PageTextRequest,
    >,
    /// 已发出、还没回来的取页请求，事件到达时用它回包。
    pub(crate) pending_page_reads:
        HashMap<u64, futures::channel::oneshot::Sender<Result<String, String>>>,
    /// 取页请求代号，避免回包串台。
    pub(crate) page_text_request_id: u64,
    /// 当前 AI 查词的运行态（状态机 + 缓存 + 任务）。
    pub(crate) ai: crate::ai_query::AiQueryState,
    /// 拖选松手后的「词典 / AI」胶囊；点它才查询。
    pub(crate) selection_capsule: Option<crate::selection::SelectionCapsule>,
    /// 鼠标是否停在选区胶囊上；悬停期间不执行自动收起。
    pub(crate) selection_capsule_hovered: bool,
    /// 胶囊显示/悬停的代次；过期的自动收起任务据此退出。
    selection_capsule_version: u64,
}

impl AppState {
    /// 构造应用状态。
    ///
    /// 设置由 `main` 先读好再传进来：窗口几何要在 `open_window` 之前用到，
    /// 两处各读一遍磁盘会不一致，也会白读一次。
    pub fn with_settings(cx: &mut Context<Self>, settings: SettingsStore) -> Self {
        let (event_tx, mut event_rx) = futures::channel::mpsc::unbounded();
        let pdfium_gate = PdfiumGate::default();
        let engine = PdfEngine::spawn_with_gate(event_tx, pdfium_gate.clone());

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

        // 首页缩略图有自己的 worker：一次要补十几张图，不能和翻页渲染抢队列。
        // 两者共用 PDFiumGate，打开文档时会等缩略图退出临界区，避免同时解析同一个大文件。
        let (thumbnail_tx, mut thumbnail_rx) = futures::channel::mpsc::unbounded();
        let thumbnail_engine = ThumbnailEngine::spawn_with_gate(thumbnail_tx, pdfium_gate.clone());
        cx.spawn(async move |this, cx| {
            while let Some(event) = thumbnail_rx.next().await {
                if this
                    .update(cx, |this, cx| this.handle_thumbnail_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // AI 工具取页：工具侧发请求，这里转成引擎命令并把回包接到 oneshot 上。
        let (page_text_tx, mut page_text_rx) =
            futures::channel::mpsc::unbounded::<crate::ai_context::PageTextRequest>();
        cx.spawn(async move |this, cx| {
            while let Some(request) = page_text_rx.next().await {
                if this
                    .update(cx, |this, _cx| this.begin_page_read(request))
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

        // 释义里的 entry:// 链接通过通道把目标词条送回主窗口就地跳转。
        let (dict_query, mut dict_query_rx) = futures::channel::mpsc::unbounded::<DictRequest>();
        cx.spawn(async move |this, cx| {
            while let Some(request) = dict_query_rx.next().await {
                let updated = this.update(cx, |this, cx| match request {
                    DictRequest::Lookup(term) => this.lookup_word_from_link(term, cx),
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

        // 关窗/退出时兜底保存一次，避免最后不到一秒的滚动丢失。
        cx.on_release(|this, _cx| {
            this.flush_progress();
            this.flush_window_state();
            this.flush_settings_save();
        })
        .detach();

        let search_input =
            cx.new(|cx| TextInput::new("输入关键词，Enter 下一个", theme.clone(), 13.0, cx));
        let home_input = cx.new(|cx| TextInput::new("搜索书名、作者或全文", theme.clone(), 13.0, cx));
        let page_input = cx.new(|cx| TextInput::new("页码", theme.clone(), 12.0, cx));
        let dict_input =
            cx.new(|cx| TextInput::new("输入字词，Enter 查词", theme.clone(), 12.0, cx));

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
                    // 搜索只对最近阅读列表有意义，输入时自动离开设置页。
                    this.home.nav = HomeNav::Home;
                    this.settings_open = false;
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

        cx.subscribe(
            &dict_input,
            |this, _input, event: &TextInputEvent, cx| match event {
                TextInputEvent::Changed => {
                    let term = this.dict_input.read(cx).value().to_string();
                    this.request_suggestions(term, cx);
                }
                TextInputEvent::Submitted { .. } => this.submit_dict_query(cx),
                TextInputEvent::Cancelled => {
                    // 和其他位置的 Esc 保持一致：收起整张查词卡，而不是只清空输入框。
                    this.close_dictionary(cx);
                }
            },
        )
        .detach();

        Self {
            theme,
            focus_handle: cx.focus_handle(),
            engine,
            pdfium_gate,
            thumbnail_engine,
            viewer: Viewer::new(),
            thumbnails: Thumbnails::default(),
            home: HomeState::load(),
            search_input,
            home_input,
            page_input,
            page_input_open: false,
            dict_input,
            pending_focus: None,
            settings,
            settings_open: false,
            dict,
            dict_query,
            dictionary,
            gloss_card_heights: HashMap::new(),
            dict_history,
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
            window_title: "笺释".into(),
            window_title_dirty: true,
            last_progress: None,
            progress_flush_scheduled: false,
            pending_window_fit: None,
            pending_window_resize: None,
            reader_window_size: None,
            pending_window_state: None,
            window_state_save_scheduled: false,
            settings_save_scheduled: false,
            update_engine,
            update_phase: UpdatePhase::default(),
            registered_dict_fonts: HashSet::new(),
            ai_ui: crate::ai_settings::AiUiState::default(),
            page_text_requests: page_text_tx,
            pending_page_reads: HashMap::new(),
            page_text_request_id: 0,
            ai: crate::ai_query::AiQueryState::default(),
            selection_capsule: None,
            selection_capsule_hovered: false,
            selection_capsule_version: 0,
        }
    }

    // ---------- 应用级状态 ----------

    /// 设置改动后延迟落盘。
    ///
    /// AI 配置里的 key / 模型名是逐字符输入的：每键写一次 `settings.json`
    /// 既浪费也容易在断电时留下半截文件，所以合并成一次写。
    pub(crate) fn schedule_settings_save(&mut self, cx: &mut Context<Self>) {
        if self.settings_save_scheduled {
            return;
        }
        self.settings_save_scheduled = true;

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTINGS_SAVE_DELAY).await;
            let _ = this.update(cx, |this, _cx| {
                this.settings_save_scheduled = false;
                this.settings.save();
            });
        })
        .detach();
    }

    /// 立即落盘（退出兜底，不等去抖计时器）。
    pub(crate) fn flush_settings_save(&mut self) {
        if self.settings_save_scheduled {
            self.settings_save_scheduled = false;
        }
        self.settings.save();
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
            .update(cx, |input, cx| input.set_theme(theme.clone(), cx));
        self.dict_input
            .update(cx, |input, cx| input.set_theme(theme, cx));
        self.set_status(format!("主题：{}", self.theme.name()), cx);
        cx.notify();
    }

    pub(crate) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        // 首页的「设置」是内容区里的一页；阅读器里没有可替换的页面，才用浮层。
        if !self.viewer.has_document() {
            self.settings_open = false;
            self.home.nav = HomeNav::Settings;
            self.home.menu_open = None;
            self.home.search_focused = false;
            self.enter_ai_settings(cx);
            cx.notify();
            return;
        }

        self.settings_open = !self.settings_open;
        if self.settings_open {
            self.home.menu_open = None;
            self.home.search_focused = false;
            self.enter_ai_settings(cx);
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
        let had_pending_fit = self.pending_window_fit.is_some();
        self.apply_pending_window_resize(window, &**cx);
        if self.window_title_dirty {
            window.set_window_title(&self.window_title);
            self.window_title_dirty = false;
        }

        let viewport_changed = self.sync_window_metrics(window, &**cx);
        if (viewport_changed || had_pending_fit) && self.viewer.has_document() {
            // 适宽/适页在窗口变化后要重算缩放，阅读位置保持在同一个锚点上。
            self.refit();
        }

        if self.viewer.has_document() {
            // 滚动条每帧都在动：先把「读到第几页」对齐到最新偏移，再补渲染可视页。
            self.sync_current_page();
            self.ensure_visible_pages();

            // 浮动胶囊的自动隐身：鼠标位置靠窗口级监听（`chrome_mouse_watch`，只能在
            // 绘制阶段注册），这里只负责“鼠标没动但参数变了”和首帧的结算。
            self.settle_chrome(window.mouse_position(), false, cx);
        }

        let theme = self.theme.clone();

        // 首页缩略图：首帧读缓存，缺图时排队；设置页没有卡片，不必生成。
        if !self.viewer.has_document() && self.home.nav == HomeNav::Home {
            self.ensure_home_thumbnails();
        }

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
                    FocusTarget::Dictionary => this.dict_input.read(cx).focus_handle(cx),
                };
                window.focus(&handle, cx);
            });
        }

        root
    }
}
