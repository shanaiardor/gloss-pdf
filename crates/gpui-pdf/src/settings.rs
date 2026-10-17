//! 用户设置：主题与词典路径，持久化在用户数据目录的 `settings.json`。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::home::user_data_dir;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// 上次使用的主题。
    #[serde(default)]
    pub dark: bool,
    /// 用户选择的 `.mdx` 词典。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dictionary_path: Option<PathBuf>,
}

pub struct SettingsStore {
    pub current: Settings,
    /// `None` 表示不落盘（测试或无法确定用户目录）。
    storage: Option<PathBuf>,
}

impl SettingsStore {
    pub fn load() -> Self {
        let storage = user_data_dir().map(|directory| directory.join("settings.json"));
        let current = storage
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| parse_settings(&text))
            .unwrap_or_default();

        Self { current, storage }
    }

    pub fn save(&self) {
        let Some(path) = self.storage.as_deref() else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(parent) {
            log::warn!("无法创建设置目录 {}: {error}", parent.display());
            return;
        }
        match serde_json::to_string_pretty(&self.current) {
            Ok(json) => {
                if let Err(error) = std::fs::write(path, json) {
                    log::warn!("无法写入设置 {}: {error}", path.display());
                }
            }
            Err(error) => log::warn!("无法序列化设置: {error}"),
        }
    }
}

/// 读取设置；兼容 Windows 编辑器写的 UTF-8 BOM，坏文件静默降级。
fn parse_settings(text: &str) -> Settings {
    serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let settings = Settings {
            dark: true,
            dictionary_path: Some(PathBuf::from("C:/dicts/oxford.mdx")),
        };
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, settings);
    }

    #[test]
    fn legacy_settings_without_fields_still_parse() {
        let parsed: Settings = serde_json::from_str("{}").unwrap();
        assert!(!parsed.dark);
        assert!(parsed.dictionary_path.is_none());
    }

    #[test]
    fn utf8_bom_is_accepted() {
        let parsed = parse_settings("\u{feff}{\"dark\":true}");
        assert!(parsed.dark);
    }

    #[test]
    fn settings_without_storage_do_not_touch_disk() {
        let store = SettingsStore {
            current: Settings::default(),
            storage: None,
        };
        store.save();
    }

    #[test]
    fn save_writes_readable_json_and_creates_the_directory() {
        let directory = std::env::temp_dir().join(format!(
            "gpui-pdf-settings-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = directory.join("settings.json");
        let store = SettingsStore {
            current: Settings {
                dark: true,
                dictionary_path: Some(PathBuf::from("C:/dicts/oxford.mdx")),
            },
            storage: Some(path.clone()),
        };

        store.save();

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, store.current);

        std::fs::remove_dir_all(directory).ok();
    }
}
