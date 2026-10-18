//! 词典输入联想的界面侧约束：条数上限与防抖间隔。
//!
//! 真正的前缀扫描在 dict-core 的 worker 线程里完成；这里只保留可单测的
//! 纯常量/裁剪函数，避免 UI 代码把“最多 8 条”写成散落的魔法数字。

/// 跨词典合并、去重后最多展示多少条建议。
pub const MAX_SUGGESTIONS: usize = 8;
/// 连续输入时等待多久再发前缀请求，避免每个按键都扫词典。
pub const SUGGEST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(160);

/// 裁剪并去重（忽略大小写），保留最先出现的写法。
pub fn trim_suggestions(suggestions: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut kept = Vec::new();
    for suggestion in suggestions {
        let suggestion = suggestion.trim();
        if suggestion.is_empty() || !seen.insert(suggestion.to_lowercase()) {
            continue;
        }
        kept.push(suggestion.to_string());
        if kept.len() == MAX_SUGGESTIONS {
            break;
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_blank_duplicates_case_insensitively_and_caps_at_eight() {
        let input = vec![
            " Apple ".to_string(),
            "apple".to_string(),
            String::new(),
            "banana".to_string(),
            "Cherry".to_string(),
            "cherry".to_string(),
            "d1".to_string(),
            "d2".to_string(),
            "d3".to_string(),
            "d4".to_string(),
            "d5".to_string(),
            "d6".to_string(),
        ];
        let kept = trim_suggestions(input);
        assert_eq!(kept.len(), MAX_SUGGESTIONS);
        assert_eq!(kept[0], "Apple");
        assert_eq!(kept[1], "banana");
        assert_eq!(kept[2], "Cherry");
        assert!(!kept.iter().any(|value| value.is_empty()));
    }
}
