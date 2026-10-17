//! 首页：左侧导航、顶部搜索、拖拽打开区和最近阅读。

use gpui::{
    ClickEvent, ClipboardItem, Context, ExternalPaths, Rgba, WindowControlArea, div, prelude::*,
    px, relative, rgb,
};

use crate::app::AppState;
use crate::home::{HomeNav, relative_time};
use crate::theme::Theme;

/// 首页独立的一套颜色，读者界面仍使用 `Theme`。
#[derive(Clone, Copy)]
struct HomeColors {
    bg: Rgba,
    sidebar: Rgba,
    card: Rgba,
    paper: Rgba,
    preview: Rgba,
    border: Rgba,
    border_strong: Rgba,
    text: Rgba,
    muted: Rgba,
    faint: Rgba,
    faint_strong: Rgba,
    active: Rgba,
    hover: Rgba,
    ink: Rgba,
    ink_text: Rgba,
    cream: Rgba,
    slate: Rgba,
    teal: Rgba,
    terracotta: Rgba,
}

fn symbol_font() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Segoe UI Symbol"
    }
    #[cfg(target_os = "macos")]
    {
        "Apple Symbols"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "Noto Sans Symbols 2"
    }
}

fn home_colors(theme: &Theme) -> HomeColors {
    if theme.dark {
        HomeColors {
            bg: rgb(0x161613),
            sidebar: rgb(0x1c1c18),
            card: rgb(0x201f1b),
            paper: rgb(0x2a2924),
            preview: rgb(0x252420),
            border: rgb(0x32312b),
            border_strong: rgb(0x46443b),
            text: rgb(0xece9e1),
            muted: rgb(0x969188),
            faint: rgb(0x3b3932),
            faint_strong: rgb(0x565249),
            active: rgb(0x2b2a24),
            hover: rgb(0x25241f),
            ink: rgb(0xf0ede6),
            ink_text: rgb(0x201f1b),
            cream: rgb(0x3b3930),
            slate: rgb(0x8e99a0),
            teal: rgb(0x6f98a1),
            terracotta: rgb(0xc98a76),
        }
    } else {
        HomeColors {
            bg: rgb(0xfbfaf6),
            sidebar: rgb(0xf3f2ed),
            card: rgb(0xfdfcf8),
            paper: rgb(0xfffefb),
            preview: rgb(0xf4f2ec),
            border: rgb(0xe7e4dc),
            border_strong: rgb(0xd2cec2),
            text: rgb(0x24231f),
            muted: rgb(0x8b8982),
            faint: rgb(0xdad7cf),
            faint_strong: rgb(0xb9b6ae),
            active: rgb(0xe6e4dd),
            hover: rgb(0xedebe5),
            ink: rgb(0x2a2925),
            ink_text: rgb(0xfbfaf6),
            cream: rgb(0xece8dd),
            slate: rgb(0x7c8890),
            teal: rgb(0x678e98),
            terracotta: rgb(0xc97f69),
        }
    }
}

