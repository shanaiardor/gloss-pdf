//! 高亮批注：内存模型 + 与 PDF 同名的 sidecar JSON 持久化。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pdf_core::RectPt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Highlight {
    /// 稳定标识，批注列表里删除时用；旧文件没有这个字段，加载时补上。
    #[serde(default)]
    pub id: u64,
    pub page: usize,
    pub rects: Vec<RectPt>,
    /// 高亮时的选中文字，列表里显示用；旧文件没有这个字段。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

#[derive(Default)]
pub struct HighlightStore {
    pub items: Vec<Highlight>,
    next_id: u64,
}

impl HighlightStore {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[allow(dead_code)] // 测试与调试用；界面直接读 items.len()
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.next_id = 1;
    }

    /// 加一条高亮，返回它的 id。
    pub fn add(&mut self, page: usize, rects: Vec<RectPt>, text: String) -> Option<u64> {
        if rects.is_empty() {
            return None;
        }
        let id = self.take_id();
        self.items.push(Highlight {
            id,
            page,
            rects,
            text,
        });
        Some(id)
    }

    /// 按 id 删除；返回是否删掉了东西。
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.id != id);
        self.items.len() != before
    }

    fn take_id(&mut self) -> u64 {
        if self.next_id == 0 {
            self.next_id = 1;
        }
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn for_page(&self, page: usize) -> impl Iterator<Item = &Highlight> {
        self.items
            .iter()
            .filter(move |highlight| highlight.page == page)
    }

    /// 按页码排序后的副本，批注侧栏用。
    pub fn sorted(&self) -> Vec<Highlight> {
        let mut items = self.items.clone();
        items.sort_by_key(|item| (item.page, item.id));
        items
    }

    /// 读取 sidecar；文件不存在不是错误。
    ///
    /// 旧版本的高亮没有 `id`，这里统一补成递增 id，避免删除时误删同页其他条目。
    pub fn load(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        match serde_json::from_str::<Vec<Highlight>>(&text) {
            Ok(mut items) => {
                let mut next = 1;
                let mut seen = std::collections::HashSet::new();
                for item in &mut items {
                    if item.id == 0 || !seen.insert(item.id) {
                        item.id = next;
                    }
                    seen.insert(item.id);
                    next = next.max(item.id + 1);
                }
                self.items = items;
                self.next_id = next;
                log::info!(
                    "loaded {} highlights from {}",
                    self.items.len(),
                    path.display()
                );
            }
            Err(error) => log::warn!("failed to parse {}: {error}", path.display()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.items)?;
        std::fs::write(path, json).with_context(|| format!("无法写入 {}", path.display()))?;
        log::info!(
            "saved {} highlights to {}",
            self.items.len(),
            path.display()
        );
        Ok(())
    }
}

/// `<pdf 文件名>.highlights.json`，与 PDF 放在同一目录。
pub fn sidecar_path(pdf: &Path) -> PathBuf {
    let name = pdf
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "document.pdf".to_string());
    pdf.with_file_name(format!("{name}.highlights.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_rect() -> Vec<RectPt> {
        vec![RectPt {
            left: 1.0,
            bottom: 2.0,
            right: 3.0,
            top: 4.0,
        }]
    }

    #[test]
    fn add_assigns_increasing_ids() {
        let mut store = HighlightStore::default();
        assert_eq!(store.add(0, one_rect(), "一".into()), Some(1));
        assert_eq!(store.add(0, one_rect(), "二".into()), Some(2));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn empty_rects_are_rejected() {
        let mut store = HighlightStore::default();
        assert_eq!(store.add(0, Vec::new(), String::new()), None);
        assert!(store.is_empty());
    }

    #[test]
    fn remove_only_drops_the_matching_id() {
        let mut store = HighlightStore::default();
        store.add(1, one_rect(), "一".into());
        store.add(1, one_rect(), "二".into());

        assert!(store.remove(1));
        assert!(!store.remove(1));
        assert_eq!(store.len(), 1);
        assert_eq!(store.items[0].text, "二");
    }

    #[test]
    fn sorted_orders_by_page_then_id() {
        let mut store = HighlightStore::default();
        store.add(5, one_rect(), "后".into());
        store.add(1, one_rect(), "前".into());

        let pages: Vec<usize> = store.sorted().iter().map(|item| item.page).collect();
        assert_eq!(pages, vec![1, 5]);
    }

    #[test]
    fn loading_legacy_json_fills_missing_ids() {
        // 旧版本只写 page / rects，没有 id 和 text。
        let json = r#"[
            {"page":0,"rects":[{"left":1.0,"bottom":2.0,"right":3.0,"top":4.0}]},
            {"page":0,"rects":[{"left":1.0,"bottom":2.0,"right":3.0,"top":4.0}]}
        ]"#;
        let path = std::env::temp_dir().join(format!(
            "gloss-pdf-highlights-test-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, json).unwrap();

        let mut store = HighlightStore::default();
        store.load(&path);
        std::fs::remove_file(&path).ok();

        assert_eq!(store.len(), 2);
        let ids: Vec<u64> = store.items.iter().map(|item| item.id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(store.add(1, one_rect(), "新".into()), Some(3));
    }
}
