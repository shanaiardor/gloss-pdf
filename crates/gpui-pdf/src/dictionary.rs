//! 词典窗口的界面状态：当前词、加载状态、查询结果。

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
