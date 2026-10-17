//! 浮动界面组件：顶部胶囊、底部胶囊、窗口按钮、拖拽条。

use gpui::{App, ClickEvent, Context, Window, WindowControlArea, div, prelude::*, px, rgb};

use crate::app::AppState;
use crate::layout::FitMode;
use crate::theme::Theme;

impl AppState {
    pub(crate) fn floating_top(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let outline_label = if self.outline_open {
            "目录 ·"
        } else {
            "目录"
        };
        let can_save = !self.viewer.highlights.is_empty();

        let mut save = pill_button(
            "save",
            "保存",
            theme,
            cx.listener(|this, _event: &ClickEvent, _window, cx| {
                this.save_highlights(cx);
            }),
        );
        if !can_save {
            save = save.opacity(0.35);
        }

        let capsule = div()
            .id("floating-top-capsule")
            .flex()
            .items_center()
            .gap_1()
            .h(px(38.))
            .px_2()
            .rounded_full()
            .bg(theme.surface.opacity(0.92))
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            // 扩大后的顶部拖拽带在胶囊下面，胶囊要挡住命中区。
            .occlude()
            .child(pill_button(
                "home",
                "首页",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.close_document(cx);
                }),
            ))
            .child(pill_button(
                "open",
                "打开",
                theme,
                cx.listener(|this, _event: &ClickEvent, window, cx| {
                    this.open_pdf(window, cx);
                }),
            ))
            .child(pill_button(
                "outline",
                outline_label,
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.outline_open = !this.outline_open;
                    cx.notify();
                }),
            ))
            .child(pill_button(
                "search",
                "搜索",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    if this.viewer.search.open {
                        this.viewer.search.close();
                    } else {
                        this.viewer.search.open = true;
                    }
                    cx.notify();
                }),
            ))
            .child(pill_button(
                "highlight",
                "高亮",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.add_highlight(cx);
                }),
            ))
            .child(pill_button(
                "dictionary",
                if self.dictionary_window.is_some() {
                    "词典 ·"
                } else {
                    "词典"
                },
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.toggle_dictionary_window(cx);
                }),
            ))
            .child(save)
            .child(pill_button(
                "settings",
                "设置",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.toggle_settings(cx);
                }),
            ));

        div()
            .absolute()
            .top(px(8.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(capsule)
    }

    pub(crate) fn floating_bottom(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let page_count = self.viewer.page_count();
        let page_label = if page_count == 0 {
            "— / —".to_string()
        } else {
            format!("{} / {}", self.viewer.current_page + 1, page_count)
        };

        let capsule = div()
            .flex()
            .items_center()
            .gap_1()
            .h(px(38.))
            .px_2()
            .rounded_full()
            .bg(theme.surface.opacity(0.92))
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .child(pill_button(
                "prev",
                "‹",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    if this.viewer.current_page > 0 {
                        let previous = this.viewer.current_page - 1;
                        this.set_page(previous, cx);
                    }
                }),
            ))
            .child(
                div()
                    .min_w(px(70.))
                    .text_center()
                    .font_family(theme.mono_font.clone())
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(page_label),
            )
            .child(pill_button(
                "next",
                "›",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    let next = this.viewer.current_page + 1;
                    this.set_page(next, cx);
                }),
            ))
            .child(div().w(px(1.)).h(px(16.)).bg(theme.border))
            .child(pill_button(
                "zoom-out",
                "−",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    let zoom = this.viewer.zoom / 1.15;
                    this.set_zoom(zoom, cx);
                }),
            ))
            .child(
                div()
                    .w(px(46.))
                    .text_center()
                    .font_family(theme.mono_font.clone())
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(format!("{:.0}%", self.viewer.zoom * 100.0)),
            )
            .child(pill_button(
                "zoom-in",
                "＋",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    let zoom = this.viewer.zoom * 1.15;
                    this.set_zoom(zoom, cx);
                }),
            ))
            .child(div().w(px(1.)).h(px(16.)).bg(theme.border))
            .child(fit_button(
                "fit-width",
                "适宽",
                self.viewer.fit_mode == FitMode::Width,
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.fit_to_width(cx);
                }),
            ))
            .child(fit_button(
                "fit-page",
                "适页",
                self.viewer.fit_mode == FitMode::Page,
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.fit_to_page(cx);
                }),
            ));

        div()
            .absolute()
            .bottom(px(16.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(capsule)
    }

    pub(crate) fn window_controls(&self, _theme: &Theme) -> gpui::Div {
        div()
            .absolute()
            .top(px(14.))
            .left(px(16.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(window_control_dot(
                "win-close",
                WindowControlArea::Close,
                rgb(0xff5f57),
            ))
            .child(window_control_dot(
                "win-min",
                WindowControlArea::Min,
                rgb(0xfebc2e),
            ))
            .child(window_control_dot(
                "win-max",
                WindowControlArea::Max,
                rgb(0x28c840),
            ))
    }
}

pub(crate) fn drag_strip() -> gpui::Stateful<gpui::Div> {
    drag_cursor(
        div()
            .id("titlebar-drag")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(46.))
            .window_control_area(WindowControlArea::Drag),
    )
}

/// 拖拽区域的鼠标样式。
///
/// Windows 端 GPUI 把 OpenHand/ClosedHand 都映射成箭头，只有 PointingHand 会变成手型，
/// 所以这里在 Windows 上用 pointing hand，其他平台用更贴切的 grab hand。
pub(crate) fn drag_cursor<D: gpui::Styled>(element: D) -> D {
    #[cfg(target_os = "windows")]
    {
        element.cursor_pointer()
    }
    #[cfg(not(target_os = "windows"))]
    {
        element.cursor_grab()
    }
}

fn pill_button(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let hover_bg = theme.surface_hover;
    div()
        .id(id)
        .flex_shrink_0()
        .px_3()
        .py_1()
        .rounded_full()
        .text_size(px(12.))
        .text_color(theme.text)
        .cursor_pointer()
        .hover(move |style| style.bg(hover_bg))
        .on_click(on_click)
        .child(label.into())
}

/// 适宽/适页按钮：当前生效的模式用强调色标出来。
fn fit_button(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    active: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let button = pill_button(id, label, theme, on_click);
    if active {
        button
            .bg(theme.accent.opacity(0.16))
            .text_color(theme.accent)
    } else {
        button
    }
}

fn window_control_dot(
    id: &'static str,
    area: WindowControlArea,
    color: gpui::Rgba,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(12.))
        .rounded_full()
        .bg(color)
        .cursor_pointer()
        .hover(|style| style.opacity(0.82))
        // 首页顶部整条都可拖动，窗口按钮要挡住拖拽命中区。
        .occlude()
        .window_control_area(area)
}
