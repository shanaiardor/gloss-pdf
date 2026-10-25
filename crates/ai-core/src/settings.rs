//! AI 配置：供应商条目 + 功能级配置。
//!
//! 分两层是刻意的：供应商（端点 + key）与功能解耦，同一个供应商可以被查词、
//! 以后的对话等复用；功能只持有 `provider_id` 引用和模型名。
//!
//! 模型放在功能层而不是供应商层，是因为同一家供应商在不同功能下的诉求不同
//! （查典故要知识密度高的模型，对话要快而长上下文），放在供应商层会被迫复制条目。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 单次查询允许模型额外读取的页数上限（含当前页以外的页）。
pub const DEFAULT_MAX_PAGES: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// 官方 OpenAI，走 Responses API。
    OpenAi,
    /// OpenAI 兼容端点（中转站、自建网关），走 Chat Completions。
    ///
    /// `compat` 是旧写法，保留别名避免早期配置读不回来。
    #[serde(alias = "compat")]
    OpenAiCompat,
    /// Anthropic Messages API。
    Anthropic,
}

impl ProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::OpenAiCompat => "兼容端点",
            Self::Anthropic => "Anthropic",
        }
    }

    /// 新增供应商时给模型输入框的提示值；真正的模型名由用户在功能层决定。
    pub fn default_model(self) -> &'static str {
        match self {
            Self::OpenAi => "gpt-5.5",
            Self::OpenAiCompat => "gpt-5.5",
            Self::Anthropic => "claude-sonnet-4-6",
        }
    }

    /// 官方端点自带 URL，兼容端点必须由用户填写。
    pub fn needs_base_url(self) -> bool {
        matches!(self, Self::OpenAiCompat)
    }
}

/// 一个供应商条目：连接信息，不含任何功能相关设置。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub kind: ProviderKind,
    /// 密钥明文存盘（见 `settings.rs` 的存储说明）；空串表示还没填。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// 仅兼容端点和自定义网关需要；`None` 表示用官方默认地址。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl ProviderEntry {
    pub fn new(id: impl Into<String>, kind: ProviderKind) -> Self {
        Self {
            id: id.into(),
            name: kind.label().to_string(),
            kind,
            api_key: String::new(),
            base_url: None,
        }
    }

    /// 连接信息是否齐全（不看功能层是否启用）。
    pub fn is_configured(&self) -> bool {
        !self.api_key.trim().is_empty()
            && (!self.kind.needs_base_url() || self.resolved_base_url().is_some())
    }

    /// 去掉空白后的 base_url；空串等同于没有填。
    pub fn resolved_base_url(&self) -> Option<&str> {
        self.base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
    }

    /// 展示用的密钥掩码：只露前 4 后 4，避免设置页与日志泄漏完整 key。
    pub fn masked_key(&self) -> String {
        mask_secret(&self.api_key)
    }
}

impl fmt::Debug for ProviderEntry {
    /// 手写 `Debug`：任何 `{:?}` 都不得打印完整密钥。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderEntry")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("api_key", &self.masked_key())
            .field("base_url", &self.base_url)
            .finish()
    }
}

/// 查词功能自身的配置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupFeature {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 引用的供应商条目 id；`None` 表示还没选。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// 模型名；必填，不做跨层继承。
    #[serde(default)]
    pub model: String,
    /// 是否允许模型读取当前页以外的页（受 `max_pages` 约束）。
    #[serde(default = "default_true")]
    pub read_page: bool,
    #[serde(default = "default_max_pages")]
    pub max_pages: u32,
}

impl Default for LookupFeature {
    fn default() -> Self {
        Self {
            enabled: true,
            provider_id: None,
            model: String::new(),
            read_page: true,
            max_pages: DEFAULT_MAX_PAGES,
        }
    }
}

/// 落盘的 AI 配置整体。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSettings {
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
    #[serde(default)]
    pub lookup: LookupFeature,
}

impl AiSettings {
    pub fn provider(&self, id: &str) -> Option<&ProviderEntry> {
        self.providers.iter().find(|entry| entry.id == id)
    }

    pub fn active_provider(&self) -> Option<&ProviderEntry> {
        self.lookup
            .provider_id
            .as_deref()
            .and_then(|id| self.provider(id))
    }

