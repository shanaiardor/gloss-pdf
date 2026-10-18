//! 文本匹配规则：UI 页内高亮与 worker 全文计数共用这一份实现。
//!
//! 两份实现走偏的后果很具体：全文总数与页内高亮数量对不上，跨页跳转会定位错位。
//! 所以匹配规则（大小写折叠、不重叠、汉字空格容错）只在这里定义一次。
//!
//! 汉字空格容错针对的是排版软件在汉字间插空格的情况：对中日韩查询额外做一次
//! 「去掉空白」的匹配，再与直接匹配按起点合并（保持不重叠），并把压缩后的
//! 下标映射回**原字符序列**，这样高亮区间既包含中间的空格，也能直接拿去索引
//! `CharBox`。同一页里既有「汉字」又有「汉 字」时，两种写法都会命中。

use std::ops::RangeInclusive;

/// 在字符序列里找所有不重叠的命中，返回原序列的字符下标区间。
pub fn find_matches(query: &str, chars: &[char]) -> Vec<RangeInclusive<usize>> {
    let needle: Vec<char> = query.to_lowercase().chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }

    let haystack: Vec<char> = chars.iter().copied().map(fold_char).collect();
    let direct = scan(&needle, &haystack);
    if !allow_space_insensitive(query) {
        return direct;
    }

    // 去掉空白再匹配；`origins` 记住每个保留字符在原序列里的位置。
    let mut compact = Vec::with_capacity(haystack.len());
    let mut origins = Vec::with_capacity(haystack.len());
    for (index, ch) in haystack.iter().copied().enumerate() {
        if !is_ignorable_space(ch) {
            compact.push(ch);
            origins.push(index);
        }
    }

    let compact_needle: Vec<char> = needle
        .iter()
        .copied()
        .filter(|ch| !is_ignorable_space(*ch))
        .collect();
    if compact_needle.is_empty() {
        return direct;
    }

    let mut candidates = direct;
    candidates.extend(
        scan(&compact_needle, &compact)
            .into_iter()
            .map(|range| origins[*range.start()]..=origins[*range.end()]),
    );

    // 直接匹配与压缩匹配可能指向同一处；按起点排序后贪心保留不重叠的区间，
    // 起点相同则保留先加入的直接匹配。
    candidates.sort_by(|a, b| a.start().cmp(b.start()).then(a.end().cmp(b.end())));
    let mut matches: Vec<RangeInclusive<usize>> = Vec::with_capacity(candidates.len());
    for range in candidates {
        if matches
            .last()
            .is_some_and(|last| range.start() <= last.end())
        {
            continue;
        }
        matches.push(range);
    }
    matches
}

/// 统计不重叠命中数；worker 全文计数用。
pub fn count_matches(text: &str, query: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    find_matches(query, &chars).len()
}

/// 单页逐字符折叠：小写化只取第一个字符，和原 UI 的规则保持一致。
fn fold_char(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

/// 不重叠扫描；命中后整段跳过。
fn scan(needle: &[char], haystack: &[char]) -> Vec<RangeInclusive<usize>> {
    let mut matches = Vec::new();
    if needle.len() > haystack.len() {
        return matches;
    }

    let mut start = 0;
    while start + needle.len() <= haystack.len() {
        if haystack[start..start + needle.len()] == needle[..] {
            matches.push(start..=start + needle.len() - 1);
            start += needle.len();
        } else {
            start += 1;
        }
    }
    matches
}

fn is_ignorable_space(ch: char) -> bool {
    ch.is_whitespace()
}

/// 是否允许「去掉空白再匹配一次」。
///
/// 只对中日韩文本启用：拉丁字母与数字混排时，空格本身有语义（「a b」不能
/// 命中「ab」），贸然压缩会造出用户没要的匹配。
fn allow_space_insensitive(query: &str) -> bool {
    let mut has_cjk = false;
    for ch in query.chars().filter(|ch| !ch.is_whitespace()) {
        if is_cjk(ch) {
            has_cjk = true;
        } else if ch.is_alphanumeric() {
            return false;
        }
    }
    has_cjk
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF     // 平假名、片假名
        | 0x3400..=0x4DBF   // 扩展 A
        | 0x4E00..=0x9FFF   // 基本区
        | 0xAC00..=0xD7AF   // 谚文音节
        | 0xF900..=0xFAFF   // 兼容表意文字
        | 0x20000..=0x2FA1F // 扩展 B 及以后
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    #[test]
    fn finds_all_non_overlapping_hits() {
        assert_eq!(find_matches("o", &chars("hello world")).len(), 2);
        assert_eq!(find_matches("aa", &chars("aaaa")).len(), 2);
        assert_eq!(find_matches("汉字", &chars("汉字里找汉字")).len(), 2);
        assert_eq!(find_matches("x", &chars("abc")).len(), 0);
        assert!(find_matches("", &chars("abc")).is_empty());
    }

    #[test]
    fn chinese_query_matches_across_inserted_spaces() {
        let text = "这 是 汉 字";
        let matches = find_matches("汉字", &chars(text));
        assert_eq!(matches.len(), 1);
        // 区间映射回原序列：从「汉」到「字」，中间的空格也包含在高亮里。
        assert_eq!(*matches[0].start(), 4);
        assert_eq!(*matches[0].end(), 6);
        let original: Vec<char> = text.chars().collect();
        assert_eq!(original[*matches[0].start()], '汉');
        assert_eq!(original[*matches[0].end()], '字');
    }

    #[test]
    fn chinese_query_with_spaces_still_matches_compact_text() {
        let matches = find_matches("汉 字", &chars("汉字"));
        assert_eq!(matches, vec![0..=1]);
    }

    #[test]
    fn chinese_query_matches_both_spaced_and_unspaced_occurrences() {
        // 直接匹配到一处时，不能漏掉同页里空格写法的另一处。
        let matches = find_matches("汉字", &chars("汉字 汉 字"));
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0], 0..=1);
        assert_eq!(matches[1], 3..=5);
        assert_eq!(count_matches("汉字 汉 字", "汉字"), 2);
    }

    #[test]
    fn direct_match_wins_over_compact_match() {
        // 文本里本来就有「汉 字」时，直接匹配优先，不去动其他空白。
        let matches = find_matches("汉 字", &chars("汉 字"));
        assert_eq!(matches, vec![0..=2]);
    }

    #[test]
    fn latin_query_does_not_ignore_spaces() {
        assert!(find_matches("ab", &chars("a b")).is_empty());
        assert!(find_matches("a b", &chars("ab")).is_empty());
        // 中英混排同样按精确规则处理。
        assert!(find_matches("汉a", &chars("汉 a")).is_empty());
    }

    #[test]
    fn count_matches_uses_the_same_rules_as_page_matching() {
        let text = "汉 字，还有 汉 字";
        assert_eq!(count_matches(text, "汉字"), 2);
        assert_eq!(
            count_matches(text, "汉字"),
            find_matches("汉字", &chars(text)).len()
        );
    }
}
