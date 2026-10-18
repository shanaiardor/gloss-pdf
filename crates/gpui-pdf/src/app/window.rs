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
use crate::ui::dictionary::DictionaryWindow;
use crate::window_layout::{RectF, adjacent_origin};

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
        if self.settings.current.dictionary_path.is_none() {
            self.settings_open = true;
            self.set_status("请先在设置中选择 MDX 词典", cx);
            cx.notify();
            return;
        }

        self.ensure_dictionary_window(cx);
        cx.notify();
    }

    pub(super) fn ensure_dictionary_window(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn sync_dictionary_window(&mut self, cx: &mut Context<Self>) {
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
}
