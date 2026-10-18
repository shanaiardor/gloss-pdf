//! 浮动界面组件：顶部胶囊、底部胶囊、窗口按钮、拖拽条。

use gpui::{
    App, ClickEvent, Context, MouseMoveEvent, Pixels, Point, Render, Window, WindowControlArea,
    div, prelude::*, px, rgb, svg,
};

use crate::app::AppState;
use crate::chrome_visibility::{ChromeInput, ChromeSignature, ChromeUpdate, HIDE_DELAY, band_at};
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

        chrome_wrapper(
            div()
                .absolute()
                .top(px(8.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(capsule),
            self.chrome.is_visible(),
        )
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
                    this.step_zoom(-crate::layout::ZOOM_STEP_PERCENT, cx);
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
                    this.step_zoom(crate::layout::ZOOM_STEP_PERCENT, cx);
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

        chrome_wrapper(
            div()
                .absolute()
                .bottom(px(16.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(capsule),
            self.chrome.is_visible(),
        )
    }

    /// 影响胶囊可见性的阅读参数摘要。任何一项变化都会把胶囊叫回来。
    fn chrome_signature(&self) -> ChromeSignature {
        ChromeSignature {
            zoom: self.viewer.zoom,
            current_page: self.viewer.current_page,
            page_count: self.viewer.page_count(),
            fit_mode: self.viewer.fit_mode,
            outline_open: self.outline_open,
            highlight_panel_open: self.highlight_panel_open,
            settings_open: self.settings_open,
            page_input_open: self.page_input_open,
            search_open: self.viewer.search.open,
            highlight_count: self.viewer.highlights.len(),
            dark: self.theme.dark,
        }
    }

    /// 正在进行的交互：这些时候不许隐身，否则页码输入框会跟着胶囊一起消失。
    fn chrome_interacting(&self) -> bool {
        self.page_input_open || self.panning.is_some() || self.viewer.selection.dragging
    }

    /// 结算一次胶囊可见性。鼠标事件回调和每帧 `render` 都走这里，所以必须幂等。
    pub(crate) fn settle_chrome(
        &mut self,
        mouse: Point<Pixels>,
        pointer_moved: bool,
        cx: &mut Context<Self>,
    ) -> ChromeUpdate {
        let viewport = (
            f32::from(self.viewport.width),
            f32::from(self.viewport.height),
        );
        let mouse = (f32::from(mouse.x), f32::from(mouse.y));
        let parameters_changed = self.chrome.observe_parameters(self.chrome_signature());
        let update = self.chrome.update(ChromeInput {
            mouse_in_band: band_at(mouse, viewport, self.chrome.bands()).is_some(),
            parameters_changed,
            interacting: self.chrome_interacting(),
            pointer_moved,
        });
        if let Some(generation) = update.countdown {
            self.schedule_chrome_hide(generation, cx);
        }
        update
    }

    /// 排一个隐藏倒计时；到期时只有代数还对得上才真的隐藏（防止旧定时器
    /// 把刚刚因为鼠标回来而显示的胶囊又藏掉）。
    fn schedule_chrome_hide(&mut self, generation: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIDE_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.chrome.on_countdown_elapsed(generation) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 窗口级鼠标移动：胶囊可见性的实时输入。
    ///
    /// 为什么用窗口级监听而不是胶囊上的 `on_hover`：顶部的 46px `drag_strip()` 是
    /// `WindowControlArea::Drag`，Windows 上对应 HTCAPTION（非客户区），鼠标可能在它
    /// 上面收不到普通元素的 hover/move 事件。gpui_windows 会把 `WM_NCMOUSEMOVE` 翻译
    /// 成同样的 `MouseMoveEvent` 走统一输入管线，窗口级监听两种移动都能收到；再把位置
    /// 和几何感应带（[`band_at`]）比一下，就不依赖任何元素的命中测试。
    pub(crate) fn on_global_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.viewer.has_document() {
            return;
        }
        if self.settle_chrome(event.position, true, cx).changed {
            cx.notify();
        }
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

/// 注册窗口级鼠标监听的零尺寸元素。
///
/// `Window::on_mouse_event` 自带 `debug_assert_paint`：**只能在绘制阶段**调用。
/// `Render::render` 是布局阶段跑的（`EntityRenderer::request_layout` 里调 `render`），
/// 在那里注册会直接 panic（`this method can only be called during paint`），并且会让
/// 已经借用的 `App` 在 panic 展开时泄漏，之后所有 `AsyncApp` 操作都跟着炸。
/// 所以放到 `canvas` 的 paint 回调里：那是真正的绘制阶段，`in_paint` 为真。
///
/// 监听器每帧开画前会被清空，因此每帧都要重新注册一次。
pub(crate) fn chrome_mouse_watch(cx: &mut Context<AppState>) -> impl gpui::IntoElement {
    let view = cx.entity().downgrade();
    gpui::canvas(
        |_bounds, _window, _cx| {},
        move |_bounds, (), window, _cx| {
            let view = view.clone();
            // 同一个监听器在捕获阶段和冒泡阶段各被调一次，这里只吃捕获阶段那一遍。
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                if phase != gpui::DispatchPhase::Capture {
                    return;
                }
                let _ = view.update(cx, |this, cx| this.on_global_mouse_move(event, cx));
            });
        },
    )
}

/// 胶囊外层的可见性包装。
///
/// 隐身必须用 `hidden()`（`display: none`），不能 `invisible()`（`visibility: hidden`）：
/// 后者仍然占着布局位置、仍然在命中测试树里，会把页面的点击、滚轮、拖选和中键平移全吞掉；
/// `display: none` 的子树既不参与布局也不参与 hit-test，胶囊才算真的“隐身”。
fn chrome_wrapper(wrapper: gpui::Div, visible: bool) -> gpui::Div {
    if visible { wrapper } else { wrapper.hidden() }
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