impl AppState {
    pub(crate) fn home_view(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = home_colors(theme);

        div()
            .id("home")
            .size_full()
            .flex()
            .bg(colors.bg)
            .font_family(theme.font_family.clone())
            .text_color(colors.text)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                this.open_dropped(paths.paths(), cx);
            }))
            // 铺在首页最底层的标题栏拖拽区：搜索框和窗口按钮会盖在它上面。
            .child(crate::ui::chrome::drag_cursor(
                div()
                    .id("home-titlebar-drag")
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(46.))
                    .window_control_area(WindowControlArea::Drag),
            ))
            .child(self.home_sidebar(colors, cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.home_topbar(colors, cx))
                    .child(self.home_content(colors, cx)),
            )
    }

    // ---------- 侧栏 ----------

    fn home_sidebar(&self, colors: HomeColors, cx: &mut Context<Self>) -> gpui::Div {
        let mut nav = div().flex().flex_col().gap(px(2.)).px(px(10.));
        for (index, item) in [
            HomeNav::Home,
            HomeNav::Recent,
            HomeNav::Favorites,
            HomeNav::Tools,
            HomeNav::Settings,
        ]
        .into_iter()
        .enumerate()
        {
            let active = self.home.nav == item;
            let hover = colors.hover;
            let message = item.placeholder_message();

            nav = nav.child(
                div()
                    .id(("home-nav", index))
                    .h(px(34.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded_md()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text)
                    .bg(if active {
                        colors.active
                    } else {
                        colors.bg.opacity(0.0)
                    })
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        this.home.menu_open = None;
                        this.home.search_focused = false;
                        if item == HomeNav::Home {
                            this.home.nav = HomeNav::Home;
                            this.home.query.clear();
                        } else if let Some(message) = message {
                            // 其他页面尚未实现，保持“主页”选中，只给一条提示。
                            this.set_status(message, cx);
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(18.))
                            .text_center()
                            .font_family(symbol_font())
                            .text_size(px(15.))
                            .text_color(colors.text)
                            .child(item.icon()),
                    )
                    .child(div().child(item.label())),
            );
        }

        div()
            .w(px(168.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(colors.sidebar)
            .border_r_1()
            .border_color(colors.border)
            .pt(px(46.))
            .child(nav)
            .child(div().flex_1())
            .child(
                div()
                    .id("home-about")
                    .mx(px(18.))
                    .mb(px(16.))
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .cursor_pointer()
                    .hover({
                        let hover_text = colors.text;
                        move |style| style.text_color(hover_text)
                    })
                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.set_status(
                            format!("gpui-pdf {} · GPUI + PDFium", env!("CARGO_PKG_VERSION")),
                            cx,
                        );
                        cx.notify();
                    }))
                    .child("关于 / 升级"),
            )
    }

    // ---------- 顶栏搜索 ----------

    fn home_topbar(&self, colors: HomeColors, cx: &mut Context<Self>) -> gpui::Div {
        let query = self.home.query.clone();
        let focused = self.home.search_focused;
        let empty = query.is_empty();
        let search_width = (f32::from(self.viewport.width) - 168.0 - 64.0).clamp(200.0, 380.0);
        let text_color = if empty {
            colors.text.opacity(0.78)
        } else {
            colors.text
        };

        let mut text = div()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.))
            .text_color(text_color)
            .child(if empty {
                "极简 PDF".to_string()
            } else {
                query.clone()
            });

        if empty {
            text = text.justify_center();
        } else {
            text = text.justify_start().pl(px(34.));
            if focused {
                text = text.child(div().ml(px(1.)).w(px(1.)).h(px(14.)).bg(colors.text));
            }
        }

        let search = div()
            .id("home-search")
            .relative()
            .w(px(search_width))
            .h(px(28.))
            .rounded_full()
            .bg(colors.bg)
            .border_1()
            .border_color(if focused {
                colors.border_strong
            } else {
                colors.border
            })
            .cursor_text()
            // 让搜索框挡住底下的标题栏拖拽区，保证点击/输入不会被当成拖动窗口。
            .occlude()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                this.open_dropped(paths.paths(), cx);
            }))
            .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                this.home.search_focused = true;
                this.home.menu_open = None;
                window.focus(&this.focus_handle, cx);
                cx.notify();
            }))
            .child(
                div()
                    .absolute()
                    .left(px(13.))
                    .top(px(7.))
                    .size(px(13.))
                    .child(
                        div()
                            .absolute()
                            .left(px(1.))
                            .top(px(1.))
                            .size(px(8.))
                            .rounded_full()
                            .border_1()
                            .border_color(colors.muted),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(8.))
                            .top(px(9.))
                            .w(px(1.5))
                            .h(px(4.))
                            .rounded_full()
                            .bg(colors.muted),
                    ),
            )
            .child(text);

        div()
            .h(px(46.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .border_b_1()
            .border_color(colors.border)
            // 搜索框与整窗中心对齐，而不是与右侧内容区对齐。
            .child(div().mr(px(168.)).child(search))
    }

    // ---------- 内容区 ----------

    fn home_content(
        &self,
        colors: HomeColors,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let available = f32::from(self.viewport.width) - 168.0 - 48.0;
        let columns = if available >= 820.0 {
            3
        } else if available >= 540.0 {
            2
        } else {
            1
        };

        let filtered = self.home.filtered();
        let mut grid = div().grid().grid_cols(columns).gap(px(14.));
        if filtered.is_empty() {
            grid = grid.child(self.home_empty_card(colors));
        } else {
            for index in filtered {
                grid = grid.child(self.home_recent_card(index, colors, cx));
            }
        }

        div()
            .id("home-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .scrollbar_width(px(8.))
            .child(
                div()
                    .px(px(24.))
                    .pt(px(22.))
                    .pb(px(36.))
                    .flex()
                    .flex_col()
                    .child(self.home_drop_zone(colors, cx))
                    .child(
                        div()
                            .mt(px(16.))
                            .flex()
                            .justify_center()
                            .child(self.home_open_button(colors, cx)),
                    )
                    .child(
                        div()
                            .mt(px(30.))
                            .mb(px(12.))
                            .text_size(px(17.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child("最近阅读"),
                    )
                    .child(grid),
            )
    }

    fn home_drop_zone(
        &self,
        colors: HomeColors,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let hover_bg = colors.preview;
        let drag_bg = colors.active;

        div()
            .id("home-drop")
            .w_full()
            .h(px(166.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .rounded_lg()
            .border_1()
            .border_dashed()
            .border_color(colors.border_strong)
            .bg(colors.bg)
            .cursor_pointer()
            .hover(move |style| style.bg(hover_bg))
            .drag_over::<ExternalPaths>(move |style, _paths, _window, _cx| {
                style.bg(drag_bg).border_color(colors.border_strong)
            })
            .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                this.open_pdf(window, cx);
            }))
            .child(
                div()
                    .w(px(38.))
                    .h(px(46.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .border_2()
                    .border_color(colors.text)
                    .text_size(px(24.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text)
                    .child("↑"),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(colors.text)
                    .child("将 PDF 拖拽到此处，或点击打开"),
            )
    }

    fn home_open_button(
        &self,
        colors: HomeColors,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("home-open")
            .h(px(34.))
            .px(px(18.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded_full()
            .bg(colors.ink)
            .text_color(colors.ink_text)
            .text_size(px(12.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .cursor_pointer()
            .shadow_md()
            .hover(|style| style.opacity(0.9))
            .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                this.open_pdf(window, cx);
            }))
            .child("+")
            .child("打开 PDF")
    }

    // ---------- 最近阅读 ----------

    fn home_recent_card(
        &self,
        index: usize,
        colors: HomeColors,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let recent = &self.home.recents[index];
        let title = recent.title.clone();
        let time = relative_time(recent.opened_at);
        let menu_open = self.home.menu_open == Some(index);
        let variant = preview_variant(&recent.path);
        let progress = recent.progress.filter(|progress| progress.pages > 0);
        let subtitle = match progress {
            Some(progress) => {
                let page = progress.page.min(progress.pages.saturating_sub(1)) + 1;
                format!(
                    "{time} · 第 {page}/{} 页 · {}%",
                    progress.pages,
                    progress.percent()
                )
            }
            None => time,
        };
        let progress_bar = progress.map(|progress| {
            div()
                .h(px(3.))
                .w_full()
                .flex_shrink_0()
                .bg(colors.preview)
                .child(
                    div()
                        .h_full()
                        .w(relative(progress.fraction()))
                        .bg(colors.teal),
                )
        });

        let mut card = div()
            .id(("recent-card", index))
            .relative()
            .flex()
            .flex_col()
            .rounded_lg()
            .bg(colors.card)
            .border_1()
            .border_color(colors.border)
            .cursor_pointer()
            .hover({
                let hover_border = colors.border_strong;
                move |style| style.border_color(hover_border).shadow_md()
            })
            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                // 菜单开着时，第一次点卡片只负责收起菜单，避免误打开。
                if this.home.menu_open == Some(index) {
                    this.home.menu_open = None;
                    cx.notify();
                    return;
                }
                this.open_recent(index, cx);
            }))
            .child(
                div()
                    .h(px(94.))
                    .w_full()
                    .p(px(12.))
                    .overflow_hidden()
                    .rounded_t_lg()
                    .bg(colors.preview)
                    .border_b_1()
                    .border_color(colors.border)
                    .child(preview_art(variant, colors)),
            );

        if let Some(bar) = progress_bar {
            card = card.child(bar);
        }

        card = card.child(
            div()
                .h(px(48.))
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(colors.text)
                                .child(title),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(subtitle),
                        ),
                )
                .child(
                    div()
                        .id(("recent-menu", index))
                        .size(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .text_size(px(15.))
                        .text_color(colors.muted)
                        .cursor_pointer()
                        .hover({
                            let hover_bg = colors.hover;
                            move |style| style.bg(hover_bg)
                        })
                        // 在 mouse down 阶段就截住事件，避免冒泡到整张卡片的打开动作。
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _event: &gpui::MouseDownEvent, _window, cx| {
                                cx.stop_propagation();
                                this.home.menu_open = if this.home.menu_open == Some(index) {
                                    None
                                } else {
                                    Some(index)
                                };
                                cx.notify();
                            }),
                        )
                        .on_click(cx.listener(move |_this, _event: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                        }))
                        .child("…"),
                ),
        );

        if menu_open {
            card = card.child(self.home_recent_menu(index, colors, cx));
        }

        card
    }

    fn home_recent_menu(
        &self,
        index: usize,
        colors: HomeColors,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let hover_bg = colors.hover;
        div()
            .absolute()
            .top(px(28.))
            .right(px(8.))
            .w(px(142.))
            .p(px(4.))
            .rounded_md()
            .bg(colors.card)
            .border_1()
            .border_color(colors.border)
            .shadow_lg()
            .on_mouse_down(gpui::MouseButton::Left, |_event, _window, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .id(("recent-copy", index))
                    .h(px(27.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .text_size(px(12.))
                    .text_color(colors.text)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover_bg))
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        if let Some(recent) = this.home.recents.get(index) {
                            let path = recent.path.display().to_string();
                            cx.write_to_clipboard(ClipboardItem::new_string(path));
                            this.set_status("已复制文件路径", cx);
                            this.home.menu_open = None;
                            cx.notify();
                        }
                    }))
                    .child("复制文件路径"),
            )
            .child(
                div()
                    .id(("recent-remove", index))
                    .h(px(27.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .text_size(px(12.))
                    .text_color(colors.terracotta)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover_bg))
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.home.remove(index);
                        this.set_status("已从最近阅读移除", cx);
                        cx.notify();
                    }))
                    .child("从最近阅读移除"),
            )
    }

    fn home_empty_card(&self, colors: HomeColors) -> gpui::Div {
        let message = if self.home.is_filtering() {
            "没有找到匹配的文档"
        } else {
            "还没有最近阅读，打开一个 PDF 就会出现在这里"
        };

        div()
            .col_span_full()
            .h(px(110.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_lg()
            .border_1()
            .border_dashed()
            .border_color(colors.border)
            .text_size(px(12.))
            .text_color(colors.muted)
            .child(message)
    }
}

// ---------- 卡片缩略图（占位排版，不读 PDF 内容） ----------

/// 用路径哈希选一个稳定的预览样式，删除其他条目后不会变脸。
fn preview_variant(path: &std::path::Path) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % 4) as usize
}

