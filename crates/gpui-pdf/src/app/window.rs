//! 窗口管理：主窗口按纸张比例调整/恢复尺寸、视口度量同步，以及词典浮窗的
//! 创建、定位、同步与关闭。
//!
//! 边界是「窗口」本身：只读 `PageSize` 与显示器工作区做尺寸/位置计算，不读
//! 文档内容或渲染缓存，也不决定什么时候该渲染哪一页。

use gpui::{
    App, Bounds, Context, Size, TitlebarOptions, Window, WindowBounds, WindowOptions, point,
    prelude::*, px, size,
};

use crate::dictionary::DictionaryView;
use crate::layout::{
    MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH, SCREEN_MARGIN, WINDOW_FILL, window_size_for_page,
};
use crate::settings::WindowState;
use crate::ui::dictionary::DictionaryWindow;
use crate::window_layout::{Anchor, RectF, adjacent_origin, best_work_area, restore_window_bounds};

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

fn rect_f(bounds: Bounds<gpui::Pixels>) -> RectF {
    RectF::new(
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    )
}

fn rect_bounds(rect: RectF) -> Bounds<gpui::Pixels> {
    Bounds::new(
        point(px(rect.x), px(rect.y)),
        size(px(rect.width), px(rect.height)),
    )
}

/// 把词典浮窗移到全局坐标（逻辑像素）。
///
/// GPUI 没有跨平台的「设置窗口位置」API，非 Windows 平台只能不跟（浮窗在
/// 那些平台上本来也只能用系统原生拖拽）。
#[cfg(target_os = "windows")]
fn move_dictionary_origin(window: &mut Window, origin: (f32, f32)) {
    crate::window_move::move_window(window, point(px(origin.0), px(origin.1)));
}

#[cfg(not(target_os = "windows"))]
fn move_dictionary_origin(_window: &mut Window, _origin: (f32, f32)) {}

/// 首次启动（或还没存过几何）时的窗口尺寸。
const DEFAULT_WINDOW_SIZE: (f32, f32) = (1040.0, 720.0);
/// 窗口几何写盘的去抖延迟：拖动窗口时只在停下来之后写一次。
const WINDOW_STATE_SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// Win32 最小化后 `GetWindowRect` 给出的哨兵坐标量级：正常屏幕坐标不会到那里。
const MINIMIZED_SENTINEL: f32 = 32_000.0;

/// 窗口框看起来是否可信（尺寸正常、不是最小化哨兵）。
fn plausible_rect(rect: RectF) -> bool {
    rect.is_finite() && rect.x.abs() <= MINIMIZED_SENTINEL && rect.y.abs() <= MINIMIZED_SENTINEL
}

/// 把当前窗口框转成可落盘的几何；全屏/最小化时不记。
///
/// 返回 `None` 表示这次不值得写盘：全屏（还原框不可靠）、Windows 最小化后
/// `GetWindowRect` 会给出 -32000 这类哨兵坐标，都不能当真，否则会把一份
/// 好几何覆成“看不见的窗口”。
pub(super) fn window_state(window: &Window) -> Option<WindowState> {
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) => (bounds, true),
        WindowBounds::Fullscreen(_) => return None,
    };
    let rect = rect_f(bounds);
    // 32000 是 Win32 最小化哨兵坐标的量级，正常屏幕坐标不会到那里。
    if !plausible_rect(rect) {
        return None;
    }
    Some(WindowState {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
        maximized,
    })
}

/// 启动时主窗口的初始外框：有保存过的几何就恢复（夹到当前屏幕），否则居中。
///
/// 这一步跑在 `open_window` 之前，所以只能读 `App`，不能读 `Window`。
pub fn initial_window_bounds(saved: Option<WindowState>, cx: &App) -> WindowBounds {
    let min_size = (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT);
    let works: Vec<RectF> = cx
        .displays()
        .iter()
        .map(|display| rect_f(display.visible_bounds()))
        .collect();

    let Some(saved) = saved.filter(|state| state.is_finite()) else {
        // 首次启动：按默认尺寸居中（和以前的 1040×720 一致）。
        return WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(DEFAULT_WINDOW_SIZE.0), px(DEFAULT_WINDOW_SIZE.1)),
            cx,
        ));
    };

    let saved_rect = RectF::new(saved.x, saved.y, saved.width, saved.height);
    let work = best_work_area(saved_rect, &works).or_else(|| {
        works
            .iter()
            .copied()
            .find(|work| work.is_finite())
            .or_else(|| {
                cx.primary_display()
                    .map(|display| rect_f(display.visible_bounds()))
                    .filter(|work| work.is_finite())
            })
    });

    let Some(work) = work else {
        log::warn!("no display info, opening the window at the saved bounds as-is");
        let rect = restore_window_bounds(
            saved_rect,
            RectF::new(0.0, 0.0, saved.width, saved.height),
            min_size,
        );
        return WindowBounds::Windowed(rect_bounds(rect));
    };

    let rect = restore_window_bounds(saved_rect, work, min_size);
    log::info!(
        "restore window: saved {}x{} at ({:.0}, {:.0}) -> {}x{} at ({:.0}, {:.0}){}",
        saved.width,
        saved.height,
        saved.x,
        saved.y,
        rect.width,
        rect.height,
        rect.x,
        rect.y,
        if saved.maximized { " (maximized)" } else { "" }
    );
    if saved.maximized {
        WindowBounds::Maximized(rect_bounds(rect))
    } else {
        WindowBounds::Windowed(rect_bounds(rect))
    }
}

