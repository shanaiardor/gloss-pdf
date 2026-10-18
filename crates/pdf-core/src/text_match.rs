//! 文本匹配规则：UI 页内高亮与 worker 全文计数共用这一份实现。
//!
//! 两份实现走偏的后果很具体：全文总数与页内高亮数量对不上，跨页跳转会定位错位。
//! 所以匹配规则（大小写折叠、忽略空白、不重叠）只在这里定义一次。
//!
//! 空白字符一律不参与匹配：查询串里的空白先整串去掉，被搜文本也去掉空白后再比对，
//! 命中下标再映射回**原字符序列**。这样排版软件插的空格、「回车」换行、制表符都
//! 不会让搜索落空——「汉 字」能被「汉字」命中，「T\n细胞」能被「T 细胞」命中；
//! 同时高亮区间仍覆盖中间那些空白，可以直接拿去索引 `CharBox`。

use std::ops::RangeInclusive;

/// 在字符序列里找所有不重叠的命中，返回原序列的字符下标区间。
///
/// 查询串与文本都先去掉空白再比对（见模块文档），返回的区间映射回原始序列，
/// 因此覆盖命中内部的空白。
pub fn find_matches(query: &str, chars: &[char]) -> Vec<RangeInclusive<usize>> {
    let needle: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|ch| !is_ignorable_space(*ch))
        .collect();
    if needle.is_empty() {
        return Vec::new();
    }

    // 文本去空白后逐字符折叠；`origins` 记住每个保留字符在原序列里的位置。
    let mut compact = Vec::with_capacity(chars.len());
    let mut origins = Vec::with_capacity(chars.len());
    for (index, ch) in chars.iter().copied().enumerate() {
        if is_ignorable_space(ch) {
            continue;
        }
        compact.push(fold_char(ch));
        origins.push(index);
    }

    scan(&needle, &compact)
        .into_iter()
        .map(|range| origins[*range.start()]..=origins[*range.end()])
        .collect()
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

/// 匹配前要去掉的字符：所有 Unicode 空白（空格、制表符、换行、回车、
/// 不间断空格、全角空格等）。
fn is_ignorable_space(ch: char) -> bool {
    ch.is_whitespace()
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
        // 同一页里既有「汉字」又有「汉 字」时两处都要命中。
        let matches = find_matches("汉字", &chars("汉字 汉 字"));
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0], 0..=1);
        assert_eq!(matches[1], 3..=5);
        assert_eq!(count_matches("汉字 汉 字", "汉字"), 2);
    }

    #[test]
    fn query_with_spaces_matches_spaced_text_and_covers_whitespace() {
        // 查询串与文本都带空格时命中一处，高亮仍从「汉」盖到「字」。
        let matches = find_matches("汉 字", &chars("汉 字"));
        assert_eq!(matches, vec![0..=2]);
    }

    #[test]
    fn whitespace_in_query_is_ignored_for_every_script() {
        // 用户从 PDF 里复制出来的词条常带空格、换行：查询串里的空白一律去掉。
        assert_eq!(find_matches("ab", &chars("a b")), vec![0..=2]);
        assert_eq!(find_matches("a b", &chars("ab")), vec![0..=1]);
        assert_eq!(find_matches("T 细胞", &chars("T细胞")), vec![0..=2]);
        assert_eq!(find_matches("汉a", &chars("汉 a")), vec![0..=2]);
        // 行首行尾空白同样不参与匹配。
        assert_eq!(
            find_matches("  hello \n", &chars("helloworld")),
            vec![0..=4]
        );
        // 纯空白查询不产生任何命中。
        assert!(find_matches(" \n\t", &chars("a b")).is_empty());
    }

    #[test]
    fn newlines_and_tabs_are_ignored_on_both_sides() {
        // 文本层换行（「回车」）不能把词拆开。
        assert_eq!(find_matches("hello", &chars("he\nllo")), vec![0..=5]);
        assert_eq!(find_matches("he\tllo", &chars("hello")), vec![0..=4]);
        assert_eq!(find_matches("发现", &chars("发\r\n现")), vec![0..=3]);
        // 全角空格与不间断空格也算空白。
        assert_eq!(find_matches("汉字", &chars("汉\u{3000}字")), vec![0..=2]);
        assert_eq!(find_matches("汉字", &chars("汉\u{00a0}字")), vec![0..=2]);
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
