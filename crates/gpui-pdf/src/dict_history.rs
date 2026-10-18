//! 查词历史：最近 50 个词，倒序、去重，落盘到用户数据目录。
//!
//! 历史只保存查询词本身，不保存释义；文件损坏时静默从空历史开始，和设置/
//! 最近文档的容错策略一致。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::home::user_data_dir;

/// 最多保留多少个历史词。
pub const MAX_HISTORY: usize = 50;
const FILE_NAME: &str = "dictionary_history.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct HistoryFile {
    #[serde(default)]
    entries: Vec<String>,
}

pub struct DictHistory {
    entries: Vec<String>,
    storage: Option<PathBuf>,
}

impl DictHistory {
    pub fn load() -> Self {
        let storage = user_data_dir().map(|directory| directory.join(FILE_NAME));
        let entries = storage
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| parse_history(&text))
            .unwrap_or_default();
        Self { entries, storage }
    }

    #[cfg(test)]
    fn load_from(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .map(|text| parse_history(&text))
            .unwrap_or_default();
        Self {
            entries,
            storage: Some(path),
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// 记录一次查询：同一词（忽略大小写）移到最前，超出上限截断，立即写盘。
    pub fn record(&mut self, term: &str) {
        let term = term.trim();
        if term.is_empty() {
            return;
        }
        let key = term.to_lowercase();
        self.entries
            .retain(|entry| entry.to_lowercase() != key);
        self.entries.insert(0, term.to_string());
        self.entries.truncate(MAX_HISTORY);
        self.save();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.save();
    }

    fn save(&self) {
        let Some(path) = self.storage.as_deref() else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(parent) {
            log::warn!("无法创建历史目录 {}: {error}", parent.display());
            return;
        }
        let file = HistoryFile {
            entries: self.entries.clone(),
        };
        match serde_json::to_string_pretty(&file) {
            Ok(json) => {
                if let Err(error) = std::fs::write(path, json) {
                    log::warn!("无法写入查词历史 {}: {error}", path.display());
                }
            }
            Err(error) => log::warn!("无法序列化查词历史: {error}"),
        }
    }
}

fn parse_history(text: &str) -> Vec<String> {
    let text = text.trim_start_matches('\u{feff}');
    let entries = match serde_json::from_str::<HistoryFile>(text) {
        Ok(file) => file.entries,
        Err(_) => serde_json::from_str::<Vec<String>>(text).unwrap_or_default(),
    };
    let mut seen = std::collections::HashSet::new();
    let mut cleaned = Vec::new();
    for entry in entries {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if !seen.insert(entry.to_lowercase()) {
            continue;
        }
        cleaned.push(entry.to_string());
        if cleaned.len() == MAX_HISTORY {
            break;
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "gpui-pdf-history-{name}-{}-{stamp}",
                std::process::id()
            ))
            .join(FILE_NAME)
    }

    #[test]
    fn records_newest_first_and_deduplicates_ignoring_case() {
        let path = temp_path("dedupe");
        let mut history = DictHistory::load_from(path.clone());
        history.record("Apple");
        history.record("banana");
        history.record("apple");
        assert_eq!(history.entries(), ["apple", "banana"]);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn caps_at_fifty_entries() {
        let path = temp_path("cap");
        let mut history = DictHistory::load_from(path.clone());
        for index in 0..60 {
            history.record(&format!("word-{index}"));
        }
        assert_eq!(history.entries().len(), MAX_HISTORY);
        assert_eq!(history.entries()[0], "word-59");
        assert_eq!(history.entries()[MAX_HISTORY - 1], "word-10");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn clear_empties_memory_and_disk() {
        let path = temp_path("clear");
        let mut history = DictHistory::load_from(path.clone());
        history.record("alpha");
        history.record("beta");
        assert!(path.is_file());
        history.clear();
        assert!(history.entries().is_empty());
        let reloaded = DictHistory::load_from(path.clone());
        assert!(reloaded.entries().is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn history_survives_restart_in_order() {
        let path = temp_path("restart");
        let mut history = DictHistory::load_from(path.clone());
        history.record("one");
        history.record("two");
        history.record("three");

        let reloaded = DictHistory::load_from(path.clone());
        assert_eq!(reloaded.entries(), ["three", "two", "one"]);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn corrupt_or_legacy_files_fall_back_gracefully() {
        assert_eq!(parse_history("not json"), Vec::<String>::new());
        assert_eq!(
            parse_history(r#"["a","A","","b"]"#),
            vec!["a".to_string(), "b".to_string()]
        );
        let long = (0..60)
            .map(|index| format!("w{index}"))
            .collect::<Vec<_>>();
        assert_eq!(
            parse_history(&serde_json::to_string(&long).unwrap()).len(),
            MAX_HISTORY
        );
    }

    #[test]
    fn missing_directory_is_created_on_save() {
        let path = temp_path("mkdir");
        let mut history = DictHistory::load_from(path.clone());
        history.record("gamma");
        assert!(path.is_file());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
