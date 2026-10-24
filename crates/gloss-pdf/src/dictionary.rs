//! 词典域：页边笺注的界面状态（当前词、页面锚点、加载状态、查询结果）与词条清洗/摘要。

use std::path::PathBuf;

use dict_core::DictEntry;
use pdf_core::RectPt;

/// 词条内超链接或交互发回主状态的请求。
#[derive(Clone, Debug)]
pub enum DictRequest {
    /// 词条内 `entry://` 链接跳转查词（保留当前页面锚点并压入返回栈）。
    Lookup(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DictLoadState {
    /// 还没有在设置里选择词典。
    #[default]
    Unconfigured,
    Loading,
    Ready {
        title: Option<String>,
        entries: u64,
    },
    Failed(String),
}

/// 一本已配置词典的加载状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictStatus {
    pub path: PathBuf,
    pub state: DictLoadState,
}

#[derive(Default)]
pub struct DictionaryState {
    /// 当前是否展开活动页边笺注卡。
    pub open: bool,
    /// 活动笺注卡所在的页码（0-based）。
    pub anchor_page: usize,
    /// 原文被选词在页面上的 PDF 坐标框（pt，原点左下角）。
    pub anchor_rects: Vec<RectPt>,
    /// 活动笺注卡距离该页顶部的纵向偏移（PDF pt，向下为正）。
    pub anchor_top: f32,
    /// 词条内点击 `entry://` 链接跳转时的历史词头栈，用于一键返回原词。
    pub back_stack: Vec<String>,
    /// 是否展开「最近查词」列表（默认收起以保持页边卡片紧凑）。
    pub show_history: bool,
    pub word: String,
    pub loading: bool,
    pub entries: Vec<DictEntry>,
    pub not_found: bool,
    pub error: Option<String>,
    pub load_state: DictLoadState,
    /// 与设置中的词典列表同序的逐本加载状态。
    pub statuses: Vec<DictStatus>,
    /// 词典配置请求代号，用来丢弃过期的 `Configured` 事件。
    pub configure_request: u64,
    /// 查询代次号，用来丢弃过期结果。
    pub request: u64,
    /// 联想代次号，用来丢弃防抖后过期的前缀请求。
    pub suggest_request: u64,
    /// 当前输入对应的前缀建议。
    pub suggestions: Vec<String>,
    /// 词典还在加载时用户查询的词，加载完成后自动补查。
    pub pending_term: Option<String>,
}

impl DictionaryState {
    pub fn status_for(&self, path: &std::path::Path) -> Option<&DictLoadState> {
        self.statuses
            .iter()
            .find(|status| status.path == path)
            .map(|status| &status.state)
    }

    /// 设置活动笺注卡在文档中的页码与纵向锚点。
    pub fn set_anchor(&mut self, page: usize, rects: Vec<RectPt>, anchor_top: f32) {
        self.anchor_page = page;
        self.anchor_rects = rects;
        self.anchor_top = if anchor_top.is_finite() && anchor_top >= 0.0 {
            anchor_top
        } else {
            0.0
        };
        self.back_stack.clear();
    }

    /// 清空一次查询的结果，保留词典加载状态。
    pub fn clear_results(&mut self) {
        self.request = self.request.wrapping_add(1);
        self.suggest_request = self.suggest_request.wrapping_add(1);
        self.suggestions.clear();
        self.word.clear();
        self.loading = false;
        self.entries.clear();
        self.not_found = false;
        self.error = None;
        self.pending_term = None;
        self.anchor_rects.clear();
        self.back_stack.clear();
        self.show_history = false;
    }
}

/// 从查询结果中提取音标与精简释义摘要（供钉住的页边笺注常驻展示）。
pub(crate) fn summarize_entries(entries: &[DictEntry], word: &str) -> (Option<String>, String) {
    let phonetic = entries
        .iter()
        .find_map(|entry| entry.phonetic.as_ref())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());

    let headword = word.trim();
    let mut lines = Vec::new();
    let mut total_chars = 0usize;

