//! 首页：高保真还原设计稿（Ardot #740208401507583）
//! 包含 248px 侧边栏、顶栏、内容区、继续阅读 Hero 深色卡、最近阅读卡片网格以及空态页面。

use gpui::{
    ClickEvent, ClipboardItem, Context, ExternalPaths, ObjectFit, Rgba, WindowControlArea, div, img,
    prelude::*, px, relative, rgb, rgba, svg,
};

use crate::app::AppState;
use crate::home::{HomeNav, RecentDocument, ViewMode, format_today_header, relative_time};
use crate::progress::unix_now;
use crate::theme::Theme;

/// 首页高保真调色板（严格提取自设计稿）
#[derive(Clone, Copy)]
struct DesignPalette {
    bg: Rgba,
    sidebar: Rgba,
    card: Rgba,
    preview_bg: Rgba,
    border: Rgba,
    border_strong: Rgba,
    text: Rgba,
    muted: Rgba,
    subtle: Rgba,
    accent: Rgba,
    accent_hover: Rgba,
    hero_bg: Rgba,
    hero_text: Rgba,
    hero_subtext: Rgba,
    hero_accent: Rgba,
    hero_track: Rgba,
    nav_active: Rgba,
    hover: Rgba,
}

fn palette(theme: &Theme) -> DesignPalette {
    if theme.dark {
        DesignPalette {
            bg: rgb(0x161618),
            sidebar: rgb(0x1c1c1f),
            card: rgb(0x222226),
            preview_bg: rgb(0x28282c),
            border: rgb(0x323238),
            border_strong: rgb(0x42424b),
            text: rgb(0xf5f5f7),
            muted: rgb(0x86868b),
            subtle: rgb(0xa1a1a6),
            accent: rgb(0x0a84ff),
            accent_hover: rgb(0x2492ff),
            hero_bg: rgb(0x1d1d20),
            hero_text: rgb(0xffffff),
            hero_subtext: rgb(0xa1a1a6),
            hero_accent: rgb(0x2997ff),
            hero_track: rgb(0x3a3a3c),
            nav_active: rgb(0x2c2c30),
            hover: rgb(0x28282c),
        }
    } else {
        DesignPalette {
            // 设计稿: #F5F5F7 (0.961, 0.961, 0.969)
            bg: rgb(0xf5f5f7),
            // 设计稿侧边栏: 同 #F5F5F7
            sidebar: rgb(0xf5f5f7),
            // 设计稿卡片: #FFFFFF
            card: rgb(0xffffff),
            // 预览底色: #FFFFFF
            preview_bg: rgb(0xffffff),
            // 边框 / 分隔线: #E5E5EA (0.898, 0.898, 0.918)
            border: rgb(0xe5e5ea),
            border_strong: rgb(0xd1d1d6),
            // 主文字: #1D1D1F (0.114, 0.114, 0.122)
            text: rgb(0x1d1d1f),
            // 次级文案: #86868B (0.525, 0.525, 0.545)
            muted: rgb(0x86868b),
            // 第三级文案: #6E6E73 (0.431, 0.431, 0.451)
            subtle: rgb(0x6e6e73),
            // 强调亮蓝: #0071E3 / #0070E3 (0, 0.443, 0.890)
            accent: rgb(0x0071e3),
            accent_hover: rgb(0x0077ed),
            // Hero 深色卡: #1D1D1F
            hero_bg: rgb(0x1d1d1f),
            hero_text: rgb(0xffffff),
            hero_subtext: rgb(0xa1a1a6),
            hero_accent: rgb(0x2997ff),
            hero_track: rgb(0x3a3a3c),
            // 导航选中: #E8E8EC (0.910, 0.910, 0.929)
            nav_active: rgb(0xe8e8ec),
            // Hover: #EFEFF3
            hover: rgb(0xefeff3),
        }
    }
}