/// 词典窗口的尺寸：优先 380×720，屏幕放不下时按工作区收窄。
fn dictionary_window_size(work: RectF) -> Size<gpui::Pixels> {
    use crate::window_layout::SCREEN_MARGIN;

    let width = 380.0_f32.min((work.width - SCREEN_MARGIN * 2.0).max(280.0));
    let height = 720.0_f32.min((work.height - SCREEN_MARGIN * 2.0).max(360.0));
    size(px(width), px(height))
}

impl super::AppState {
    /// 打开文档后按纸张比例调窗口，或回到首页时恢复原窗口尺寸。
    ///
    /// 窗口尺寸要在 `Render` 里改，因为事件处理拿不到 `Window`。
    pub(super) fn apply_pending_window_resize(&mut self, window: &mut Window, cx: &App) {
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

    /// 把当前窗口的视口、缩放比与显示器工作区同步到状态里。
    ///
    /// 返回视口是否变化，调用方据此决定要不要重算适宽/适页。
    pub(super) fn sync_window_metrics(&mut self, window: &Window, cx: &App) -> bool {
        let viewport = window.viewport_size();
        let changed = self.viewport != viewport;
        self.viewport = viewport;
        self.scale_factor = window.scale_factor();
        self.main_window_bounds = Some(window.bounds());
        self.display_bounds = window.display(cx).map(|display| display.visible_bounds());
        changed
    }

    // ---------- 主窗口几何持久化 ----------

    /// 监听窗口尺寸/位置变化，去抖后写进 `settings.json`。
    ///
    /// 拖动窗口会连续触发很多次，不适合每次都写盘；改动期间只记最新值，
    /// 停下来一小会儿再落盘。
    pub(crate) fn watch_window_bounds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.observe_window_bounds(window, |this, window, cx| {
            // 主窗口一移动，贴着它的词典浮窗就得跟着走。
            this.follow_dictionary_window(window, cx);

            let Some(state) = window_state(window) else {
                return;
            };
            if this.settings.current.window == Some(state) {
                return;
            }
            this.pending_window_state = Some(state);
            this.schedule_window_state_save(cx);
        })
        .detach();
    }

    /// 主窗口被拖动/改大小时，把贴着它的词典浮窗一起带走。
    ///
    /// 锚点（贴哪边、水平间距、竖直偏移）由 `dictionary_float_placed`
    /// 维护：创建浮窗时按当时几何定下，用户拖动浮窗后按新位置重算。所以
    /// 拖远就不再跟随、拖回来自动恢复，而且浮窗被屏幕边缘夹住时也不会把
    /// 锚点算坏。
    pub(crate) fn follow_dictionary_window(&mut self, window: &Window, cx: &mut Context<Self>) {
        let main = rect_f(window.bounds());
        // 最小化时窗口框是 -32000 之类的哨兵值，别拿它去挪浮窗。
        if !plausible_rect(main) {
            return;
        }
        self.main_window_bounds = Some(window.bounds());
        let Some(handle) = self.dictionary_window else {
            return;
        };
        // 没贴着主窗口就不跟。
        let Some(anchor) = self.dictionary_follow else {
            return;
        };

        let fallback_work = self.display_bounds.map(rect_f);
        let placed = handle.update(cx, |_view, dict_window, cx| {
            let size = dict_window.bounds().size;
            let size = (f32::from(size.width), f32::from(size.height));
            let work = dict_window
                .display(&**cx)
                .map(|display| rect_f(display.visible_bounds()))
                .or(fallback_work)
                .unwrap_or_else(|| RectF::new(0.0, 0.0, 1920.0, 1080.0));
            let (x, y) = anchor.place(main, size, work);
            log::debug!(
                "follow dictionary window: main {:?} -> float ({x:.0}, {y:.0})",
                (main.x, main.y, main.width, main.height)
            );
            move_dictionary_origin(dict_window, (x, y));
            RectF::new(x, y, size.0, size.1)
        });
        // 浮窗已经关掉了，下次查询时重新创建。
        if placed.is_err() {
            self.dictionary_window = None;
            self.dictionary_follow = None;
        }
    }

