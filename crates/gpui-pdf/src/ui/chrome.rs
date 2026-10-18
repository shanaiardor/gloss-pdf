//! 浮动界面组件：顶部胶囊、底部胶囊、窗口按钮、拖拽条。

use gpui::{
    App, ClickEvent, Context, Render, Window, WindowControlArea, div, prelude::*, px, rgb, svg,
};

use crate::app::AppState;
use crate::layout::FitMode;
use crate::theme::Theme;

impl AppState {
    pub(crate) fn floating_top(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let can_save = !self.viewer.highlights.is_empty();

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
            .child(icon_button(
                "home",
                "icons/home.svg",
                "首页",
                false,
                true,
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.close_document(cx);
                }),
            ))
            .child(icon_button(
                "open",
                "icons/open.svg",
                "打开 (Ctrl+O)",
                false,
                true,
                theme,
                cx.listener(|this, _event: &ClickEvent, window, cx| {
                    this.open_pdf(window, cx);
                }),
            ))
            .child(icon_button(
                "outline",
                "icons/outline.svg",
                "目录 (Ctrl+B)",
                self.outline_open,
                true,
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.outline_open = !this.outline_open;
                    cx.notify();
                }),
            ))
            .child(icon_button(
                "search",
                "icons/search.svg",
                "搜索 (Ctrl+F)",
                self.viewer.search.open,
                true,
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    if this.viewer.search.open {
                        this.close_search(cx);
                    } else {
                        this.open_search(cx);
                    }
                }),
            ))
            .child(icon_button(
                "highlight",
                "icons/highlight.svg",
                "高亮 (H)",
                false,
                true,
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.add_highlight(cx);
                }),
            ))
            .child(icon_button(
                "annotations",
                "icons/annotations.svg",
                "批注 (Ctrl+H)",
                self.highlight_panel_open,
                true,
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.toggle_highlight_panel(cx);
                }),
            ))
            .child(icon_button(
                "dictionary",
                "icons/dictionary.svg",
                "词典",
                self.dictionary_window.is_some(),
                true,
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.toggle_dictionary_window(cx);
                }),
            ))
            .child(icon_button(
                "save",
                "icons/save.svg",
                "保存 (Ctrl+S)",
                false,
                can_save,
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.save_highlights(cx);
                }),
            ))
            .child(icon_button(
                "settings",
                "icons/settings.svg",
                "设置",
                self.settings_open,
                true,
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

        // 页码默认是只读标签；点一下变成输入框，Enter 跳页、Esc 收起。
        let page_control: gpui::AnyElement = if self.page_input_open {
            div()
                .h_full()
                .w(px(64.))
                .flex()
                .items_center()
                .child(self.page_input.clone())
                .into_any_element()
        } else {
            div()
                .id("page-jump")
                .min_w(px(70.))
                .px_2()
                .py_1()
                .rounded_full()
                .text_center()
                .font_family(theme.mono_font.clone())
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .hover({
                    let hover_bg = theme.surface_hover;
                    move |style| style.bg(hover_bg)
                })
                .aria_label("跳转到页码")
                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.open_page_input(cx);
                }))
                .child(page_label)
                .into_any_element()
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
            .child(page_control)
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

/// 顶部胶囊的纯图标按钮：悬停提示 + `aria_label` 补上文字含义。
fn icon_button(
    id: &'static str,
    icon: &'static str,
    label: &'static str,
    active: bool,
    enabled: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let hover_bg = theme.surface_hover;
    let icon_color = if !enabled {
        theme.text_muted
    } else if active {
        theme.accent
    } else {
        theme.text
    };
    let tooltip_theme = theme.clone();

    let button = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(28.))
        .rounded_full()
        .aria_label(label)
        .tooltip(move |_window, cx| {
            cx.new(|_| IconTooltip {
                label,
                theme: tooltip_theme.clone(),
            })
            .into()
        })
        .child(svg().path(icon).size(px(16.)).text_color(icon_color));

    let button = if enabled {
        button
            .cursor_pointer()
            .hover(move |style| style.bg(hover_bg))
            .on_click(on_click)
    } else {
        button.opacity(0.35)
    };

    if active {
        button.bg(theme.accent.opacity(0.16))
    } else {
        button
    }
}

/// 图标按钮的悬停提示，保持原来的文字说明。
struct IconTooltip {
    label: &'static str,
    theme: Theme,
}

impl Render for IconTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(self.theme.surface)
            .border_1()
            .border_color(self.theme.border)
            .text_size(px(11.))
            .text_color(self.theme.text_muted)
            .child(self.label)
    }
}

/// 解析页码输入：非数字返回 `None`，数字夹取到 1..=page_count 后转成 0 基下标。
pub(crate) fn parse_page_number(input: &str, page_count: usize) -> Option<usize> {
    if page_count == 0 {
        return None;
    }
    let number = input.trim().parse::<usize>().ok()?;
    Some(number.clamp(1, page_count) - 1)
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

#[cfg(test)]
mod tests {
    use super::parse_page_number;

    #[test]
    fn page_number_is_clamped_into_range() {
        assert_eq!(parse_page_number("5", 10), Some(4));
        assert_eq!(parse_page_number("0", 10), Some(0));
        assert_eq!(parse_page_number("99", 10), Some(9));
        assert_eq!(parse_page_number(" 7 ", 10), Some(6));
    }

    #[test]
    fn page_number_rejects_non_numbers_and_empty_documents() {
        assert_eq!(parse_page_number("", 10), None);
        assert_eq!(parse_page_number("abc", 10), None);
        assert_eq!(parse_page_number("-1", 10), None);
        assert_eq!(parse_page_number("1.5", 10), None);
        assert_eq!(parse_page_number("3", 0), None);
    }
}
