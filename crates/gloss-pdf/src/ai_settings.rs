//! AI 设置页的交互状态与动作。
//!
//! 供应商是动态列表，字段输入框按需创建后缓存在 [`AiUiState`]：折叠时不建、
//! 展开时才建。这样设置页不会常驻一堆没人看的空输入框，也不用为「任意多个
//! 供应商」预先分配固定数量的控件。

use std::collections::{HashMap, HashSet};

use ai_core::{ProviderEntry, ProviderKind};
use gpui::{AppContext as _, Context, Entity, Focusable as _};

use crate::app::AppState;
use crate::ui::text_input::{TextInput, TextInputEvent};

/// 供应商卡片里的可编辑字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ProviderField {
    Name,
    ApiKey,
    BaseUrl,
}

/// AI 设置页的瞬时 UI 状态，不落盘。
#[derive(Default)]
pub(crate) struct AiUiState {
    /// 当前展开编辑的供应商 id；一次只展开一个。
    pub expanded: Option<String>,
    /// 是否正在选择新供应商的类型。
    pub adding: bool,
    /// 各供应商字段的输入框，按 (供应商 id, 字段) 缓存。
    pub provider_inputs: HashMap<(String, ProviderField), Entity<TextInput>>,
    /// 查词功能的模型名输入框。
    pub model_input: Option<Entity<TextInput>>,
    /// 点了「显示」的供应商 id，展开时以明文显示 key。
    pub revealed: HashSet<String>,
}

impl AppState {
    /// 确保某个供应商的三个字段输入框存在（展开卡片时调用）。
    pub(crate) fn ensure_provider_inputs(&mut self, id: &str, cx: &mut Context<Self>) {
        if self
            .ai_ui
            .provider_inputs
            .contains_key(&(id.to_string(), ProviderField::Name))
        {
            return;
        }
        let Some(entry) = self.settings.current.ai.provider(id).cloned() else {
            return;
        };
        let theme = self.theme.clone();

        for (field, placeholder) in [
            (ProviderField::Name, "显示名称"),
            (ProviderField::ApiKey, "API Key"),
            (ProviderField::BaseUrl, "https://…/v1"),
        ] {
            let initial = match field {
                ProviderField::Name => entry.name.clone(),
                ProviderField::ApiKey => entry.api_key.clone(),
                ProviderField::BaseUrl => entry.base_url.clone().unwrap_or_default(),
            };
            let input = cx.new(|cx| {
                let mut input = TextInput::new(placeholder, theme.clone(), 12.0, cx);
                input.set_value(initial, cx);
                input
            });

            let owner = id.to_string();
            cx.subscribe(&input, move |this, input, event: &TextInputEvent, cx| {
                match event {
                    TextInputEvent::Changed => {
                        let value = input.read(cx).value().to_string();
                        this.write_provider_field(&owner, field, value);
                        this.schedule_settings_save(cx);
                        cx.notify();
                    }
                    // 失焦但保留内容：刚粘贴的 key 不该被 Esc 抹掉。
                    TextInputEvent::Cancelled => {
                        this.pending_focus = Some(crate::app::FocusTarget::Root);
                        cx.notify();
                    }
                    TextInputEvent::Submitted { .. } => {}
                }
            })
            .detach();

            self.ai_ui.provider_inputs.insert((id.to_string(), field), input);
        }
    }

    /// 确保查词功能的模型名输入框存在。
    pub(crate) fn ensure_model_input(&mut self, cx: &mut Context<Self>) {
        if self.ai_ui.model_input.is_some() {
            return;
        }
        let theme = self.theme.clone();
        let initial = self.settings.current.ai.lookup.model.clone();
        let placeholder = self
            .settings
            .current
            .ai
            .active_provider()
            .map(|entry| entry.kind.default_model())
            .unwrap_or("模型名");

        let input = cx.new(|cx| {
            let mut input = TextInput::new(placeholder, theme.clone(), 12.0, cx);
            input.set_value(initial, cx);
            input
        });
        cx.subscribe(&input, |this, input, event: &TextInputEvent, cx| match event {
            TextInputEvent::Changed => {
                this.settings.current.ai.lookup.model = input.read(cx).value().to_string();
                this.schedule_settings_save(cx);
                cx.notify();
            }
            TextInputEvent::Cancelled => {
                this.pending_focus = Some(crate::app::FocusTarget::Root);
                cx.notify();
            }
            TextInputEvent::Submitted { .. } => {}
        })
        .detach();

        self.ai_ui.model_input = Some(input);
    }