    /// 用户把词典浮窗拖到了新位置：按它和主窗口的相对关系重算跟随锚点。
    pub(crate) fn dictionary_float_placed(&mut self, rect: RectF, _cx: &mut Context<Self>) {
        self.dictionary_follow = self
            .main_window_bounds
            .map(rect_f)
            .filter(|main| plausible_rect(*main))
            .and_then(|main| Anchor::from_geometry(main, rect));
    }

    fn schedule_window_state_save(&mut self, cx: &mut Context<Self>) {
        if self.window_state_save_scheduled {
            return;
        }
        self.window_state_save_scheduled = true;

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(WINDOW_STATE_SAVE_DELAY)
                .await;
            let _ = this.update(cx, |this, _cx| {
                this.window_state_save_scheduled = false;
                this.flush_window_state();
            });
        })
        .detach();
    }

    /// 立即把窗口几何落盘（退出时兜底，不等去抖计时器）。
    pub(crate) fn flush_window_state(&mut self) {
        let Some(state) = self.pending_window_state.take() else {
            return;
        };
        if self.settings.current.window == Some(state) {
            return;
        }
        self.settings.current.window = Some(state);
        self.settings.save();
    }

    // ---------- 词典浮窗 ----------

    pub(crate) fn toggle_dictionary_window(&mut self, cx: &mut Context<Self>) {
        let alive = self
            .dictionary_window
            .is_some_and(|handle| handle.update(cx, |_view, _window, _cx| ()).is_ok());

        if alive {
            self.close_dictionary_window(cx);
            return;
        }

        self.dictionary_window = None;
        if !self.settings.current.has_enabled_dictionary() {
            self.settings_open = true;
            self.set_status("请先在设置中添加并启用 MDX 词典", cx);
            cx.notify();
            return;
        }

        self.ensure_dictionary_window(cx);
        cx.notify();
    }

    pub(super) fn ensure_dictionary_window(&mut self, cx: &mut Context<Self>) {
        let mut view = DictionaryView::from(&self.dictionary);
        view.history = self.dict_history.entries().to_vec();
        view.suggestions = self.dictionary.suggestions.clone();
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
                let (x, y) = adjacent_origin(
                    rect_f(main),
                    (f32::from(window_size.width), f32::from(window_size.height)),
                    work,
                    12.0,
                );
                Bounds::new(point(px(x), px(y)), window_size)
            }
            _ => Bounds::centered(
                None,
                dictionary_window_size(RectF::new(0.0, 0.0, 1920.0, 1080.0)),
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

        let mut view = DictionaryView::from(&self.dictionary);
        view.history = self.dict_history.entries().to_vec();
        view.suggestions = self.dictionary.suggestions.clone();
        let theme = self.theme.clone();
        let dict_query = self.dict_query.clone();

        match cx.open_window(options, move |_window, cx| {
            cx.new(|cx| DictionaryWindow::new(view, theme, dict_query, cx))
        }) {
            Ok(handle) => {
                self.dictionary_window = Some(handle);
                // 锚点按刚放好的几何定下来：贴边就跟随，离得远就不跟随。
                self.dictionary_follow = self
                    .main_window_bounds
                    .map(rect_f)
                    .and_then(|main| Anchor::from_geometry(main, rect_f(bounds)));
                self.dictionary.open = true;
                log::info!("dictionary window opened");
            }
            Err(error) => {
                self.dictionary.open = false;
                self.set_status(format!("无法打开词典窗口: {error}"), cx);
            }
        }
    }

    pub(super) fn sync_dictionary_window(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.dictionary_window else {
            return;
        };
        let mut view = DictionaryView::from(&self.dictionary);
        view.history = self.dict_history.entries().to_vec();
        view.suggestions = self.dictionary.suggestions.clone();
        let theme = self.theme.clone();

        if handle
            .update(cx, |window, _window, cx| window.apply(view, theme, cx))
            .is_err()
        {
            self.dictionary_window = None;
            self.dictionary_follow = None;
            self.dictionary.open = false;
        }
    }

    pub(crate) fn close_dictionary_window(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.dictionary_window.take() {
            let _ = handle.update(cx, |_window, window, _cx| window.remove_window());
        }
        self.dictionary_follow = None;
        self.dictionary.open = false;
        cx.notify();
    }
}
