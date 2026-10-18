//! 词典域：窗口的界面状态（当前词、加载状态、查询结果）与词条清洗。

use std::path::PathBuf;

use dict_core::DictEntry;

/// 词典窗口发回主窗口的请求：查词，或清空持久化历史。
#[derive(Clone, Debug)]
pub enum DictRequest {
    Lookup(String),
    /// 输入框内容变化后请求前缀联想；主窗口负责防抖。
    Suggest(String),
    ClearHistory,
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

/// 词典窗口需要的最小快照；主窗口状态变化时推送给独立窗口。
#[derive(Clone, Default)]
pub struct DictionaryView {
    pub word: String,
    /// 最近查过的词，最新的在前；最多 50 条。
    pub history: Vec<String>,
    /// 当前输入的前缀建议，最多 8 条。
    pub suggestions: Vec<String>,
    pub loading: bool,
    pub entries: Vec<DictEntry>,
    pub not_found: bool,
    pub error: Option<String>,
    pub load_state: DictLoadState,
}

/// 一本已配置词典的加载状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictStatus {
    pub path: PathBuf,
    pub state: DictLoadState,
}

#[derive(Default)]
pub struct DictionaryState {
    pub open: bool,
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

impl From<&DictionaryState> for DictionaryView {
    fn from(state: &DictionaryState) -> Self {
        Self {
            word: state.word.clone(),
            history: Vec::new(),
            suggestions: Vec::new(),
            loading: state.loading,
            entries: state.entries.clone(),
            not_found: state.not_found,
            error: state.error.clone(),
            load_state: state.load_state.clone(),
        }
    }
}

impl DictionaryState {
    pub fn status_for(&self, path: &std::path::Path) -> Option<&DictLoadState> {
        self.statuses
            .iter()
            .find(|status| status.path == path)
            .map(|status| &status.state)
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
    }
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
}
