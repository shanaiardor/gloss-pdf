//! 浮动界面组件：顶部胶囊、底部胶囊、窗口按钮、拖拽条。

use gpui::{App, ClickEvent, Context, Window, WindowControlArea, div, prelude::*, px};

use crate::app::AppState;
use crate::theme::Theme;

impl AppState {
    pub(crate) fn floating_top(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let outline_label = if self.outline_open { "目录 ·" } else { "目录" };
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
            .child(save)
            .child(pill_button(
                "theme",
                "主题",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.toggle_theme(cx);
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
            .child(pill_button(
                "fit",
                "适宽",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.fit_to_width();
                    let zoom = this.viewer.zoom;
                    this.set_zoom(zoom, cx);
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

    pub(crate) fn window_controls(&self, theme: &Theme) -> gpui::Div {
        div()
            .absolute()
            .top(px(10.))
            .right(px(12.))
            .flex()
            .items_center()
            .gap_1()
            .child(window_control(
                "win-min",
                "—",
                WindowControlArea::Min,
                26.0,
                theme,
            ))
            .child(window_control(
                "win-max",
                "□",
                WindowControlArea::Max,
                22.0,
                theme,
            ))
            .child(window_control(
                "win-close",
                "✕",
                WindowControlArea::Close,
                26.0,
                theme,
            ))
    }
}

pub(crate) fn drag_strip() -> gpui::Stateful<gpui::Div> {
    div()
        .id("titlebar-drag")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(14.))
        .window_control_area(WindowControlArea::Drag)
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

fn window_control(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    area: WindowControlArea,
    size: f32,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let hover_bg = theme.surface_hover;
    div()
        .id(id)
        .size(px(size))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.46))
        .text_color(theme.text_muted)
        .cursor_pointer()
        .hover(move |style| style.bg(hover_bg))
        .window_control_area(area)
        .child(label.into())
}
