//! 设置：主题、MDX 词典路径与更新。
//!
//! 首页侧栏的「设置」是文档区里的一页（[`AppState::settings_page`]），不是弹窗；
//! 阅读器里没有可替换的页面，才用浮动面板（[`AppState::settings_panel`]）。

use gpui::{ClickEvent, Context, FontWeight, div, prelude::*, px};

use crate::ai_settings::ProviderField;
use crate::app::{AppState, UpdatePhase};
use crate::dictionary::DictLoadState;
use crate::home::HomeNav;
use crate::theme::Theme;
use crate::update::CURRENT_VERSION;

impl AppState {
    /// 阅读器里的设置浮层。首页使用文档区里的 [`Self::settings_page`]，不走这里。
    pub(crate) fn settings_panel(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.settings_open || !self.viewer.has_document() {
            return None;
        }
        self.settings_view(theme, cx, false)
    }

    /// 首页侧栏「设置」对应的整页视图，直接占据右侧内容区。
    pub(crate) fn settings_page(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.home.nav != HomeNav::Settings {
            return None;
        }
        self.settings_view(theme, cx, true)
    }

    fn settings_view(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
        page: bool,
    ) -> Option<gpui::AnyElement> {
        let mut body = div().flex().flex_col().gap(px(16.));

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

        // 文档信息：只读展示 PDF 元数据，缺失字段用「—」。
        // 首页的设置页没有打开文档，整段省略，避免出现一排「未打开文档」。
        let metadata_section = self.viewer.doc.as_ref().map(|document| {
            let mut section = div().flex().flex_col().gap(px(4.)).child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .child("文档信息"),
            );
            for (label, value) in document.metadata.fields() {
                section = section.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .text_size(px(11.))
                        .child(
                            div()
                                .w(px(56.))
                                .flex_shrink_0()
                                .text_color(theme.text_muted)
                                .child(label),
                        )
                        .child(
                            div()
                                .min_w(px(0.))
                                .flex_1()
                                .truncate()
                                .text_color(theme.text)
                                .child(value.unwrap_or("—").to_string()),
                        ),
                );
            }
            section
        });

        // 词典
        let dictionaries = self.settings.current.dictionaries.clone();
        let mut dictionary_section = div().flex().flex_col().gap(px(8.)).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .child("词典"),
                )
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(theme.text_muted)
                        .child("按顺序查询，全部启用词典的结果都会显示"),
                ),
        );

        if dictionaries.is_empty() {
            dictionary_section = dictionary_section.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .child("添加一本或多本 .mdx 词典，双击 PDF 中的字词即可查询。"),
            );
        } else {
            for (index, dictionary) in dictionaries.iter().enumerate() {
                let name = dictionary
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| dictionary.path.display().to_string());
                let state = self
                    .dictionary
                    .status_for(&dictionary.path)
                    .cloned()
                    .unwrap_or(DictLoadState::Unconfigured);
                let status = dictionary_status_label(dictionary.enabled, &state);
                let status_color = if matches!(state, DictLoadState::Failed(_)) {
                    theme.accent
                } else {
                    theme.text_muted
                };

                dictionary_section = dictionary_section.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p_2()
                        .rounded_md()
                        .bg(theme.surface_hover)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(12.))
                                        .text_color(if dictionary.enabled {
                                            theme.text
                                        } else {
                                            theme.text_muted
                                        })
                                        .child(name),
                                )
                                .child(mini_button(
                                    ("settings-dict-toggle", index),
                                    if dictionary.enabled {
                                        "停用"
                                    } else {
                                        "启用"
                                    },
                                    theme,
                                    cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                                        this.toggle_dictionary(index, cx);
                                    }),
                                ))
                                .child(mini_button(
                                    ("settings-dict-up", index),
                                    "↑",
                                    theme,
                                    cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                                        this.move_dictionary(index, -1, cx);
                                    }),
                                ))
                                .child(mini_button(
                                    ("settings-dict-down", index),
                                    "↓",
                                    theme,
                                    cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                                        this.move_dictionary(index, 1, cx);
                                    }),
                                ))
                                .child(mini_button(
                                    ("settings-dict-remove", index),
                                    "移除",
                                    theme,
                                    cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                                        this.remove_dictionary(index, cx);
                                    }),
                                )),
                        )
                        .child(
                            div()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(10.))
                                .text_color(theme.text_muted)
                                .child(dictionary.path.display().to_string()),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(status_color)
                                .child(status),
                        ),
                );
            }
        }

        dictionary_section = dictionary_section.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .mt(px(2.))
                .child(action_button(
                    "settings-pick-dictionary",
                    "添加词典",
                    theme,
                    cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.choose_dictionary(cx);
                    }),
                ))
                .child(action_button(
                    "settings-clear-dictionary",
                    "全部清除",
                    theme,
                    cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.clear_dictionary(cx);
                    }),
                )),
        );

        if let Some(metadata_section) = metadata_section {
            body = body.child(metadata_section);
        }
        body = body
            .child(dictionary_section)
            .child(self.ai_section(theme, cx))
            .child(self.update_section(theme, cx))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .child(if page {
                        "提示：设置修改后立即生效，Esc 返回首页。"
                    } else {
                        "提示：双击单词或汉字查词，Esc 关闭面板。"
                    }),
            );

        if page {
            return Some(
                div()
                    .id("settings-page")
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .scrollbar_width(px(8.))
                    .child(
                        div().w_full().flex().justify_center().child(
                            div()
                                .w_full()
                                .max_w(px(760.))
                                .px(px(32.))
                                .pt(px(34.))
                                .pb(px(56.))
                                .flex()
                                .flex_col()
                                .gap(px(24.))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(4.))
                                        .child(
                                            div()
                                                .text_size(px(24.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme.text)
                                                .child("设置"),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(12.))
                                                .text_color(theme.text_muted)
                                                .child("主题、词典与更新"),
                                        ),
                                )
                                .child(
                                    div()
                                        .p(px(22.))
                                        .rounded_lg()
                                        .bg(theme.surface)
                                        .border_1()
                                        .border_color(theme.border)
                                        .child(body),
                                ),
                        ),
                    )
                    .into_any_element(),
            );
        }

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
                        .child(div().p_3().child(body)),
                )
                .into_any_element(),
        )
    }

    /// AI 区块：启用开关、供应商卡片列表、查词模型。
    ///
    /// 供应商是动态列表，卡片默认收起（只露名称 / 类型 / Key 状态）；
    /// 一次只展开一张，展开时才创建字段输入框（见 `ai_settings.rs`）。
    fn ai_section(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let ai = &self.settings.current.ai;
        let ready = ai.is_ready();
        let status = if !ai.lookup.enabled {
            "已关闭".to_string()
        } else if ready {
            match ai.active_provider() {
                Some(entry) => format!("已就绪 · {} / {}", entry.name, ai.lookup.model.trim()),
                None => "已就绪".to_string(),
            }
        } else {
            match ai.validate() {
                Err(error) => error.to_string(),
                Ok(()) => "待配置".to_string(),
            }
        };

        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .child("AI"),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(if ready {
                                theme.text_muted
                            } else {
                                theme.text_muted
                            })
                            .child(status),
                    ),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(theme.text_muted)
                    .child("启用后，查询的词以及当前页 / 所需页的文本会发送到你配置的供应商。"),
            );

        // 总开关
        let enabled = ai.lookup.enabled;
        section = section.child(
            div()
                .id("ai-enabled")
                .flex()
                .items_center()
                .gap(px(6.))
                .cursor_pointer()
                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.toggle_ai_enabled(cx);
                }))
                .child(checkbox(enabled, theme))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child("在查词时启用 AI 释义"),
                ),
        );

        let providers = ai.providers.clone();
        let active_id = ai.lookup.provider_id.clone();

        if providers.is_empty() {
            section = section.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .child("还没有供应商。添加后即可在查不到词时让 AI 补充解释。"),
            );
        }

        for (index, entry) in providers.iter().enumerate() {
            let id = entry.id.clone();
            let expanded = self.ai_ui.expanded.as_deref() == Some(id.as_str());
            let in_use = active_id.as_deref() == Some(id.as_str());

            let mut card = div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .p_2()
                .rounded_md()
                .bg(theme.surface_hover)
                .child(
                    div()
                        .id(("ai-provider-header", index))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                            this.toggle_provider_expanded(&id, cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(12.))
                                .text_color(theme.text)
                                .child(if entry.name.trim().is_empty() {
                                    entry.kind.label().to_string()
                                } else {
                                    entry.name.clone()
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme.text_muted)
                                .child(entry.kind.label()),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(if entry.api_key.trim().is_empty() {
                                    theme.text_muted
                                } else {
                                    theme.text
                                })
                                .child(if entry.api_key.trim().is_empty() {
                                    "未填 Key".to_string()
                                } else {
                                    entry.masked_key()
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(if in_use { theme.accent } else { theme.text_muted })
                                .child(if in_use { "查词使用中" } else { "" }),
                        ),
                );

            if expanded {
                let id = entry.id.clone();
                card = card.child(ai_field_row(
                    "名称",
                    self.ai_field_input(&id, ProviderField::Name),
                    theme,
                ));
                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ai_field_label("类型", theme))
                        .child(
                            div()
                                .id(("ai-provider-kind", index))
                                .px_2()
                                .rounded_sm()
                                .cursor_pointer()
                                .bg(theme.surface)
                                .text_size(px(11.))
                                .text_color(theme.text)
                                .hover({
                                    let hover_bg = theme.border;
                                    move |style| style.bg(hover_bg)
                                })
                                .on_click(cx.listener(
                                    move |this, _event: &ClickEvent, _window, cx| {
                                        this.cycle_provider_kind(&id, cx);
                                    },
                                ))
                                .child(entry.kind.label()),
                        ),
                );

                let revealed = self.ai_ui.revealed.contains(&entry.id);
                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ai_field_label("API Key", theme))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(ai_input_slot(
                                    self.ai_field_input(&entry.id, ProviderField::ApiKey),
                                    theme,
                                )),
                        )
                        .child(mini_button(
                            ("ai-key-reveal", index),
                            if revealed { "隐藏" } else { "显示" },
                            theme,
                            cx.listener({
                                let id = entry.id.clone();
                                move |this, _event: &ClickEvent, _window, cx| {
                                    this.toggle_key_reveal(&id, cx);
                                }
                            }),
                        )),
                );

                if entry.kind.needs_base_url() {
                    card = card.child(ai_field_row(
                        "Base URL",
                        self.ai_field_input(&entry.id, ProviderField::BaseUrl),
                        theme,
                    ));
                }

                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .mt(px(2.))
                        .child(mini_button(
                            ("ai-provider-use", index),
                            if in_use { "查词使用中" } else { "用于查词" },
                            theme,
                            cx.listener({
                                let id = entry.id.clone();
                                move |this, _event: &ClickEvent, _window, cx| {
                                    this.select_ai_provider(&id, cx);
                                }
                            }),
                        ))
                        .child(mini_button(
                            ("ai-provider-remove", index),
                            "移除",
                            theme,
                            cx.listener({
                                let id = entry.id.clone();
                                move |this, _event: &ClickEvent, _window, cx| {
                                    this.remove_ai_provider(&id, cx);
                                }
                            }),
                        )),
                );
            }

            section = section.child(card);
        }

        if self.ai_ui.adding {
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.text_muted)
                            .child("供应商类型"),
                    )
                    .child(mini_button(
                        "ai-add-openai",
                        "OpenAI",
                        theme,
                        cx.listener(|this, _event: &ClickEvent, _window, cx| {
                            this.add_provider_of_kind(ai_core::ProviderKind::OpenAi, cx);
                        }),
                    ))
                    .child(mini_button(
                        "ai-add-compat",
                        "兼容端点",
                        theme,
                        cx.listener(|this, _event: &ClickEvent, _window, cx| {
                            this.add_provider_of_kind(ai_core::ProviderKind::OpenAiCompat, cx);
                        }),
                    ))
                    .child(mini_button(
                        "ai-add-anthropic",
                        "Anthropic",
                        theme,
                        cx.listener(|this, _event: &ClickEvent, _window, cx| {
                            this.add_provider_of_kind(ai_core::ProviderKind::Anthropic, cx);
                        }),
                    ))
                    .child(mini_button(
                        "ai-add-cancel",
                        "取消",
                        theme,
                        cx.listener(|this, _event: &ClickEvent, _window, cx| {
                            this.cancel_add_provider(cx);
                        }),
                    )),
            );
        } else {
            section = section.child(action_button(
                "ai-add-provider",
                "添加供应商",
                theme,
                cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.begin_add_provider(cx);
                }),
            ));
        }

        // 查词功能：模型名挂在功能层，不跟随供应商
        section = match self.ai_ui.model_input.clone() {
            Some(input) => section.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ai_field_label("模型", theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(ai_input_slot(Some(input), theme)),
                    ),
            ),
            None => section.child(
                div()
                    .text_size(px(10.))
                    .text_color(theme.text_muted)
                    .child("展开任一供应商后即可填写查词用的模型名。"),
            ),
        };

        section
    }

    /// 取某供应商某字段的输入框；还没创建时返回空盒子（展开动作里会先建好）。
    fn ai_field_input(
        &self,
        provider_id: &str,
        field: ProviderField,
    ) -> Option<gpui::Entity<crate::ui::text_input::TextInput>> {
        self.ai_ui
            .provider_inputs
            .get(&(provider_id.to_string(), field))
            .cloned()
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

/// 复选框小方块：勾选时填强调色，与更新区块保持一致。
fn checkbox(checked: bool, theme: &Theme) -> gpui::Div {
    let mut box_ = div()
        .w(px(12.))
        .h(px(12.))
        .rounded_sm()
        .border_1()
        .border_color(theme.border);
    if checked {
        box_ = box_.bg(theme.accent).border_color(theme.accent);
    }
    box_
}

fn ai_field_label(text: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .w(px(62.))
        .flex_shrink_0()
        .text_size(px(10.))
        .text_color(theme.text_muted)
        .child(text)
}

/// 输入框或「还没创建」占位；两者都是 `AnyElement`，调用处不必再分支。
fn ai_input_slot(
    input: Option<gpui::Entity<crate::ui::text_input::TextInput>>,
    theme: &Theme,
) -> gpui::AnyElement {
    match input {
        Some(input) => input.into_any_element(),
        None => div()
            .text_size(px(10.))
            .text_color(theme.text_muted)
            .child("…")
            .into_any_element(),
    }
}

/// 一行「标签 + 输入框」；输入框还没创建时给个占位提示，避免行高塌陷。
fn ai_field_row(
    label: &'static str,
    input: Option<gpui::Entity<crate::ui::text_input::TextInput>>,
    theme: &Theme,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(ai_field_label(label, theme))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .child(ai_input_slot(input, theme)),
        )
}

fn status_line(text: impl Into<String>, theme: &Theme) -> gpui::Div {    div()
        .text_size(px(11.))
        .text_color(theme.text_muted)
        .child(text.into())
}

fn dictionary_status_label(enabled: bool, state: &DictLoadState) -> String {
    if !enabled {
        return "已停用".to_string();
    }
    match state {
        DictLoadState::Unconfigured => "未加载".to_string(),
        DictLoadState::Loading => "正在加载…".to_string(),
        DictLoadState::Ready { title, entries } => match title {
            Some(title) => format!("已加载 {entries} 条 · {title}"),
            None => format!("已加载 {entries} 条"),
        },
        DictLoadState::Failed(message) => format!("加载失败：{message}"),
    }
}

fn mini_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    theme: &Theme,
    listener: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_1()
        .rounded_sm()
        .cursor_pointer()
        .text_size(px(10.))
        .text_color(theme.text_muted)
        .hover({
            let hover_bg = theme.border;
            move |style| style.bg(hover_bg)
        })
        .on_click(listener)
        .child(label)
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
        .self_start()
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