    /// 把设置里的当前值推回已存在的输入框（外部修改配置后同步）。
    pub(crate) fn sync_ai_inputs(&mut self, cx: &mut Context<Self>) {
        let entries: Vec<ProviderEntry> = self.settings.current.ai.providers.clone();
        for entry in entries {
            for field in [ProviderField::Name, ProviderField::ApiKey, ProviderField::BaseUrl] {
                let Some(input) = self
                    .ai_ui
                    .provider_inputs
                    .get(&(entry.id.clone(), field))
                    .cloned()
                else {
                    continue;
                };
                let value = match field {
                    ProviderField::Name => entry.name.clone(),
                    ProviderField::ApiKey => entry.api_key.clone(),
                    ProviderField::BaseUrl => entry.base_url.clone().unwrap_or_default(),
                };
                input.update(cx, |input, cx| input.set_value(value, cx));
            }
        }
        if let Some(input) = self.ai_ui.model_input.clone() {
            let model = self.settings.current.ai.lookup.model.clone();
            input.update(cx, |input, cx| input.set_value(model, cx));
        }
    }

    fn write_provider_field(&mut self, id: &str, field: ProviderField, value: String) {
        let Some(entry) = self
            .settings
            .current
            .ai
            .providers
            .iter_mut()
            .find(|entry| entry.id == id)
        else {
            return;
        };
        match field {
            ProviderField::Name => entry.name = value,
            ProviderField::ApiKey => entry.api_key = value,
            ProviderField::BaseUrl => {
                let trimmed = value.trim();
                entry.base_url = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
            }
        }
    }

    pub(crate) fn toggle_ai_enabled(&mut self, cx: &mut Context<Self>) {
        let enabled = &mut self.settings.current.ai.lookup.enabled;
        *enabled = !*enabled;
        self.schedule_settings_save(cx);
        cx.notify();
    }

    pub(crate) fn begin_add_provider(&mut self, cx: &mut Context<Self>) {
        self.ai_ui.adding = true;
        cx.notify();
    }

    pub(crate) fn cancel_add_provider(&mut self, cx: &mut Context<Self>) {
        self.ai_ui.adding = false;
        cx.notify();
    }

    /// 选定类型后立刻建条目：类型决定字段形态（是否需要 base_url），先问再建能
    /// 避免用户填完才发现要换类型。
    pub(crate) fn add_provider_of_kind(&mut self, kind: ProviderKind, cx: &mut Context<Self>) {
        let id = self.settings.current.ai.add_provider(kind);
        self.ai_ui.adding = false;
        self.ai_ui.expanded = Some(id.clone());
        if self.settings.current.ai.lookup.provider_id.is_none() {
            self.settings.current.ai.lookup.provider_id = Some(id.clone());
        }
        self.ensure_provider_inputs(&id, cx);
        self.schedule_settings_save(cx);
        cx.notify();
    }

    pub(crate) fn toggle_provider_expanded(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.ai_ui.expanded.as_deref() == Some(id) {
            self.ai_ui.expanded = None;
        } else {
            self.ai_ui.expanded = Some(id.to_string());
            self.ensure_provider_inputs(id, cx);
            self.ensure_model_input(cx);
        }
        cx.notify();
    }