fn preview_art(variant: usize, colors: HomeColors) -> gpui::Div {
    match variant {
        0 => text_page(colors),
        1 => swatch_collage(colors),
        2 => magazine(colors),
        _ => dashboard(colors),
    }
}

fn text_page(colors: HomeColors) -> gpui::Div {
    div()
        .size_full()
        .rounded(px(3.))
        .bg(colors.paper)
        .border_1()
        .border_color(colors.border)
        .p(px(10.))
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(bar(relative(0.38), 5.0, colors.faint_strong))
        .child(bar(relative(0.9), 4.0, colors.faint))
        .child(bar(relative(0.74), 4.0, colors.faint))
        .child(bar(relative(0.94), 4.0, colors.faint))
        .child(bar(relative(0.56), 4.0, colors.faint))
}

fn swatch_collage(colors: HomeColors) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .gap(px(8.))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(5.))
                .pt(px(4.))
                .child(bar(relative(0.34), 6.0, colors.faint_strong))
                .child(bar(relative(0.92), 5.0, colors.faint_strong))
                .child(bar(relative(0.62), 4.0, colors.faint))
                .child(bar(relative(0.82), 4.0, colors.faint)),
        )
        .child(
            div()
                .w(px(60.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .child(swatch(28.0, colors.cream))
                        .child(swatch(28.0, colors.ink)),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .child(swatch(28.0, colors.slate))
                        .child(swatch(28.0, colors.terracotta)),
                ),
        )
}