    /// 是否具备发起一次查询的全部条件；设置页据此显示「已就绪/待配置」。
    pub fn is_ready(&self) -> bool {
        self.lookup.enabled
            && !self.lookup.model.trim().is_empty()
            && self.active_provider().is_some_and(|entry| entry.is_configured())
    }

    /// 起一次查询前的严格校验；错误分类要能直接映射成设置页的提示语。
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.lookup.enabled {
            return Ok(());
        }
        if self.lookup.max_pages == 0 {
            return Err(ConfigError::InvalidMaxPages);
        }
        let provider = self
            .active_provider()
            .ok_or(ConfigError::MissingProvider)?;
        if provider.api_key.trim().is_empty() {
            return Err(ConfigError::EmptyApiKey);
        }
        if provider.kind.needs_base_url() && provider.resolved_base_url().is_none() {
            return Err(ConfigError::MissingBaseUrl);
        }
        if self.lookup.model.trim().is_empty() {
            return Err(ConfigError::EmptyModel);
        }
        Ok(())
    }

    /// 生成下一个可用 id（`p1`、`p2`…）；纯字符串运算，不引 uuid 依赖。
    pub fn next_provider_id(&self) -> String {
        let used: Vec<u64> = self
            .providers
            .iter()
            .filter_map(|entry| entry.id.strip_prefix('p')?.parse().ok())
            .collect();
        let next = used.into_iter().max().unwrap_or(0) + 1;
        format!("p{next}")
    }

    /// 追加一个供应商条目并返回其 id；不自动选中，选择权留给调用方。
    pub fn add_provider(&mut self, kind: ProviderKind) -> String {
        let id = self.next_provider_id();
        self.providers.push(ProviderEntry::new(id.clone(), kind));
        id
    }

    /// 删除供应商；若功能正引用它，一并清空引用，避免留下悬空 id。
    pub fn remove_provider(&mut self, id: &str) -> bool {
        let before = self.providers.len();
        self.providers.retain(|entry| entry.id != id);
        if self.providers.len() == before {
            return false;
        }
        if self.lookup.provider_id.as_deref() == Some(id) {
            self.lookup.provider_id = None;
        }
        true
    }
}

/// 配置类错误。刻意与网络/协议错误分开：这些是「用户还没填好」，不该重试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// 功能已启用但没选供应商。
    MissingProvider,
    /// 引用的供应商条目已被删除。
    ProviderNotFound,
    EmptyApiKey,
    EmptyModel,
    MissingBaseUrl,
    InvalidMaxPages,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::MissingProvider => "还没有选择供应商",
            Self::ProviderNotFound => "选择的供应商已不存在",
            Self::EmptyApiKey => "供应商还没有填写 API Key",
            Self::EmptyModel => "还没有填写模型名",
            Self::MissingBaseUrl => "兼容端点需要填写 Base URL",
            Self::InvalidMaxPages => "页数上限必须大于 0",
        };
        f.write_str(text)
    }
}

impl std::error::Error for ConfigError {}

/// 密钥掩码：短于 8 位时只露首字符，避免「掩码即泄漏」。
pub fn mask_secret(secret: &str) -> String {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() < 8 {
        let head: String = chars.iter().take(1).collect();
        return format!("{head}…");
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}…{tail}")
}

fn default_true() -> bool {
    true
}

