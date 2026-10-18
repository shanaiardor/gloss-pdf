//! 设置面板：主题、MDX 词典路径与更新。从首页侧栏或阅读器顶部打开。

use gpui::{ClickEvent, Context, FontWeight, div, prelude::*, px};

use crate::app::{AppState, UpdatePhase};
use crate::dictionary::DictLoadState;
use crate::theme::Theme;
use crate::update::CURRENT_VERSION;

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

        body = body
            .child(dictionary_section)
            .child(self.update_section(theme, cx))
            .child(
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

    /// 更新区块：当前版本、检查/下载/安装按钮、自动检查开关。
    fn update_section(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let mut section = div().flex().flex_col().gap(px(6.)).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .child("更新"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child(format!("当前版本 {CURRENT_VERSION}")),
                ),
        );

        let percent = self.update_phase.percent();
        section = match &self.update_phase {
            UpdatePhase::Idle => section
                .child(status_line("检查 GitHub 上是否有新版本。", theme))
                .child(action_button(
                    "settings-check-update",
                    "检查更新",
                    theme,
                    cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.check_for_updates(cx);
                    }),
                )),
            UpdatePhase::Checking => section.child(status_line("正在检查更新…", theme)),
            UpdatePhase::UpToDate { version } => section
                .child(status_line(format!("已是最新版本（{version}）。"), theme))
                .child(action_button(
                    "settings-check-update-again",
                    "再次检查",
                    theme,
                    cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.check_for_updates(cx);
                    }),
                )),
            UpdatePhase::Available(info) => {
                let notes = if info.notes.trim().is_empty() {
                    "可以下载并安装。".to_string()
                } else {
                    info.notes.clone()
                };
                section
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text)
                            .child(format!("发现新版本 {}", info.version)),
                    )
                    .child(status_line(notes, theme))
                    .child(action_button(
                        "settings-download-update",
                        "下载并安装",
                        theme,
                        cx.listener(|this, _event: &ClickEvent, _window, cx| {
                            this.download_update(cx);
                        }),
                    ))
            }
            UpdatePhase::Downloading { downloaded, total } => {
                let progress = match (percent, total.filter(|total| *total > 0)) {
                    (Some(percent), Some(total)) => format!(
                        "正在下载 {:.1} / {:.1} MB（{percent}%）",
                        *downloaded as f64 / 1_048_576.0,
                        total as f64 / 1_048_576.0
                    ),
                    _ => format!("正在下载 {:.1} MB…", *downloaded as f64 / 1_048_576.0),
                };
                section.child(status_line(progress, theme))
            }
            UpdatePhase::Downloaded(path) => {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                section
                    .child(status_line(format!("安装包已下载：{name}"), theme))
                    .child(action_button(
                        "settings-install-update",
                        "安装并重启",
                        theme,
                        cx.listener(|this, _event: &ClickEvent, _window, cx| {
                            this.install_update(cx);
                        }),
                    ))
            }
            UpdatePhase::Failed(message) => section
                .child(status_line(format!("更新失败：{message}"), theme))
                .child(action_button(
                    "settings-check-update-retry",
                    "重试",
                    theme,
                    cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.check_for_updates(cx);
                    }),
                )),
        };

        let auto = self.settings.current.auto_check_updates;
        let mut checkbox = div()
            .w(px(12.))
            .h(px(12.))
            .rounded_sm()
            .border_1()
            .border_color(theme.border);
        if auto {
            checkbox = checkbox.bg(theme.accent).border_color(theme.accent);
        }

        section.child(
            div()
                .id("settings-auto-update")
                .flex()
                .items_center()
                .gap(px(6.))
                .mt(px(2.))
                .cursor_pointer()
                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.toggle_auto_update_check(cx);
                }))
                .child(checkbox)
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child("启动时自动检查更新"),
                ),
        )
    }
}

fn status_line(text: impl Into<String>, theme: &Theme) -> gpui::Div {
    div()
        .text_size(px(11.))
        .text_color(theme.text_muted)
        .child(text.into())
}

fn action_button(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    theme: &Theme,
    listener: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
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
