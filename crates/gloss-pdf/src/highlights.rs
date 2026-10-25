//! 高亮与页边笺注：内存模型 + 与 PDF 同名的 sidecar JSON 持久化。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pdf_core::RectPt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

/// 钉在页面右侧笺注栏里的一条永久词典笺注。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PinnedGloss {
    #[serde(default)]
    pub id: u64,
    pub page: usize,
    /// 原文被选词在页面上的 PDF 坐标框（pt，原点左下角）；手动输入查词钉住时可为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rects: Vec<RectPt>,
    /// 锚点距离页顶的纵向偏移（PDF pt，向下为正），用来把笺注卡对齐到原词所在行。
    #[serde(default)]
    pub anchor_top: f32,
    /// 词头。
    pub word: String,
    /// 音标（若有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phonetic: Option<String>,
    /// 精简释义摘要，供页边笺注卡常驻展示与离线回看。
    #[serde(default)]
    pub summary: String,
    /// 这条笺注是 AI 回答钉下来的（没有词典条目）。
    ///
    /// 点开时必须走 AI 卡、直接显示已存的答案，而不是再查一遍词典
    /// —— 词典查不到会再触发一次 AI 查询，等于重复计费。
    #[serde(default, skip_serializing_if = "is_false")]
    pub from_ai: bool,
    /// AI 回答的完整正文；`summary` 只保留前 200 字给紧凑卡。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answer: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SidecarData {
    Legacy(Vec<Highlight>),
    Unified {
        #[serde(default)]
        highlights: Vec<Highlight>,
        #[serde(default)]
        glosses: Vec<PinnedGloss>,
    },
}

#[derive(Default)]
pub struct HighlightStore {
    pub items: Vec<Highlight>,
    pub glosses: Vec<PinnedGloss>,
    next_id: u64,
}

