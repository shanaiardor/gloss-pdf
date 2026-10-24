//! 窗口管理：主窗口按纸张比例调整/恢复尺寸、视口度量同步与几何持久化。
//!
//! 边界是「窗口」本身：只读 `PageSize` 与显示器工作区做尺寸/位置计算，不读
//! 文档内容或渲染缓存，也不决定什么时候该渲染哪一页。

use gpui::{App, Bounds, Context, Window, WindowBounds, point, px, size};

use crate::layout::{
    GLOSS_RAIL_EXTENT, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH, SCREEN_MARGIN, WINDOW_FILL,
    window_size_for_page,
};
use crate::settings::WindowState;
use crate::window_layout::{RectF, best_work_area, restore_window_bounds};

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
pub(super) fn window_state(window: &Window) -> Option<WindowState> {
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) => (bounds, true),
        WindowBounds::Fullscreen(_) => return None,
    };
    let rect = rect_f(bounds);
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
pub fn initial_window_bounds(saved: Option<WindowState>, cx: &App) -> WindowBounds {
    let min_size = (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT);
    let works: Vec<RectF> = cx
        .displays()
        .iter()
        .map(|display| rect_f(display.visible_bounds()))
        .collect();

    let Some(saved) = saved.filter(|state| state.is_finite()) else {
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

impl super::AppState {
    /// 打开文档后按纸张比例调窗口，或回到首页时恢复原窗口尺寸。
    pub(super) fn apply_pending_window_resize(&mut self, window: &mut Window, cx: &App) {
        let page = self.pending_window_fit.take();
        let restore = self.pending_window_resize.take();

        if window.is_maximized() || window.is_fullscreen() {
            return;
        }

        if let Some(page) = page {
            if self.reader_window_size.is_none() {
                self.reader_window_size = Some(window.viewport_size());
            }

            let (room_width, room_height) = window_room(window, cx);
            let (mut width, height) = window_size_for_page(page, room_width, room_height);
            // 只有文档里已有钉住的笺注、确实需要常驻栏位时才加宽；
            // 「配置了词典」本身不该把没打开词典的窗口也撑宽。
            if self.has_gloss_rail() {
                width = (width + GLOSS_RAIL_EXTENT).min(room_width);
            }
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
    pub(crate) fn watch_window_bounds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.observe_window_bounds(window, |this, window, cx| {
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
}
