//! 按配置构造 rig 模型（一句话查询用的完成模型）。
//!
//! 这里是 `rig` 类型唯一允许越过 crate 边界的出口：外部只拿到 `DynModel`
//! （模型已擦除成统一类型），升级 rig 时改动被限制在本文件。

use rig::DynModel;
use rig::operation::Completion;
use rig::providers::{anthropic, openai};

use crate::settings::{AiSettings, ConfigError, ProviderKind};

/// 构造一次调用用的完成模型。
///
/// 每次现建而不缓存：客户端构造是纯内存操作，缓存反而要维护「配置变了要失效」
/// 的一致性逻辑，得不偿失。
pub fn completion_model(settings: &AiSettings) -> Result<DynModel<Completion>, ConfigError> {
    settings.validate()?;
    let provider = settings
        .active_provider()
        .ok_or(ConfigError::MissingProvider)?;
    let model = settings.lookup.model.trim();

    match provider.kind {
        // 官方 OpenAI 走 Responses API（新接口，带服务端状态）。
        ProviderKind::OpenAi => Ok(openai::OpenAI::new(provider.api_key.trim())
            .responses(model)
            .into()),
        // 兼容端点只保证 Chat Completions，不能假设它实现了 /responses。
        ProviderKind::OpenAiCompat => {
            let base_url = provider
                .resolved_base_url()
                .ok_or(ConfigError::MissingBaseUrl)?;
            let client = openai::OpenAIConfig::new(provider.api_key.trim())
                .with_base_url(base_url)
                .client();
            Ok(client.chat(model).into())
        }
        ProviderKind::Anthropic => {
            let mut config = anthropic::AnthropicConfig::new(provider.api_key.trim());
            if let Some(base_url) = provider.resolved_base_url() {
                config = config.with_base_url(base_url);
            }
            Ok(config.client().completion(model).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ProviderEntry;

    fn settings_with(kind: ProviderKind, key: &str, base_url: Option<&str>) -> AiSettings {
        AiSettings {
            providers: vec![ProviderEntry {
                id: "p1".to_string(),
                name: kind.label().to_string(),
                kind,
                api_key: key.to_string(),
                base_url: base_url.map(str::to_string),
            }],
            lookup: crate::settings::LookupFeature {
                provider_id: Some("p1".to_string()),
                model: "some-model".to_string(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn unconfigured_settings_are_rejected_before_any_network_call() {
        let empty = AiSettings::default();
        assert_eq!(
            completion_model(&empty).unwrap_err(),
            ConfigError::MissingProvider
        );
    }

    #[test]
    fn compat_provider_without_base_url_is_rejected() {
        let settings = settings_with(ProviderKind::OpenAiCompat, "test-key-1234", None);
        assert_eq!(
            completion_model(&settings).unwrap_err(),
            ConfigError::MissingBaseUrl
        );
    }

    #[test]
    fn building_a_model_touches_no_network() {
        // 只验构造：离线环境下也应该成功（真正的请求发生在调用时）。
        let settings = settings_with(ProviderKind::OpenAi, "test-key-1234", None);
        assert!(completion_model(&settings).is_ok());

        let settings = settings_with(ProviderKind::Anthropic, "test-key-1234", None);
        assert!(completion_model(&settings).is_ok());

        let settings = settings_with(
            ProviderKind::OpenAiCompat,
            "test-key-1234",
            Some("https://gateway.example/v1"),
        );
        assert!(completion_model(&settings).is_ok());
    }
}