impl AppState {
    pub(crate) fn home_view(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = palette(theme);

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
            // 铺在首页最底层的标题栏拖拽区（避让左侧边栏 248px，避免拖拽区盖住左侧导航点击）
            .child(crate::ui::chrome::drag_cursor(
                div()
                    .id("home-titlebar-drag")
                    .absolute()
                    .top_0()
                    .left(px(248.))
                    .right_0()
                    .h(px(46.))
                    .window_control_area(WindowControlArea::Drag),
            ))
            .child(self.home_sidebar(colors, cx))
            // 侧栏分隔线
            .child(
                div()
                    .w(px(1.))
                    .h_full()
                    .bg(colors.border),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.home_topbar(colors, cx))
                    .child(self.home_content(colors, theme, cx)),
            )
    }

    // =========================================================================
    // 侧边栏 (248px)
    // =========================================================================

    fn home_sidebar(&self, colors: DesignPalette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .w(px(248.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(colors.sidebar)
            .child(self.home_sidebar_brand(colors))
            .child(self.home_sidebar_main_nav(colors, cx))
            .child(self.home_sidebar_tools_nav(colors, cx))
            .child(div().flex_1()) // 弹性占位
            .child(self.home_sidebar_bottom(colors, cx))
    }

    fn home_sidebar_brand(&self, colors: DesignPalette) -> gpui::Div {
        div()
            .h(px(84.))
            .px(px(22.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                // Logo 标记 (26x26, 8px 圆角, 蓝底, 白图标)
                div()
                    .size(px(26.))
                    .rounded(px(8.))
                    .bg(colors.accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path("icons/open.svg")
                            .size(px(15.))
                            .text_color(rgb(0xffffff)),
                    ),
            )
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.text)
                    .child("gloss"),
            )
    }

    fn home_sidebar_main_nav(&self, colors: DesignPalette, cx: &mut Context<Self>) -> gpui::Div {
        let count = self.home.recents.len();
        div()
            .px(px(12.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(self.sidebar_nav_item(
                HomeNav::Home,
                "icons/home.svg",
                "主页",
                if count > 0 { Some(count.to_string()) } else { None },
                colors,
                cx,
            ))
            .child(self.sidebar_nav_item(
                HomeNav::Favorites,
                "icons/favorite.svg",
                "收藏",
                None,
                colors,
                cx,
            ))
            .child(self.sidebar_nav_item(
                HomeNav::Bookmarks,
                "icons/bookmark.svg",
                "书签",
                None,
                colors,
                cx,
            ))
    }

    fn home_sidebar_tools_nav(&self, colors: DesignPalette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .child(
                // 分组标题
                div()
                    .pt(px(26.))
                    .pb(px(8.))
                    .px(px(24.))
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.muted)
                    .child("工具"),
            )
            .child(
                div()
                    .px(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(self.sidebar_nav_item(
                        HomeNav::Dictionary,
                        "icons/dictionary.svg",
                        "词典",
                        None,
                        colors,
                        cx,
                    ))
                    .child(self.sidebar_nav_item(
                        HomeNav::Annotations,
                        "icons/annotations.svg",
                        "标注",
                        None,
                        colors,
                        cx,
                    )),
            )
    }

    fn home_sidebar_bottom(&self, colors: DesignPalette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .px(px(12.))
            .pb(px(16.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .mb(px(8.))
                    .h(px(1.))
                    .bg(colors.border),
            )
            .child(self.sidebar_nav_item(
                HomeNav::Settings,
                "icons/settings.svg",
                "设置",
                None,
                colors,
                cx,
            ))
            .child(self.sidebar_nav_item(
                HomeNav::About,
                "icons/about.svg",
                "关于",
                None,
                colors,
                cx,
            ))
            .child(
                div()
                    .px(px(12.))
                    .pt(px(8.))
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(format!("v{} · 已是最新", env!("CARGO_PKG_VERSION"))),
            )
    }

    fn sidebar_nav_item(
        &self,
        item: HomeNav,
        icon_path: &'static str,
        label: &'static str,
        badge: Option<String>,
        colors: DesignPalette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let active = self.home.nav == item;
        let hover = colors.hover;

        div()
            .id(label)
            .h(px(42.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(10.))
            .bg(if active {
                colors.nav_active
            } else {
                rgba(0x00000000)
            })
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                this.home.menu_open = None;
                this.home.search_focused = false;
                this.settings_open = false;
                this.home.nav = item;
                if item == HomeNav::Home {
                    this.home.query.clear();
                } else if item == HomeNav::About {
                    this.set_status(
                        format!("笺释 (gloss-pdf) {} · 基于 GPUI 与 PDFium", env!("CARGO_PKG_VERSION")),
                        cx,
                    );
                }
                cx.notify();
            }))
            .child(
                svg()
                    .path(icon_path)
                    .size(px(18.))
                    .text_color(if active { colors.text } else { colors.muted }),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(14.))
                    .font_weight(if active {
                        gpui::FontWeight::MEDIUM
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .text_color(colors.text)
                    .child(label),
            )
            .children(badge.map(|b| {
                div()
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child(b)
            }))
    }

    // =========================================================================
    // 顶栏 (72px)
    // =========================================================================

    fn home_topbar(&self, colors: DesignPalette, cx: &mut Context<Self>) -> gpui::Div {
        let focused = self.home.search_focused;
        let is_grid = self.home.view_mode == ViewMode::Grid;

        div()
            .h(px(72.))
            .pl(px(40.))
            .pr(if cfg!(target_os = "windows") { px(180.) } else { px(40.) })
            .flex()
            .items_center()
            .gap(px(14.))
            .border_b_1()
            .border_color(colors.border)
            .child(
                // 搜索框 (430x40, 圆角 10px, 白底)
                div()
                    .id("home-search-input")
                    .w(px(430.))
                    .h(px(40.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(10.))
                    .bg(colors.card)
                    .border_1()
                    .border_color(if focused {
                        colors.border_strong
                    } else {
                        colors.border
                    })
                    .cursor_text()
                    .occlude()
                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.home.nav = HomeNav::Home;
                        this.settings_open = false;
                        this.home.search_focused = true;
                        this.home.menu_open = None;
                        this.pending_focus = Some(crate::app::FocusTarget::HomeSearch);
                        cx.notify();
                    }))
                    .child(
                        svg()
                            .path("icons/search.svg")
                            .size(px(16.))
                            .text_color(colors.muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .child(self.home_input.clone()),
                    )
                    .child(
                        // Cmd K / Ctrl K 提示胶囊
                        div()
                            .px(px(6.))
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.))
                            .bg(colors.bg)
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(if cfg!(target_os = "macos") { "⌘ K" } else { "Ctrl K" }),
                    ),
            )
            .child(div().flex_1()) // 弹性占位
            .child(
                // 视图切换 (网格 / 列表)
                div()
                    .h(px(40.))
                    .p(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .rounded(px(12.))
                    .bg(colors.card)
                    .border_1()
                    .border_color(colors.border)
                    .child(
                        div()
                            .id("view-grid")
                            .size(px(32.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(7.))
                            .bg(if is_grid { colors.bg } else { rgba(0x00000000) })
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                this.home.view_mode = ViewMode::Grid;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(if is_grid { colors.text } else { colors.muted })
                                    .child("⊞"),
                            ),
                    )
                    .child(
                        div()
                            .id("view-list")
                            .size(px(32.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(7.))
                            .bg(if !is_grid { colors.bg } else { rgba(0x00000000) })
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                this.home.view_mode = ViewMode::List;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(if !is_grid { colors.text } else { colors.muted })
                                    .child("☰"),
                            ),
                    ),
            )
            .child(
                // 打开 PDF 按钮 (蓝底胶囊)
                div()
                    .id("home-open-pdf-btn")
                    .h(px(40.))
                    .px(px(20.))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .rounded_full()
                    .bg(colors.accent)
                    .cursor_pointer()
                    .hover({
                        let hover_bg = colors.accent_hover;
                        move |style| style.bg(hover_bg)
                    })
                    .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                        this.open_pdf(window, cx);
                    }))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(0xffffff))
                            .child("+"),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(rgb(0xffffff))
                            .child("打开 PDF"),
                    ),
            )
    }

    // =========================================================================
    // 主内容区
    // =========================================================================

    fn home_content(
        &self,
        colors: DesignPalette,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if let Some(page) = self.settings_page(theme, cx) {
            return page;
        }

        match self.home.nav {
            HomeNav::Bookmarks => self.home_bookmarks_page(colors, theme, cx),
            HomeNav::Dictionary => self.home_dictionary_page(colors, theme, cx),
            HomeNav::Annotations => self.home_annotations_page(colors, theme, cx),
            HomeNav::About => self.home_about_page(colors, theme, cx),
            HomeNav::Home | HomeNav::Favorites | HomeNav::Settings => {
                let is_empty = self.home.recents.is_empty();
                div()
                    .id("home-scroll-container")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .scrollbar_width(px(8.))
                    .child(
                        div()
                            .px(px(40.))
                            .py(px(30.))
                            .flex()
                            .flex_col()
                            .gap(px(30.))
                            .child(self.home_header_section(is_empty, colors))
                            .child(if is_empty {
                                self.home_empty_section(colors, cx)
                            } else {
                                self.home_populated_section(colors, cx)
                            }),
                    )
                    .into_any_element()
            }
        }
    }

    // ---------- 页面标题区 ----------

    fn home_header_section(&self, is_empty: bool, colors: DesignPalette) -> gpui::Div {
        if is_empty {
            div()
                .flex()
                .flex_col()
                .gap(px(5.))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(colors.muted)
                        .child("首次使用"),
                )
                .child(
                    div()
                        .text_size(px(34.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(colors.text)
                        .child("从第一本书开始"),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(colors.subtle)
                        .child("把 PDF 拖进来，或从磁盘选择文件。gloss 会记住你读到哪里。"),
                )
        } else {
            let today = format_today_header(unix_now());
            let count = self.home.recents.len();
            let last_opened = self
                .home
                .recents
                .first()
                .map(|r| relative_time(r.opened_at))
                .unwrap_or_else(|| "刚刚".to_string());

            div()
                .flex()
                .flex_col()
                .gap(px(5.))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(colors.muted)
                        .child(today),
                )
                .child(
                    div()
                        .text_size(px(34.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(colors.text)
                        .child("我的书架"),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(colors.subtle)
                        .child(format!("{count} 份文档 · 上次阅读 {last_opened}")),
                )
        }
    }

    // =========================================================================
    // 空态视图
    // =========================================================================

    fn home_empty_section(&self, colors: DesignPalette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(32.))
            .child(
                // 虚线大投放区 (20px 圆角, 居中)
                div()
                    .id("empty-drop-zone")
                    .w_full()
                    .h(px(380.))
                    .rounded(px(20.))
                    .border_2()
                    .border_dashed()
                    .border_color(colors.border)
                    .bg(colors.card)
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(16.))
                    .cursor_pointer()
                    .hover({
                        let hover_border = colors.accent;
                        move |style| style.border_color(hover_border)
                    })
                    .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                        this.open_pdf(window, cx);
                    }))
                    .child(
                        // 投放图标 (64x64, 白底圆圈, 带边框)
                        div()
                            .size(px(64.))
                            .rounded_full()
                            .bg(colors.bg)
                            .border_1()
                            .border_color(colors.border)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                svg()
                                    .path("icons/open.svg")
                                    .size(px(28.))
                                    .text_color(colors.accent),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(22.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child("把第一份 PDF 放进来"),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(colors.subtle)
                            .child("拖拽文件到窗口任意位置，或点击下方按钮从磁盘选择"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                // 选择文件 (蓝底)
                                div()
                                    .id("empty-choose-file")
                                    .h(px(42.))
                                    .px(px(20.))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .rounded_full()
                                    .bg(colors.accent)
                                    .cursor_pointer()
                                    .hover({
                                        let hover_bg = colors.accent_hover;
                                        move |style| style.bg(hover_bg)
                                    })
                                    .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                                        this.open_pdf(window, cx);
                                    }))
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(rgb(0xffffff))
                                            .child("选择文件"),
                                    ),
                            )
                            .child(
                                // 先看一份示例 (白底圆角边框)
                                div()
                                    .id("empty-sample-btn")
                                    .h(px(42.))
                                    .px(px(18.))
                                    .flex()
                                    .items_center()
                                    .rounded_full()
                                    .bg(colors.card)
                                    .border_1()
                                    .border_color(colors.border)
                                    .cursor_pointer()
                                    .hover({
                                        let hover_bg = colors.hover;
                                        move |style| style.bg(hover_bg)
                                    })
                                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                        let sample = std::path::PathBuf::from("samples/dummy.pdf");
                                        if sample.is_file() {
                                            this.open_path(sample);
                                        } else {
                                            this.set_status("未找到示例文件", cx);
                                        }
                                    }))
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(colors.text)
                                            .child("先看一份示例"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child("支持 PDF · 扫描件自动识别文字 · 文件只保存在本地"),
                    ),
            )
            .child(
                // 底部三列使用提示
                div()
                    .w_full()
                    .flex()
                    .gap(px(16.))
                    .child(self.empty_feature_card(
                        "icons/open.svg",
                        "拖拽即可打开",
                        "任意位置松手都行",
                        colors,
                    ))
                    .child(self.empty_feature_card(
                        "icons/bookmark.svg",
                        "回到中断处",
                        "自动记住每本书的位置",
                        colors,
                    ))
                    .child(self.empty_feature_card(
                        "icons/search.svg",
                        "全文搜索与词典",
                        "书内检索、划词查义",
                        colors,
                    )),
            )
    }

    fn empty_feature_card(
        &self,
        icon_path: &'static str,
        title: &'static str,
        desc: &'static str,
        colors: DesignPalette,
    ) -> gpui::Div {
        div()
            .flex_1()
            .p(px(16.))
            .flex()
            .items_center()
            .gap(px(12.))
            .rounded(px(14.))
            .bg(colors.card)
            .border_1()
            .border_color(colors.border)
            .child(
                div()
                    .size(px(32.))
                    .rounded(px(10.))
                    .bg(colors.bg)
                    .border_1()
                    .border_color(colors.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path(icon_path)
                            .size(px(16.))
                            .text_color(colors.muted),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(colors.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(desc),
                    ),
            )
    }

    // =========================================================================
    // 已有文档主页（Hero + 网格）
    // =========================================================================

    fn home_populated_section(&self, colors: DesignPalette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(30.))
            .children(self.home.hero_document().map(|hero| {
                self.home_hero_card(hero, colors, cx)
            }))
            .child(self.home_recent_grid_section(colors, cx))
    }

    // ---------- 继续阅读 Hero 深色卡片 (200px, 20px 圆角) ----------

    fn home_hero_card(
        &self,
        hero: &RecentDocument,
        colors: DesignPalette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let time = relative_time(hero.opened_at);
        let progress = hero.progress.filter(|p| p.pages > 0);
        let page_now = progress.map(|p| p.page.min(p.pages.saturating_sub(1)) + 1).unwrap_or(1);
        let total_pages = progress.map(|p| p.pages).unwrap_or(1);
        let percent = progress.map(|p| p.percent()).unwrap_or(0);
        let fraction = progress.map(|p| p.fraction()).unwrap_or(0.0);

        let path = hero.path.clone();

        div()
            .id("home-hero-card")
            .w_full()
            .h(px(200.))
            .px(px(26.))
            .flex()
            .items_center()
            .gap(px(24.))
            .rounded(px(20.))
            .bg(colors.hero_bg)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, _cx| {
                this.open_path(path.clone());
            }))
            .child(
                // 封面微缩 (88x124, 4px 圆角, 白纸)
                div()
                    .w(px(88.))
                    .h(px(124.))
                    .p(px(10.))
                    .rounded(px(4.))
                    .bg(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .w(px(26.))
                            .h(px(3.))
                            .rounded(px(2.))
                            .bg(colors.accent),
                    )
                    .child(div().w_full().h(px(3.)).rounded(px(2.)).bg(rgb(0xe5e5ea)))
                    .child(div().w_full().h(px(3.)).rounded(px(2.)).bg(rgb(0xe5e5ea)))
                    .child(div().w(px(44.)).h(px(3.)).rounded(px(2.)).bg(rgb(0xe5e5ea)))
                    .child(div().w_full().h(px(3.)).rounded(px(2.)).bg(rgb(0xf2f2f7)))
                    .child(div().w_full().h(px(3.)).rounded(px(2.)).bg(rgb(0xf2f2f7)))
                    .child(div().w(px(52.)).h(px(3.)).rounded(px(2.)).bg(rgb(0xf2f2f7))),
            )
            .child(
                // 文案列
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(9.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(colors.hero_accent)
                                    .child("继续阅读"),
                            )
                            .child(
                                div()
                                    .size(px(3.))
                                    .rounded_full()
                                    .bg(colors.hero_track),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child(format!("上次打开 {time}")),
                            ),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(26.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(colors.hero_text)
                            .child(hero.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(colors.hero_subtext)
                            .child(format!("第 {page_now} 页 / 共 {total_pages} 页 · 已读 {percent}%")),
                    )
                    .child(
                        // 进度条轨道 (320x4)
                        div()
                            .w(px(320.))
                            .h(px(4.))
                            .rounded_full()
                            .bg(colors.hero_track)
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(fraction))
                                    .rounded_full()
                                    .bg(colors.accent),
                            ),
                    ),
            )
            .child(
                // 操作列
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(9.))
                    .child(
                        // 继续阅读按钮 (白底胶囊)
                        div()
                            .h(px(42.))
                            .px(px(20.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded_full()
                            .bg(rgb(0xffffff))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0x1d1d1f))
                                    .child("继续阅读"),
                            )
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(rgb(0x1d1d1f))
                                    .child("→"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(format!("从第 {page_now} 页开始")),
                    ),
            )
    }

    // ---------- 最近阅读网格区块 ----------

    fn home_recent_grid_section(&self, colors: DesignPalette, cx: &mut Context<Self>) -> gpui::Div {
        let count = self.home.recents.len();
        let filtered = self.home.filtered();
        let is_fav = self.home.nav == HomeNav::Favorites;

        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                // 标题行
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(colors.text)
                                    .child(if is_fav { "我的收藏" } else { "最近阅读" }),
                            )
                            .child(
                                div()
                                    .h(px(20.))
                                    .px(px(8.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(colors.card)
                                    .border_1()
                                    .border_color(colors.border)
                                    .text_size(px(11.))
                                    .text_color(colors.subtle)
                                    .child(if is_fav {
                                        filtered.len().to_string()
                                    } else {
                                        count.to_string()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(16.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child("按最近打开"),
                            )
                            .child(
                                div()
                                    .id("view-all-link")
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                        this.home.nav = HomeNav::Home;
                                        this.home.query.clear();
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(colors.accent)
                                            .child(if is_fav { "查看全部" } else { "全部文档" }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(colors.accent)
                                            .child("›"),
                                    ),
                            ),
                    ),
            )
            .child(
                // 根据当前 ViewMode 切换 网格 或 列表 视图
                if self.home.view_mode == ViewMode::Grid {
                    self.home_card_grid(&filtered, colors, cx)
                } else {
                    self.home_card_list(&filtered, colors, cx)
                },
            )
    }

    fn home_card_list(
        &self,
        filtered: &[usize],
        colors: DesignPalette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        if filtered.is_empty() {
            return div()
                .w_full()
                .h(px(140.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(16.))
                .border_1()
                .border_dashed()
                .border_color(colors.border)
                .text_size(px(13.))
                .text_color(colors.muted)
                .child(if self.home.nav == HomeNav::Favorites {
                    "暂无收藏的文档"
                } else if self.home.is_filtering() {
                    "没有找到匹配的文档"
                } else {
                    "暂无阅读记录"
                });
        }

        let mut list = div().flex().flex_col().gap(px(8.));
        for &index in filtered {
            list = list.child(self.home_document_list_row(index, colors, cx));
        }
        list
    }

    // ---------- 列表行视图 (List Row) ----------

    fn home_document_list_row(
        &self,
        index: usize,
        colors: DesignPalette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let recent = &self.home.recents[index];
        let title = recent.title.clone();
        let time = relative_time(recent.opened_at);
        let menu_open = self.home.menu_open == Some(index);
        let is_fav = recent.favorite;

        let progress = recent.progress.filter(|p| p.pages > 0);
        let progress_label = match progress {
            Some(p) => {
                let page = p.page.min(p.pages.saturating_sub(1)) + 1;
                format!("第 {page} / {} 页 · {}%", p.pages, p.percent())
            }
            None => "未开始".to_string(),
        };

        let mut row = div()
            .id(("list-row", index))
            .relative()
            .h(px(56.))
            .px(px(16.))
            .flex()
            .items_center()
            .gap(px(16.))
            .rounded(px(12.))
            .bg(colors.card)
            .border_1()
            .border_color(colors.border)
            .cursor_pointer()
            .hover({
                let hover_border = colors.border_strong;
                move |style| style.border_color(hover_border).shadow_sm()
            })
            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                if this.home.menu_open == Some(index) {
                    this.home.menu_open = None;
                    cx.notify();
                    return;
                }
                this.open_recent(index, cx);
            }))
            .child(
                // 文档小图标
                div()
                    .size(px(32.))
                    .rounded(px(8.))
                    .bg(colors.bg)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path("icons/open.svg")
                            .size(px(16.))
                            .text_color(colors.accent),
                    ),
            )
            .child(
                // 书名
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(14.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text)
                    .child(title),
            )
            .child(
                // 进度胶囊
                div()
                    .h(px(24.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(colors.bg)
                    .text_size(px(11.))
                    .text_color(colors.subtle)
                    .child(progress_label),
            )
            .child(
                // 打开时间
                div()
                    .w(px(80.))
                    .text_right()
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child(time),
            )
            .child(
                // 快捷收藏星标按钮
                div()
                    .id(("list-fav", index))
                    .size(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover({
                        let hover_bg = colors.hover;
                        move |style| style.bg(hover_bg)
                    })
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.home.toggle_favorite(index);
                        this.set_status(
                            if this.home.recents.get(index).is_some_and(|r| r.favorite) {
                                "已加入收藏"
                            } else {
                                "已取消收藏"
                            },
                            cx,
                        );
                        cx.notify();
                    }))
                    .child(
                        svg()
                            .path("icons/favorite.svg")
                            .size(px(15.))
                            .text_color(if is_fav {
                                colors.accent
                            } else {
                                colors.muted.opacity(0.4)
                            }),
                    ),
            )
            .child(
                // 更多菜单
                div()
                    .id(("list-more", index))
                    .size(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_size(px(16.))
                    .text_color(colors.muted)
                    .cursor_pointer()
                    .hover({
                        let hover_bg = colors.hover;
                        move |style| style.bg(hover_bg)
                    })
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
                    .on_click(cx.listener(|_this, _event: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                    }))
                    .child("…"),
            );

        if menu_open {
            row = row.child(self.home_recent_menu(index, colors, cx));
        }

        row
    }

    fn home_card_grid(
        &self,
        filtered: &[usize],
        colors: DesignPalette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let available = f32::from(self.viewport.width) - 248.0 - 80.0;
        let columns = if available >= 900.0 {
            3
        } else if available >= 560.0 {
            2
        } else {
            1
        };

        let mut grid = div().grid().grid_cols(columns).gap(px(20.));

        if filtered.is_empty() {
            grid = grid.child(
                div()
                    .col_span_full()
                    .h(px(140.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(16.))
                    .border_1()
                    .border_dashed()
                    .border_color(colors.border)
                    .text_size(px(13.))
                    .text_color(colors.muted)
                    .child(if self.home.is_filtering() {
                        "没有找到匹配的文档"
                    } else {
                        "暂无阅读记录"
                    }),
            );
        } else {
            for &index in filtered {
                grid = grid.child(self.home_document_card(index, colors, cx));
            }
        }

        grid
    }

    // ---------- 文档卡片 (356x276, 16px 圆角) ----------

    fn home_document_card(
        &self,
        index: usize,
        colors: DesignPalette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let recent = &self.home.recents[index];
        let title = recent.title.clone();
        let time = relative_time(recent.opened_at);
        let menu_open = self.home.menu_open == Some(index);
        let thumbnail = self.thumbnails.image_for(&recent.path);

        let progress = recent.progress.filter(|p| p.pages > 0);
        let meta = match progress {
            Some(p) => {
                let page = p.page.min(p.pages.saturating_sub(1)) + 1;
                format!("第 {page} / {} 页 · {time}", p.pages)
            }
            None => time,
        };

        let mut card = div()
            .id(("recent-card", index))
            .relative()
            .h(px(276.))
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(px(16.))
            .bg(colors.card)
            .border_1()
            .border_color(colors.border)
            .cursor_pointer()
            .hover({
                let hover_border = colors.border_strong;
                move |style| style.border_color(hover_border).shadow_md()
            })
            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                if this.home.menu_open == Some(index) {
                    this.home.menu_open = None;
                    cx.notify();
                    return;
                }
                this.open_recent(index, cx);
            }))
            .child(
                // 页面预览区域 (332x200, 10px 圆角)
                div()
                    .h(px(200.))
                    .w_full()
                    .overflow_hidden()
                    .rounded(px(10.))
                    .bg(colors.preview_bg)
                    .border_1()
                    .border_color(colors.border)
                    .child(match thumbnail {
                        Some(thumb) => div()
                            .size_full()
                            .child(
                                img(thumb.image)
                                    .size_full()
                                    .object_fit(ObjectFit::Fill),
                            )
                            .into_any_element(),
                        None => {
                            // 渲染与设计图一致的书页预览排版
                            self.home_card_page_mockup(&recent.title, &recent.path, colors)
                                .into_any_element()
                        }
                    }),
            )
            .child(
                // 底部卡片文案行
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(15.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(colors.text)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child(meta),
                            ),
                    )
                    .child(
                        // 更多菜单按钮 "…"
                        div()
                            .id(("card-more", index))
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .text_size(px(16.))
                            .text_color(colors.muted)
                            .cursor_pointer()
                            .hover({
                                let hover_bg = colors.hover;
                                move |style| style.bg(hover_bg)
                            })
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
                            .on_click(cx.listener(|_this, _event: &ClickEvent, _window, cx| {
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

    /// 设计稿风格的占位书页微缩：含页眉小标题与微缩正文
    fn home_card_page_mockup(
        &self,
        title: &str,
        _path: &std::path::Path,
        colors: DesignPalette,
    ) -> gpui::Div {
        let sample_text = if title.contains("資治") || title.contains("胡注") {
            "周威烈王二十三年，初命晉大夫魏斯、趙籍、韓虔為諸侯。臣光曰：臣聞天子之職莫大於禮，禮莫大於分，分莫大於名。何謂禮？紀綱是也；何謂分？君臣是也；何謂名？公侯卿大夫是也。夫以四海之廣，兆民之眾，受制於一人，雖有絕倫之力、高世之智，莫敢不奔走而服役者，豈非以禮為之綱紀哉。"
        } else if title.contains("國史") {
            "中國歷史之演進，可分三期言之。上古之世，由部落而封建，秦漢以下，由封建而郡縣，此第一期也。魏晉以降，北方民族入居內地，至隋唐而混一，此第二期也。宋元明清，則第三期也。三期之中，皆有由分裂而統一之勢，亦皆有由統一而分裂之勢。"
        } else if title.contains("ESP32") || title.contains("规格") {
            "Features: Xtensa LX7 dual-core 32-bit microprocessor, up to 240 MHz. 512 KB on-chip SRAM, 384 KB ROM, 16 KB RTC SRAM. 8 MB PSRAM and 16 MB SPI flash in-package. 45 programmable GPIOs with flexible peripheral pin assignment. IEEE 802.11 b/g/n Wi-Fi, Bluetooth 5 LE with extended advertising."
        } else if title.contains("警世") {
            "話說國朝成化年間，山東青州府有一個秀才，姓范名二郎，為人忠厚，家道貧寒，卻喜讀書，不肯營生。一日天氣嚴寒，尚未有雪，他獨自一個在書房裡坐著，忽聽得天井裡有腳步響，抬起頭來，卻是一個年少的婦人，生得十分美貌。"
        } else if title.contains("论文") {
            "本文所關注的核心問題，在於地方財政壓力如何影響基礎設施投資的效率。既有研究大致可分為三支。其一從財政分權的視角出發，認為地方政府在稅收分成與轉移支付制度下形成了顯著的重視生產、輕視民生的支出偏向，並據此解釋了基礎設施投資的過度擴張。"
        } else {
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Integer posuere erat a ante venenatis dapibus posuere velit aliquet. Cras mattis consectetur purus sit amet fermentum. Donec ullamcorper nulla non metus auctor fringilla. Maecenas faucibus mollis interdum."
        };

        div()
            .size_full()
            .px(px(18.))
            .py(px(16.))
            .bg(rgb(0xffffff))
            .flex()
            .flex_col()
            .gap(px(9.))
            .child(
                div()
                    .truncate()
                    .text_size(px(8.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(colors.text)
                    .child(title.to_string()),
            )
            .child(
                div()
                    .text_size(px(9.))
                    .line_height(relative(1.5))
                    .text_color(colors.subtle)
                    .child(sample_text),
            )
    }

    // ---------- 卡片操作菜单 ----------

    fn home_recent_menu(
        &self,
        index: usize,
        colors: DesignPalette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let hover_bg = colors.hover;
        let is_fav = self.home.recents.get(index).is_some_and(|r| r.favorite);

        div()
            .absolute()
            .bottom(px(40.))
            .right(px(12.))
            .w(px(150.))
            .p(px(4.))
            .rounded(px(8.))
            .bg(colors.card)
            .border_1()
            .border_color(colors.border)
            .shadow_lg()
            .on_mouse_down(gpui::MouseButton::Left, |_event, _window, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .id(("recent-fav", index))
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .text_size(px(12.))
                    .text_color(if is_fav { colors.accent } else { colors.text })
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover_bg))
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.home.toggle_favorite(index);
                        this.set_status(
                            if this.home.recents.get(index).is_some_and(|r| r.favorite) {
                                "已加入收藏"
                            } else {
                                "已取消收藏"
                            },
                            cx,
                        );
                        cx.notify();
                    }))
                    .child(if is_fav { "取消收藏" } else { "加入收藏" }),
            )
            .child(
                div()
                    .id(("recent-copy", index))
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
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
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .text_size(px(12.))
                    .text_color(rgb(0xff3b30)) // 危险操作警示红
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

    // =========================================================================
    // 侧边栏独立页面：书签、全文搜索、词典、标注、关于
    // =========================================================================

    // ---------- 书签页面 ----------
    fn home_bookmarks_page(
        &self,
        colors: DesignPalette,
        _theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let bookmarks: Vec<&RecentDocument> = self
            .home
            .recents
            .iter()
            .filter(|r| r.progress.is_some())
            .collect();

        let mut list = div().flex().flex_col().gap(px(12.));
        if bookmarks.is_empty() {
            list = list.child(
                div()
                    .w_full()
                    .h(px(180.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .rounded(px(16.))
                    .border_1()
                    .border_dashed()
                    .border_color(colors.border)
                    .bg(colors.card)
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(colors.text)
                            .child("暂无书签与阅读进度"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child("打开并阅读任意文档后，gloss 会自动记录各文档的书签与断点"),
                    ),
            );
        } else {
            for (idx, recent) in bookmarks.into_iter().enumerate() {
                let path = recent.path.clone();
                let title = recent.title.clone();
                let time = relative_time(recent.opened_at);
                let p = recent.progress.as_ref().unwrap();
                let page_now = p.page.min(p.pages.saturating_sub(1)) + 1;
                let percent = p.percent();

                list = list.child(
                    div()
                        .id(("bookmark-row", idx))
                        .h(px(64.))
                        .px(px(20.))
                        .flex()
                        .items_center()
                        .gap(px(16.))
                        .rounded(px(14.))
                        .bg(colors.card)
                        .border_1()
                        .border_color(colors.border)
                        .cursor_pointer()
                        .hover({
                            let hover_border = colors.border_strong;
                            move |style| style.border_color(hover_border).shadow_sm()
                        })
                        .on_click(cx.listener(move |this, _event: &ClickEvent, _window, _cx| {
                            this.open_path(path.clone());
                        }))
                        .child(
                            svg()
                                .path("icons/bookmark.svg")
                                .size(px(18.))
                                .text_color(colors.accent),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(14.))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(colors.text)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(colors.muted)
                                        .child(format!("第 {page_now} / {} 页 · 上次阅读 {time}", p.pages)),
                                ),
                        )
                        .child(
                            div()
                                .px(px(10.))
                                .py(px(4.))
                                .rounded_full()
                                .bg(colors.bg)
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(colors.subtle)
                                .child(format!("{percent}%")),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(colors.accent)
                                .child("恢复阅读 →"),
                        ),
                );
            }
        }

        div()
            .id("bookmarks-page")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .scrollbar_width(px(8.))
            .child(
                div()
                    .px(px(40.))
                    .py(px(30.))
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(colors.muted)
                                    .child("书签"),
                            )
                            .child(
                                div()
                                    .text_size(px(32.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(colors.text)
                                    .child("书签与阅读进度"),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(colors.subtle)
                                    .child("浏览每本书上次阅读的位置与进度，点击直接恢复阅读"),
                            ),
                    )
                    .child(list),
            )
            .into_any_element()
    }

    // ---------- 独立词典与生词本页面 ----------
    fn home_dictionary_page(
        &self,
        colors: DesignPalette,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let history = self.dict_history.entries().to_vec();
        let entries = self.dictionary.entries.clone();

        div()
            .id("dictionary-page")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .scrollbar_width(px(8.))
            .child(
                div()
                    .px(px(40.))
                    .py(px(30.))
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(colors.muted)
                                    .child("工具"),
                            )
                            .child(
                                div()
                                    .text_size(px(32.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(colors.text)
                                    .child("离线词典与生词本"),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(colors.subtle)
                                    .child("支持汉字、多字词语及外文文献查词；在阅读器中双击字词也可一键钉住页边笺注"),
                            ),
                    )
                    .child(
                        // 查词卡片
                        div()
                            .p(px(20.))
                            .rounded(px(16.))
                            .bg(colors.card)
                            .border_1()
                            .border_color(colors.border)
                            .flex()
                            .flex_col()
                            .gap(px(16.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .h(px(40.))
                                            .px(px(12.))
                                            .flex()
                                            .items_center()
                                            .rounded(px(10.))
                                            .bg(colors.bg)
                                            .border_1()
                                            .border_color(colors.border)
                                            .child(self.dict_input.clone()),
                                    )
                                    .child(
                                        div()
                                            .id("dict-page-submit-btn")
                                            .h(px(40.))
                                            .px(px(24.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded_full()
                                            .bg(colors.accent)
                                            .text_size(px(13.))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(rgb(0xffffff))
                                            .cursor_pointer()
                                            .hover({
                                                let h = colors.accent_hover;
                                                move |style| style.bg(h)
                                            })
                                            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                                this.submit_dict_query(cx);
                                            }))
                                            .child("查询词典"),
                                    ),
                            )
                            .child(
                                // 查词历史标签
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(colors.muted)
                                            .child("最近查词："),
                                    )
                                    .children(history.into_iter().take(12).enumerate().map(|(idx, term)| {
                                        let t = term.clone();
                                        div()
                                            .id(("dict-page-hist", idx))
                                            .px(px(10.))
                                            .py(px(4.))
                                            .rounded_full()
                                            .bg(colors.bg)
                                            .text_size(px(12.))
                                            .text_color(colors.text)
                                            .cursor_pointer()
                                            .hover({
                                                let h = colors.hover;
                                                move |style| style.bg(h)
                                            })
                                            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                                                this.query_dict_term(t.clone(), cx);
                                            }))
                                            .child(term)
                                    })),
                            ),
                    )
                    .child(
                        // 释义呈现
                        div()
                            .p(px(24.))
                            .rounded(px(16.))
                            .bg(colors.card)
                            .border_1()
                            .border_color(colors.border)
                            .child(if entries.is_empty() {
                                div()
                                    .py(px(40.))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(colors.text)
                                            .child("在上方输入字词或点击历史标签查词"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(colors.muted)
                                            .child("已加载词典将自动联合检索并排版展现释义与例句"),
                                    )
                                    .into_any_element()
                            } else {
                                let mut entries_col = div().flex().flex_col().gap(px(16.));
                                for entry in &entries {
                                    entries_col = entries_col.child(crate::ui::dictionary::entry_element(
                                        entry,
                                        theme,
                                        &self.dict_query,
                                        0,
                                    ));
                                }
                                entries_col.into_any_element()
                            }),
                    ),
            )
            .into_any_element()
    }

    // ---------- 标注管理页面 ----------
    fn home_annotations_page(
        &self,
        colors: DesignPalette,
        _theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // 汇集最近阅读文档的 sidecar 批注
        struct HomeAnnotationItem {
            doc_title: String,
            doc_path: std::path::PathBuf,
            page: usize,
            is_gloss: bool,
            title: String,
            desc: String,
        }

        let mut all_annotations: Vec<HomeAnnotationItem> = Vec::new();
        for recent in &self.home.recents {
            let sidecar = crate::highlights::sidecar_path(&recent.path);
            if let Ok(text) = std::fs::read_to_string(&sidecar) {
                if let Ok(store) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(items) = store.get("items").or_else(|| store.get("highlights")).and_then(|v| v.as_array()) {
                        for it in items {
                            let text = it.get("text").and_then(|v| v.as_str()).unwrap_or("高亮选段");
                            let page = it.get("page").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            all_annotations.push(HomeAnnotationItem {
                                doc_title: recent.title.clone(),
                                doc_path: recent.path.clone(),
                                page,
                                is_gloss: false,
                                title: text.to_string(),
                                desc: "划词高亮".to_string(),
                            });
                        }
                    }
                    if let Some(glosses) = store.get("glosses").and_then(|v| v.as_array()) {
                        for g in glosses {
                            let word = g.get("word").and_then(|v| v.as_str()).unwrap_or("");
                            let summary = g.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                            let page = g.get("page").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            all_annotations.push(HomeAnnotationItem {
                                doc_title: recent.title.clone(),
                                doc_path: recent.path.clone(),
                                page,
                                is_gloss: true,
                                title: word.to_string(),
                                desc: summary.to_string(),
                            });
                        }
                    }
                }
            }
        }

        let mut list = div().flex().flex_col().gap(px(12.));
        if all_annotations.is_empty() {
            list = list.child(
                div()
                    .w_full()
                    .h(px(180.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .rounded(px(16.))
                    .border_1()
                    .border_dashed()
                    .border_color(colors.border)
                    .bg(colors.card)
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(colors.text)
                            .child("暂无文档标注与页边笺注"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child("阅读 PDF 时选中文字按 H 可高亮，查词时点「钉住」可保存笺注"),
                    ),
            );
        } else {
            for (idx, item) in all_annotations.into_iter().enumerate() {
                let doc_path = item.doc_path.clone();
                list = list.child(
                    div()
                        .id(("annot-row", idx))
                        .p(px(16.))
                        .flex()
                        .items_center()
                        .gap(px(16.))
                        .rounded(px(14.))
                        .bg(colors.card)
                        .border_1()
                        .border_color(colors.border)
                        .cursor_pointer()
                        .hover({
                            let hover_border = colors.border_strong;
                            move |style| style.border_color(hover_border).shadow_sm()
                        })
                        .on_click(cx.listener(move |this, _event: &ClickEvent, _window, _cx| {
                            this.open_path(doc_path.clone());
                        }))
                        .child(
                            div()
                                .size(px(32.))
                                .rounded(px(8.))
                                .bg(colors.bg)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    svg()
                                        .path(if item.is_gloss { "icons/dictionary.svg" } else { "icons/highlight.svg" })
                                        .size(px(16.))
                                        .text_color(if item.is_gloss { colors.accent } else { rgb(0xff9500) }),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(14.))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(colors.text)
                                        .child(item.title),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(12.))
                                        .text_color(colors.muted)
                                        .child(item.desc),
                                ),
                        )
                        .child(
                            div()
                                .text_right()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .truncate()
                                        .max_w(px(180.))
                                        .text_size(px(11.))
                                        .text_color(colors.subtle)
                                        .child(item.doc_title),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(colors.muted)
                                        .child(format!("第 {} 页", item.page + 1)),
                                ),
                        ),
                );
            }
        }

        div()
            .id("annotations-page")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .scrollbar_width(px(8.))
            .child(
                div()
                    .px(px(40.))
                    .py(px(30.))
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(colors.muted)
                                    .child("工具"),
                            )
                            .child(
                                div()
                                    .text_size(px(32.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(colors.text)
                                    .child("我的标注与笺注"),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(colors.subtle)
                                    .child("集中归档与回看所有文档中的划词高亮与页边词典笺注"),
                            ),
                    )
                    .child(list),
            )
            .into_any_element()
    }

    // ---------- 关于页面 ----------
    fn home_about_page(
        &self,
        colors: DesignPalette,
        _theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id("about-page")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .scrollbar_width(px(8.))
            .child(
                div()
                    .px(px(40.))
                    .py(px(30.))
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(colors.muted)
                                    .child("关于"),
                            )
                            .child(
                                div()
                                    .text_size(px(32.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(colors.text)
                                    .child("笺释 gloss-pdf"),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(colors.subtle)
                                    .child("基于 GPUI 高性能 GPU 渲染框架与 PDFium 引擎构建的深度批注阅读器"),
                            ),
                    )
                    .child(
                        // 版本与概览卡片
                        div()
                            .p(px(24.))
                            .rounded(px(16.))
                            .bg(colors.card)
                            .border_1()
                            .border_color(colors.border)
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(16.))
                                    .child(
                                        div()
                                            .size(px(48.))
                                            .rounded(px(14.))
                                            .bg(colors.accent)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                svg()
                                                    .path("icons/open.svg")
                                                    .size(px(26.))
                                                    .text_color(rgb(0xffffff)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(4.))
                                            .child(
                                                div()
                                                    .text_size(px(18.))
                                                    .font_weight(gpui::FontWeight::BOLD)
                                                    .text_color(colors.text)
                                                    .child(format!("gloss-pdf v{}", env!("CARGO_PKG_VERSION"))),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(colors.muted)
                                                    .child("原生 Rust 编写 · GPU 硬件加速排版 · 离线多本 MDX 辞典"),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .id("about-page-check-update-btn")
                                    .px(px(16.))
                                    .py(px(8.))
                                    .rounded_full()
                                    .bg(colors.bg)
                                    .border_1()
                                    .border_color(colors.border)
                                    .text_size(px(12.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(colors.text)
                                    .cursor_pointer()
                                    .hover({
                                        let h = colors.hover;
                                        move |style| style.bg(h)
                                    })
                                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                        this.check_for_updates(cx);
                                    }))
                                    .child("检查更新"),
                            ),
                    )
                    .child(
                        // 技术架构与致谢网格
                        div()
                            .grid()
                            .grid_cols(3)
                            .gap(px(16.))
                            .child(self.empty_feature_card(
                                "icons/open.svg",
                                "GPUI 渲染管线",
                                "秒开 8000+ 页巨型古籍与技术文档",
                                colors,
                            ))
                            .child(self.empty_feature_card(
                                "icons/dictionary.svg",
                                "页边笺注（Marginalia）",
                                "传统古籍夹注排版体验，查词就地钉住",
                                colors,
                            ))
                            .child(self.empty_feature_card(
                                "icons/settings.svg",
                                "本地私密",
                                "所有阅读进度与批注完全存储于本地磁盘",
                                colors,
                            )),
                    ),
            )
            .into_any_element()
    }

}
