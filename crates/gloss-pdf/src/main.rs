//! 入口：初始化日志，打开主窗口，把错误收敛到日志里。

// Release 构建时用 Windows GUI 子系统，避免启动时弹出控制台黑窗口；
// Debug 构建保留控制台，方便看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod app;
mod ai_context;
mod ai_query;
mod ai_settings;
mod assets;
mod chrome_visibility;
mod dict_history;
mod dict_suggest;
mod dictionary;
mod fonts;
mod geometry;
mod highlights;
mod home;
mod layout;
mod progress;
mod search;
mod selection;
mod settings;
mod theme;
mod thumbnails;
mod tiles;
mod ui;
mod update;
mod viewer;
mod window_layout;

use anyhow::Context as _;
use gpui::{
    App, AppContext as _, Focusable as _, KeyBinding, SharedString, TitlebarOptions, WindowOptions,
};
use gpui_platform::application;

fn main() {
    init_logging();
    log::info!("starting gloss-pdf");

    application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
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
    fonts::install(cx);
    bind_text_input_keys(cx);

    // 先读设置：窗口位置和尺寸必须在 `open_window` 之前定下来，
    // 读到的这份设置再交给 `AppState`，避免两处各读一遍。
    let settings = settings::SettingsStore::load();
    let window_bounds = app::initial_window_bounds(settings.current.window, cx);

    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("笺释")),
                    // 隐藏 Windows 原生标题栏，改用自绘的浮动控件。
                    appears_transparent: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| app::AppState::with_settings(cx, settings));
                if let Some(path) = std::env::args().nth(1) {
                    view.update(cx, |view, _cx| view.open_path(path.into()));
                }
                window.focus(&view.focus_handle(cx), cx);
                view
            },
        )
        .context("open main window")?;

    // 之后每次改窗口尺寸/位置都记一笔，下次启动按这里恢复。
    handle
        .update(cx, |view, window, cx| view.watch_window_bounds(window, cx))
        .context("watch main window bounds")?;

    cx.activate(true);
    log::info!("main window opened");
    Ok(())
}

/// 文本输入框的编辑快捷键。
fn bind_text_input_keys(cx: &mut App) {
    use ui::text_input::{
        Backspace, Copy, Cut, Delete, End, Home, Left, Paste, Right, SelectAll, SelectLeft,
        SelectRight,
    };

    let context = Some("TextInput");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("home", Home, context),
        KeyBinding::new("end", End, context),
        KeyBinding::new("ctrl-a", SelectAll, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("ctrl-c", Copy, context),
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("ctrl-x", Cut, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("ctrl-v", Paste, context),
        KeyBinding::new("cmd-v", Paste, context),
    ]);
}