    for entry in entries {
        for block in &entry.blocks {
            for raw_line in block.text().lines() {
                let cleaned = raw_line.split_whitespace().collect::<Vec<_>>().join(" ");
                let cleaned = cleaned.trim();
                if cleaned.is_empty() {
                    continue;
                }
                // 跳过与词头或音标完全重复的首行。
                if cleaned.eq_ignore_ascii_case(headword) {
                    continue;
                }
                if phonetic.as_deref().is_some_and(|p| cleaned == p) {
                    continue;
                }
                let cleaned = if let Some(rest) = cleaned.strip_prefix(headword) {
                    let rest = rest.trim_start_matches(|c: char| {
                        c.is_whitespace() || matches!(c, '·' | ':' | '：' | '-' | '—')
                    });
                    if rest.is_empty() {
                        continue;
                    }
                    rest.to_string()
                } else {
                    cleaned.to_string()
                };

                if lines.iter().any(|existing: &String| existing == &cleaned) {
                    continue;
                }

                total_chars += cleaned.chars().count();
                lines.push(cleaned);
                if lines.len() >= 3 || total_chars >= 110 {
                    break;
                }
            }
            if lines.len() >= 3 || total_chars >= 110 {
                break;
            }
        }
        if !lines.is_empty() {
            break;
        }
    }

    let joined = lines.join("\n");
    let summary = if joined.is_empty() {
        "（已加笺）".to_string()
    } else if joined.chars().count() > 120 {
        let mut truncated: String = joined.chars().take(120).collect();
        truncated.push('…');
        truncated
    } else {
        joined
    };

    (phonetic, summary)
}

/// 查词前清洗：取第一行、去掉首尾标点与空白、限制长度。
pub(crate) fn normalize_term(raw: &str) -> String {
    let term = raw.lines().next().unwrap_or_default().trim();
    let term = term.trim_matches(is_edge_punctuation);
    term.chars().take(64).collect()
}

fn is_edge_punctuation(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '.' | ','
                | ';'
                | ':'
                | '!'
                | '?'
                | '"'
                | '\''
                | '“'
                | '”'
                | '‘'
                | '’'
                | '('
                | ')'
                | '（'
                | '）'
                | '['
                | ']'
                | '{'
                | '}'
                | '《'
                | '》'
                | '「'
                | '」'
                | '【'
                | '】'
                | '。'
                | '，'
                | '、'
                | '；'
                | '：'
                | '！'
                | '？'
                | '…'
                | '—'
                | '–'
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dict_core::{Block, BlockKind, BlockStyle, Paragraph, Span, SpanStyle, TextSpan};

    fn make_paragraph(text: &str) -> Block {
        Block::Paragraph(Paragraph {
            kind: BlockKind::Paragraph,
            style: BlockStyle::default(),
            spans: vec![Span::Text(TextSpan {
                text: text.to_string(),
                style: SpanStyle::default(),
            })],
        })
    }

    #[test]
    fn normalize_term_trims_punctuation_and_takes_first_line() {
        assert_eq!(normalize_term("  \"apple\",\nsecond"), "apple");
        assert_eq!(normalize_term("（你好）"), "你好");
        assert_eq!(normalize_term("..."), "");
    }

    #[test]
    fn normalize_term_caps_length() {
        let term = normalize_term(&"a".repeat(100));
        assert_eq!(term.chars().count(), 64);
    }

    #[test]
    fn summarize_entries_extracts_phonetic_and_skips_duplicate_headword() {
        let entry = DictEntry {
            headword: "gloss".into(),
            phonetic: Some("/ɡlɒs/".into()),
            blocks: vec![
                make_paragraph("gloss"),
                make_paragraph("/ɡlɒs/"),
                make_paragraph("n. 页边批注；训诂"),
                make_paragraph("v. 给…加注释"),
            ],
            source: Some("Oxford".into()),
        };
        let (phonetic, summary) = summarize_entries(&[entry], "gloss");
        assert_eq!(phonetic.as_deref(), Some("/ɡlɒs/"));
        assert_eq!(summary, "n. 页边批注；训诂\nv. 给…加注释");
    }
}