    pub(crate) fn remove_ai_provider(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.settings.current.ai.remove_provider(id) {
            return;
        }
        self.ai_ui
            .provider_inputs
            .retain(|(owner, _), _| owner != id);
        self.ai_ui.revealed.remove(id);
        if self.ai_ui.expanded.as_deref() == Some(id) {
            self.ai_ui.expanded = None;
        }
        self.schedule_settings_save(cx);
        cx.notify();
    }

    /// 类型循环切换：官方 → 兼容 → Anthropic → 官方。
    pub(crate) fn cycle_provider_kind(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(entry) = self
            .settings
            .current
            .ai
            .providers
            .iter_mut()
            .find(|entry| entry.id == id)
        else {
            return;
        };
        let previous_label = entry.kind.label();
        entry.kind = match entry.kind {
            ProviderKind::OpenAi => ProviderKind::OpenAiCompat,
            ProviderKind::OpenAiCompat => ProviderKind::Anthropic,
            ProviderKind::Anthropic => ProviderKind::OpenAi,
        };
        // 名字还是旧的类型名时跟着改，用户自定义过就不动。
        if entry.name == previous_label {
            entry.name = entry.kind.label().to_string();
        }
        if !entry.kind.needs_base_url() {
            entry.base_url = None;
        }
        let id = id.to_string();
        if let Some(input) = self
            .ai_ui
            .provider_inputs
            .get(&(id.clone(), ProviderField::BaseUrl))
            .cloned()
        {
            let value = self
                .settings
                .current
                .ai
                .provider(&id)
                .and_then(|entry| entry.base_url.clone())
                .unwrap_or_default();
            input.update(cx, |input, cx| input.set_value(value, cx));
        }
        self.sync_ai_inputs(cx);
        self.schedule_settings_save(cx);
        cx.notify();
    }

    pub(crate) fn toggle_key_reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.ai_ui.revealed.remove(id) {
            self.ai_ui.revealed.insert(id.to_string());
        }
        cx.notify();
    }

    /// 进入设置页时调用：确保模型输入框存在（它不依赖展开哪张卡）。
    pub(crate) fn enter_ai_settings(&mut self, cx: &mut Context<Self>) {
        self.ensure_model_input(cx);
        if let Some(id) = self.ai_ui.expanded.clone() {
            self.ensure_provider_inputs(&id, cx);
        }
    }

    /// 选为查词功能使用的供应商。
    pub(crate) fn select_ai_provider(&mut self, id: &str, cx: &mut Context<Self>) {
        self.settings.current.ai.lookup.provider_id = Some(id.to_string());
        self.ensure_model_input(cx);
        self.schedule_settings_save(cx);
        cx.notify();
    }

    /// 当前是否有任何 AI 设置输入框持有焦点。
    ///
    /// 设置页的全局按键处理要靠它放行字符输入，否则打字会被当成「聚焦主页搜索」。
    pub(crate) fn ai_input_has_focus(&self, window: &gpui::Window, cx: &gpui::App) -> bool {
        self.ai_ui
            .provider_inputs
            .values()
            .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
            || self
                .ai_ui
                .model_input
                .as_ref()
                .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
    }
}

#[cfg(test)]
mod tests {
    use ai_core::{AiSettings, ProviderKind};

    #[test]
    fn adding_two_providers_keeps_both_configs() {
        // 纯数据层校验：同名类型可以并存（多个中转站），互不覆盖。
        let mut ai = AiSettings::default();
        let first = ai.add_provider(ProviderKind::OpenAiCompat);
        let second = ai.add_provider(ProviderKind::OpenAiCompat);
        assert_ne!(first, second);
        ai.providers[0].base_url = Some("https://a.example/v1".to_string());
        ai.providers[1].base_url = Some("https://b.example/v1".to_string());
        assert_eq!(ai.providers.len(), 2);
        assert_eq!(
            ai.providers[0].base_url.as_deref(),
            Some("https://a.example/v1")
        );
        assert_eq!(
            ai.providers[1].base_url.as_deref(),
            Some("https://b.example/v1")
        );
    }
}