impl HighlightStore {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.glosses.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len() + self.glosses.len()
    }

    #[allow(dead_code)]
    pub fn has_glosses(&self) -> bool {
        !self.glosses.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.glosses.clear();
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

    /// 钉住一条页边笺注；同页同词已存在时原地更新摘要与位置，避免重复堆叠。
    pub fn add_gloss(
        &mut self,
        page: usize,
        rects: Vec<RectPt>,
        anchor_top: f32,
        word: String,
        phonetic: Option<String>,
        summary: String,
        from_ai: bool,
        answer: String,
    ) -> Option<u64> {
        let word = word.trim().to_string();
        if word.is_empty() {
            return None;
        }
        let anchor_top = if anchor_top.is_finite() && anchor_top >= 0.0 {
            anchor_top
        } else {
            0.0
        };
        if let Some(existing) = self
            .glosses
            .iter_mut()
            .find(|item| item.page == page && item.word.eq_ignore_ascii_case(&word))
        {
            if !rects.is_empty() {
                existing.rects = rects;
                existing.anchor_top = anchor_top;
            }
            existing.phonetic = phonetic;
            existing.summary = summary;
            existing.from_ai = from_ai;
            existing.answer = answer;
            return Some(existing.id);
        }

        let id = self.take_id();
        self.glosses.push(PinnedGloss {
            id,
            page,
            rects,
            anchor_top,
            word,
            phonetic,
            summary,
            from_ai,
            answer,
        });
        Some(id)
    }

    /// 查找某一页上是否已经钉住了某个词。
    pub fn find_gloss(&self, page: usize, word: &str) -> Option<&PinnedGloss> {
        let word = word.trim();
        if word.is_empty() {
            return None;
        }
        self.glosses
            .iter()
            .find(|item| item.page == page && item.word.eq_ignore_ascii_case(word))
    }

    /// 按 id 删除高亮或笺注；返回是否删掉了东西。
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.len();
        self.items.retain(|item| item.id != id);
        self.glosses.retain(|item| item.id != id);
        self.len() != before
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

    pub fn glosses_for_page(&self, page: usize) -> impl Iterator<Item = &PinnedGloss> {
        self.glosses
            .iter()
            .filter(move |gloss| gloss.page == page)
    }

    /// 按页码排序后的高亮副本，批注侧栏用。
    pub fn sorted(&self) -> Vec<Highlight> {
        let mut items = self.items.clone();
        items.sort_by_key(|item| (item.page, item.id));
        items
    }

    /// 按页码与页内纵向位置排序后的笺注副本。
    pub fn sorted_glosses(&self) -> Vec<PinnedGloss> {
        let mut glosses = self.glosses.clone();
        glosses.sort_by(|a, b| {
            a.page
                .cmp(&b.page)
                .then_with(|| a.anchor_top.total_cmp(&b.anchor_top))
                .then_with(|| a.id.cmp(&b.id))
        });
        glosses
    }

    /// 读取 sidecar；文件不存在不是错误。
    ///
    /// 同时兼容旧版纯数组 `[Highlight, ...]` 与新版 `{"highlights": [...], "glosses": [...]}`。
    pub fn load(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let parsed = serde_json::from_str::<SidecarData>(&text);
        match parsed {
            Ok(data) => {
                let (mut items, mut glosses) = match data {
                    SidecarData::Legacy(items) => (items, Vec::new()),
                    SidecarData::Unified {
                        highlights,
                        glosses,
                    } => (highlights, glosses),
                };
                let mut next = 1;
                let mut seen = std::collections::HashSet::new();
                for item in &mut items {
                    if item.id == 0 || !seen.insert(item.id) {
                        item.id = next;
                    }
                    seen.insert(item.id);
                    next = next.max(item.id + 1);
                }
                for gloss in &mut glosses {
                    if gloss.id == 0 || !seen.insert(gloss.id) {
                        gloss.id = next;
                    }
                    seen.insert(gloss.id);
                    next = next.max(gloss.id + 1);
                }
                self.items = items;
                self.glosses = glosses;
                self.next_id = next;
                log::info!(
                    "loaded {} highlights and {} glosses from {}",
                    self.items.len(),
                    self.glosses.len(),
                    path.display()
                );
            }
            Err(error) => log::warn!("failed to parse {}: {error}", path.display()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = if self.glosses.is_empty() {
            serde_json::to_string_pretty(&self.items)?
        } else {
            serde_json::to_string_pretty(&SidecarData::Unified {
                highlights: self.items.clone(),
                glosses: self.glosses.clone(),
            })?
        };
        std::fs::write(path, json).with_context(|| format!("无法写入 {}", path.display()))?;
        log::info!(
            "saved {} highlights and {} glosses to {}",
            self.items.len(),
            self.glosses.len(),
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

    #[test]
    fn ai_gloss_keeps_the_full_answer_and_its_source_flag() {
        let mut store = HighlightStore::default();
        let answer = "这是一段很长的 AI 解释。".repeat(20);
        let summary: String = answer.chars().take(200).collect();
        store
            .add_gloss(
                3,
                one_rect(),
                60.0,
                "伯牙".into(),
                None,
                summary.clone(),
                true,
                answer.clone(),
            )
            .unwrap();

        let gloss = store.find_gloss(3, "伯牙").expect("应能找到");
        assert!(gloss.from_ai, "点开时要靠这个标记走 AI 卡，而不是重查词典");
        assert_eq!(gloss.answer, answer, "完整正文要留下来供再次打开");
        assert_eq!(gloss.summary, summary, "紧凑卡仍用截断后的摘要");

        // 旧 sidecar 没有这两个字段，读回来应是「非 AI 来源、无正文」。
        let legacy: PinnedGloss = serde_json::from_str(
            r#"{"id":1,"page":0,"anchor_top":0.0,"word":"舊","summary":"旧摘要"}"#,
        )
        .unwrap();
        assert!(!legacy.from_ai);
        assert!(legacy.answer.is_empty());
    }

    #[test]
    fn add_gloss_deduplicates_same_word_on_same_page_and_persists() {
        let mut store = HighlightStore::default();
        store.add(0, one_rect(), "原文高亮".into());
        let id1 = store
            .add_gloss(
                0,
                one_rect(),
                120.0,
                "Gloss".into(),
                Some("/ɡlɒs/".into()),
                "页边批注".into(),
                false,
                String::new(),
            )
            .unwrap();
        let id2 = store
            .add_gloss(
                0,
                one_rect(),
                140.0,
                "gloss".into(),
                Some("/ɡlɑːs/".into()),
                "更新后的释义".into(),
                false,
                String::new(),
            )
            .unwrap();
        assert_eq!(id1, id2);
        assert_eq!(store.glosses.len(), 1);
        assert_eq!(store.find_gloss(0, "GLOSS").unwrap().summary, "更新后的释义");

        let path = std::env::temp_dir().join(format!(
            "gloss-pdf-unified-sidecar-test-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        store.save(&path).unwrap();

        let mut loaded = HighlightStore::default();
        loaded.load(&path);
        std::fs::remove_file(&path).ok();

        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.glosses.len(), 1);
        assert_eq!(loaded.glosses[0].word, "Gloss");
        assert_eq!(loaded.glosses[0].summary, "更新后的释义");
    }
}