fn default_max_pages() -> u32 {
    DEFAULT_MAX_PAGES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(kind: ProviderKind, key: &str, base_url: Option<&str>) -> ProviderEntry {
        ProviderEntry {
            id: "p1".to_string(),
            name: kind.label().to_string(),
            kind,
            api_key: key.to_string(),
            base_url: base_url.map(str::to_string),
        }
    }

    #[test]
    fn defaults_are_sane_and_old_config_parses() {
        let parsed: AiSettings = serde_json::from_str("{}").unwrap();
        assert!(parsed.providers.is_empty());
        assert!(parsed.lookup.enabled, "默认视为启用，用户配好即可用");
        assert!(parsed.lookup.read_page);
        assert_eq!(parsed.lookup.max_pages, DEFAULT_MAX_PAGES);
        assert!(parsed.lookup.model.is_empty());
        assert!(!parsed.is_ready());
    }

    #[test]
    fn round_trips_through_json() {
        let mut settings = AiSettings::default();
        let id = settings.add_provider(ProviderKind::OpenAi);
        settings.providers[0].api_key = "test-key-1234567890".to_string();
        settings.lookup.provider_id = Some(id);
        settings.lookup.model = "gpt-5.5".to_string();

        let json = serde_json::to_string(&settings).unwrap();
        let parsed: AiSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, settings);
        assert!(parsed.is_ready());
    }

    #[test]
    fn provider_kind_accepts_legacy_compat_spelling() {
        let parsed: AiSettings = serde_json::from_str(
            r#"{"providers":[{"id":"p1","kind":"compat","api_key":"k","base_url":"https://x"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.providers[0].kind, ProviderKind::OpenAiCompat);
    }

    #[test]
    fn debug_output_never_leaks_the_key() {
        let entry = configured(ProviderKind::OpenAi, "sk-super-secret-abcdef", None);
        let debug = format!("{entry:?}");
        assert!(!debug.contains("super-secret"), "Debug 必须脱敏：{debug}");
        assert!(debug.contains('…'), "应显示掩码而不是空串：{debug}");
    }

    #[test]
    fn mask_keeps_short_secrets_unreadable() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("   "), "");
        assert_eq!(mask_secret("abc"), "a…");
        assert_eq!(mask_secret("sk-1234567890"), "sk-1…7890");
    }

    #[test]
    fn validation_reports_the_first_missing_thing() {
        let mut settings = AiSettings::default();
        assert_eq!(settings.validate(), Err(ConfigError::MissingProvider));

        let id = settings.add_provider(ProviderKind::Anthropic);
        settings.lookup.provider_id = Some(id.clone());
        assert_eq!(settings.validate(), Err(ConfigError::EmptyApiKey));

        settings.providers[0].api_key = "test-key-1234567890".to_string();
        assert_eq!(settings.validate(), Err(ConfigError::EmptyModel));

        settings.lookup.model = "claude-sonnet-4-6".to_string();
        assert_eq!(settings.validate(), Ok(()));
        assert!(settings.is_ready());
    }

    #[test]
    fn compat_provider_requires_a_base_url() {
        let mut settings = AiSettings {
            providers: vec![configured(ProviderKind::OpenAiCompat, "sk-x-123456", None)],
            ..AiSettings::default()
        };
        settings.lookup.provider_id = Some("p1".to_string());
        settings.lookup.model = "gpt-5.5".to_string();
        assert_eq!(settings.validate(), Err(ConfigError::MissingBaseUrl));

        settings.providers[0].base_url = Some("https://gateway.example/v1".to_string());
        assert_eq!(settings.validate(), Ok(()));
        assert!(settings.is_ready());
    }

    #[test]
    fn zero_max_pages_is_rejected() {
        let mut settings = AiSettings {
            providers: vec![configured(ProviderKind::OpenAi, "sk-x-123456", None)],
            ..AiSettings::default()
        };
        settings.lookup.provider_id = Some("p1".to_string());
        settings.lookup.model = "gpt-5.5".to_string();
        settings.lookup.max_pages = 0;
        assert_eq!(settings.validate(), Err(ConfigError::InvalidMaxPages));
    }

    #[test]
    fn disabled_feature_validates_even_when_unconfigured() {
        let mut settings = AiSettings::default();
        settings.lookup.enabled = false;
        assert_eq!(settings.validate(), Ok(()));
        assert!(!settings.is_ready(), "没配好时即使关掉也不算就绪");
    }

    #[test]
    fn removing_the_active_provider_clears_the_reference() {
        let mut settings = AiSettings::default();
        let id = settings.add_provider(ProviderKind::OpenAi);
        settings.lookup.provider_id = Some(id.clone());

        assert!(settings.remove_provider(&id));
        assert_eq!(settings.lookup.provider_id, None);
        assert_eq!(settings.active_provider(), None);
        assert!(!settings.remove_provider(&id), "重复删除应返回 false");
    }

    #[test]
    fn provider_ids_do_not_reuse_gaps() {
        let mut settings = AiSettings::default();
        assert_eq!(settings.add_provider(ProviderKind::OpenAi), "p1");
        assert_eq!(settings.add_provider(ProviderKind::Anthropic), "p2");
        settings.remove_provider("p1");
        assert_eq!(
            settings.add_provider(ProviderKind::OpenAi),
            "p3",
            "删掉中间条目后不能复用旧 id，否则旧配置里的引用会指向新条目"
        );
    }
}
