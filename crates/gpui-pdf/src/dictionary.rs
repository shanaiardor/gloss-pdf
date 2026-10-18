//! 词典域：窗口的界面状态（当前词、加载状态、查询结果）与词条清洗。

use dict_core::DictEntry;

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
    pub loading: bool,
    pub entries: Vec<DictEntry>,
    pub not_found: bool,
    pub error: Option<String>,
    pub load_state: DictLoadState,
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
    /// 查询代次号，用来丢弃过期结果。
    pub request: u64,
    /// 词典还在加载时用户查询的词，加载完成后自动补查。
    pub pending_term: Option<String>,
}

impl From<&DictionaryState> for DictionaryView {
    fn from(state: &DictionaryState) -> Self {
        Self {
            word: state.word.clone(),
            loading: state.loading,
            entries: state.entries.clone(),
            not_found: state.not_found,
            error: state.error.clone(),
            load_state: state.load_state.clone(),
        }
    }
}

impl DictionaryState {
    /// 清空一次查询的结果，保留词典加载状态。
    pub fn clear_results(&mut self) {
        self.request = self.request.wrapping_add(1);
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
