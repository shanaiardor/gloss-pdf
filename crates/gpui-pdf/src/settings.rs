//! 用户设置：主题与词典路径，持久化在用户数据目录的 `settings.json`。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::home::user_data_dir;

/// 设置里的一本词典：路径 + 是否参与查询。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionarySetting {
    pub path: PathBuf,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// 窗口左上角在全局屏幕坐标中的位置（逻辑像素）。
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// 退出时是否处于最大化；`true` 时上面四个值是“还原后”的窗口框。
    #[serde(default)]
    pub maximized: bool,
}

impl WindowState {
    pub fn is_finite(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// 上次使用的主题。
    #[serde(default)]
    pub dark: bool,
    /// 旧版单词典字段；仅用于向后兼容，新代码以 `dictionaries` 为准。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dictionary_path: Option<PathBuf>,
    /// 用户添加的词典，顺序就是查询顺序。
    #[serde(default)]
    pub dictionaries: Vec<DictionarySetting>,
    /// 启动后自动检查 GitHub 更新；默认开启。
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
    /// 上次使用的窗口尺寸与位置；`None` 表示首次启动，按默认尺寸居中。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowState>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dark: false,
            dictionary_path: None,
            dictionaries: Vec::new(),
            auto_check_updates: true,
            window: None,
        }
    }
}

impl Settings {
    /// 兼容旧配置：只有 `dictionary_path` 时迁移成单本启用词典；去重重复路径。
    pub fn migrate(&mut self) {
        if self.dictionaries.is_empty()
            && let Some(path) = self.dictionary_path.take()
        {
            self.dictionaries.push(DictionarySetting {
                path,
                enabled: true,
            });
        }

        let mut seen = std::collections::HashSet::new();
        self.dictionaries
            .retain(|dictionary| seen.insert(dictionary.path.clone()));

        if let Some(path) = self
            .dictionaries
            .iter()
            .find(|dictionary| dictionary.enabled)
            .or_else(|| self.dictionaries.first())
            .map(|dictionary| dictionary.path.clone())
        {
            self.dictionary_path = Some(path);
        } else {
            self.dictionary_path = None;
        }
    }

    pub fn has_enabled_dictionary(&self) -> bool {
        self.dictionaries
            .iter()
            .any(|dictionary| dictionary.enabled)
    }
}

fn default_true() -> bool {
    true
}

pub struct SettingsStore {
    pub current: Settings,
    /// `None` 表示不落盘（测试或无法确定用户目录）。
    storage: Option<PathBuf>,
}

impl SettingsStore {
    pub fn load() -> Self {
        let storage = user_data_dir().map(|directory| directory.join("settings.json"));
        let mut current = storage
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| parse_settings(&text))
            .unwrap_or_default();
        current.migrate();

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
        let mut snapshot = self.current.clone();
        snapshot.migrate();
        match serde_json::to_string_pretty(&snapshot) {
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
            dictionaries: vec![DictionarySetting {
                path: PathBuf::from("C:/dicts/oxford.mdx"),
                enabled: true,
            }],
            auto_check_updates: false,
            window: Some(WindowState {
                x: 120.0,
                y: 80.0,
                width: 1280.0,
                height: 860.0,
                maximized: false,
            }),
        };
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, settings);
    }

    #[test]
    fn window_state_round_trips_and_defaults_to_none() {
        let text =
            r#"{"window":{"x":-1600.0,"y":40.0,"width":1000.0,"height":700.0,"maximized":true}}"#;
        let parsed = parse_settings(text);
        assert_eq!(
            parsed.window,
            Some(WindowState {
                x: -1600.0,
                y: 40.0,
                width: 1000.0,
                height: 700.0,
                maximized: true,
            }),
            "副屏（负坐标）与最大化状态都要能存回来"
        );

        // 没存过窗口几何（首次启动 / 旧配置）时是 `None`，也回退成默认居中。
        assert!(parse_settings("{}").window.is_none());

        // 旧配置没有 `maximized` 字段，按未最大化处理。
        let parsed = parse_settings(r#"{"window":{"x":0.0,"y":0.0,"width":800.0,"height":600.0}}"#);
        assert!(parsed.window.is_some_and(|window| !window.maximized));
    }

    #[test]
    fn non_finite_window_state_is_rejected() {
        assert!(
            !WindowState {
                x: f32::NAN,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                maximized: false,
            }
            .is_finite()
        );
        assert!(
            !WindowState {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 600.0,
                maximized: false,
            }
            .is_finite()
        );
        assert!(
            WindowState {
                x: -1920.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                maximized: false,
            }
            .is_finite()
        );
    }

    #[test]
    fn legacy_settings_without_fields_still_parse() {
        let parsed: Settings = serde_json::from_str("{}").unwrap();
        assert!(!parsed.dark);
        assert!(parsed.dictionary_path.is_none());
        assert!(parsed.dictionaries.is_empty());
        assert!(parsed.auto_check_updates, "旧设置默认开启自动检查");
    }

    #[test]
    fn legacy_dictionary_path_migrates_to_one_enabled_entry() {
        let mut settings: Settings =
            serde_json::from_str(r#"{"dictionary_path":"C:/dicts/oxford.mdx"}"#).unwrap();
        assert!(settings.dictionaries.is_empty());
        settings.migrate();
        assert_eq!(settings.dictionaries.len(), 1);
        assert_eq!(
            settings.dictionaries[0].path,
            PathBuf::from("C:/dicts/oxford.mdx")
        );
        assert!(settings.dictionaries[0].enabled);
        assert_eq!(
            settings.dictionary_path,
            Some(PathBuf::from("C:/dicts/oxford.mdx"))
        );
    }

    #[test]
    fn migration_keeps_order_enabled_flags_and_deduplicates() {
        let mut settings = Settings {
            dictionaries: vec![
                DictionarySetting {
                    path: PathBuf::from("C:/dicts/a.mdx"),
                    enabled: false,
                },
                DictionarySetting {
                    path: PathBuf::from("C:/dicts/b.mdx"),
                    enabled: true,
                },
                DictionarySetting {
                    path: PathBuf::from("C:/dicts/a.mdx"),
                    enabled: true,
                },
            ],
            ..Settings::default()
        };
        settings.migrate();
        assert_eq!(settings.dictionaries.len(), 2);
        assert_eq!(
            settings.dictionaries[0].path,
            PathBuf::from("C:/dicts/a.mdx")
        );
        assert!(!settings.dictionaries[0].enabled);
        assert_eq!(
            settings.dictionaries[1].path,
            PathBuf::from("C:/dicts/b.mdx")
        );
        assert!(settings.dictionaries[1].enabled);
        assert!(settings.has_enabled_dictionary());
    }

    #[test]
    fn no_enabled_dictionary_clears_legacy_path() {
        let mut settings = Settings {
            dictionaries: vec![DictionarySetting {
                path: PathBuf::from("C:/dicts/a.mdx"),
                enabled: false,
            }],
            ..Settings::default()
        };
        settings.migrate();
        assert!(!settings.has_enabled_dictionary());
        assert_eq!(
            settings.dictionary_path,
            Some(PathBuf::from("C:/dicts/a.mdx"))
        );
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
                dictionaries: vec![DictionarySetting {
                    path: PathBuf::from("C:/dicts/oxford.mdx"),
                    enabled: true,
                }],
                auto_check_updates: false,
                window: None,
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
