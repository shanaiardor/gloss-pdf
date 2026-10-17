//! 设置面板：主题与 MDX 词典路径。从首页侧栏或阅读器顶部打开。

use gpui::{ClickEvent, Context, FontWeight, div, prelude::*, px};

use crate::app::AppState;
use crate::dictionary::DictLoadState;
use crate::theme::Theme;

impl AppState {
    pub(crate) fn settings_panel(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        if !self.settings_open {
            return None;
        }

        let mut body = div().p_3().flex().flex_col().gap(px(16.));

        // 外观
        body = body.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .child("外观"),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.text_muted)
                                .child("浅色 / 深色"),
                        ),
                )
                .child(
                    div()
                        .id("settings-theme")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(theme.surface_hover)
                        .text_size(px(12.))
                        .text_color(theme.text)
                        .hover({
                            let hover_bg = theme.border;
                            move |style| style.bg(hover_bg)
                        })
                        .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                            this.toggle_theme(cx);
                        }))
                        .child(self.theme.name()),
                ),
        );

        // 词典
        let path = self.settings.current.dictionary_path.clone();
        let status = match &self.dictionary.load_state {
            DictLoadState::Unconfigured => "未选择词典".to_string(),
            DictLoadState::Loading => "正在加载…".to_string(),
            DictLoadState::Ready { title, entries } => match title {
                Some(title) => format!("已加载 {entries} 条 · {title}"),
                None => format!("已加载 {entries} 条"),
            },
            DictLoadState::Failed(message) => format!("加载失败：{message}"),
        };

        let mut dictionary_section = div().flex().flex_col().gap(px(6.)).child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .child("词典"),
        );

        if let Some(path) = &path {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());

            dictionary_section = dictionary_section
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(12.))
                        .text_color(theme.text)
                        .child(name),
                )
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child(path.display().to_string()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child(status),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .mt(px(4.))
                        .child(action_button(
                            "settings-pick-dictionary",
                            "更换词典",
                            theme,
                            cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                this.choose_dictionary(cx);
                            }),
                        ))
                        .child(action_button(
                            "settings-clear-dictionary",
                            "清除",
                            theme,
                            cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                this.clear_dictionary(cx);
                            }),
                        )),
                );
        } else {
            dictionary_section = dictionary_section
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child("选择一本 .mdx 词典，双击 PDF 中的字词即可查询。"),
                )
                .child(div().mt(px(4.)).child(action_button(
                    "settings-pick-dictionary",
                    "选择 .mdx 文件",
                    theme,
                    cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.choose_dictionary(cx);
                    }),
                )));
        }

        body = body.child(dictionary_section).child(
            div()
                .text_size(px(11.))
                .text_color(theme.text_muted)
                .child("提示：双击单词或汉字查词，Esc 关闭面板。"),
        );

        Some(
            div()
                .absolute()
                .top(px(84.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .w(px(420.))
                        .flex()
                        .flex_col()
                        .occlude()
                        .rounded_lg()
                        .bg(theme.surface.opacity(0.98))
                        .border_1()
                        .border_color(theme.border)
                        .shadow_lg()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .h(px(40.))
                                .px_3()
                                .border_b_1()
                                .border_color(theme.border)
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("设置")
                                .child(
                                    div()
                                        .id("settings-close")
                                        .px_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .text_color(theme.text_muted)
                                        .hover({
                                            let hover_bg = theme.surface_hover;
                                            move |style| style.bg(hover_bg)
                                        })
                                        .on_click(cx.listener(
                                            |this, _event: &ClickEvent, _window, cx| {
                                                this.toggle_settings(cx);
                                            },
                                        ))
                                        .child("✕"),
                                ),
                        )
                        .child(body),
                ),
        )
    }
}

fn action_button(
    id: &'static str,
    label: &'static str,
    theme: &Theme,
    listener: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(theme.accent)
        .text_color(theme.accent_text)
        .text_size(px(12.))
        .hover({
            let hover_bg = theme.accent_hover;
            move |style| style.bg(hover_bg)
        })
        .on_click(listener)
        .child(label)
}
