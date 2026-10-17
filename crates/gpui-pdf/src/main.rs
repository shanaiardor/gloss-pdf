//! 入口：初始化日志，打开主窗口，把错误收敛到日志里。

mod app;
mod dictionary;
mod geometry;
mod highlights;
mod home;
mod layout;
mod progress;
mod search;
mod selection;
mod settings;
mod theme;
mod ui;
mod viewer;
mod window_layout;
#[cfg(target_os = "windows")]
mod window_move;

use anyhow::Context as _;
use gpui::{
    App, AppContext as _, Bounds, Focusable as _, SharedString, TitlebarOptions, WindowBounds,
    WindowOptions, px, size,
};
use gpui_platform::application;

fn main() {
    init_logging();
    log::info!("starting gpui-pdf");

    application().run(|cx: &mut App| {
        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        if let Err(error) = open_main_window(cx) {
            log::error!("failed to open main window: {error:#}");
            cx.quit();
        }
    });
}

fn init_logging() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
}

fn open_main_window(cx: &mut App) -> anyhow::Result<()> {
    let bounds = Bounds::centered(None, size(px(1040.), px(720.)), cx);

    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from("极简 PDF")),
                // 隐藏 Windows 原生标题栏，改用自绘的浮动控件。
                appears_transparent: true,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(app::AppState::new);
            if let Some(path) = std::env::args().nth(1) {
                view.update(cx, |view, _cx| view.open_path(path.into()));
            }
            window.focus(&view.focus_handle(cx), cx);
            view
        },
    )
    .context("open main window")?;

    cx.activate(true);
    log::info!("main window opened");
    Ok(())
}