fn magazine(colors: HomeColors) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .gap(px(8.))
        .child(
            div()
                .w(px(46.))
                .h_full()
                .rounded(px(3.))
                .bg(colors.cream)
                .p(px(6.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(bar(relative(0.8), 5.0, colors.faint_strong))
                .child(bar(relative(0.55), 4.0, colors.faint)),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(5.))
                .pt(px(2.))
                .child(bar(relative(0.48), 5.0, colors.faint_strong))
                .child(bar(relative(0.96), 4.0, colors.faint))
                .child(bar(relative(0.86), 4.0, colors.faint))
                .child(bar(relative(0.92), 4.0, colors.faint))
                .child(bar(relative(0.45), 4.0, colors.faint)),
        )
}

fn dashboard(colors: HomeColors) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .gap(px(8.))
        .child(
            div()
                .w(px(58.))
                .h_full()
                .rounded(px(3.))
                .bg(colors.teal)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(20.))
                        .rounded_full()
                        .border_2()
                        .border_color(colors.paper),
                ),
        )
        .child(
            div()
                .flex_1()
                .h_full()
                .flex()
                .items_end()
                .gap(px(5.))
                .pt(px(6.))
                .child(bar_column(18.0, colors.faint))
                .child(bar_column(34.0, colors.faint_strong))
                .child(bar_column(24.0, colors.slate))
                .child(bar_column(48.0, colors.teal))
                .child(bar_column(30.0, colors.faint)),
        )
}

fn bar(width: gpui::DefiniteLength, height: f32, color: Rgba) -> gpui::Div {
    div().w(width).h(px(height)).rounded(px(1.)).bg(color)
}

fn bar_column(height: f32, color: Rgba) -> gpui::Div {
    div().flex_1().h(px(height)).rounded_t_sm().bg(color)
}

fn swatch(size: f32, color: Rgba) -> gpui::Div {
    div().size(px(size)).rounded(px(3.)).bg(color)
}
