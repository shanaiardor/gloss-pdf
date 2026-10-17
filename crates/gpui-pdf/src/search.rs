//! 页内搜索状态：查询串、匹配字符区间、当前匹配序号。

use std::ops::RangeInclusive;

use pdf_core::CharBox;

#[derive(Default)]
pub struct SearchState {
    pub open: bool,
    pub query: String,
    pub matches: Vec<RangeInclusive<usize>>,
    pub current: usize,
}

impl SearchState {
    pub fn close(&mut self) {
        self.open = false;
        self.clear();
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.current = 0;
    }

    /// 重新计算当前页匹配，返回给状态栏的提示。
    pub fn recompute(&mut self, chars: &[CharBox]) -> Option<String> {
        self.matches.clear();
        self.current = 0;

        if !self.open || self.query.is_empty() {
            return None;
        }

        let needle: Vec<char> = self.query.to_lowercase().chars().collect();
        let haystack: Vec<char> = chars
            .iter()
            .map(|character| {
                character
                    .ch
                    .to_lowercase()
                    .next()
                    .unwrap_or(character.ch)
            })
            .collect();

        if needle.is_empty() || needle.len() > haystack.len() {
            return Some("本页无匹配".to_string());
        }

        let mut start = 0;
        while start + needle.len() <= haystack.len() {
            if haystack[start..start + needle.len()] == needle[..] {
                self.matches
                    .push(start..=start + needle.len() - 1);
                start += needle.len();
            } else {
                start += 1;
            }
        }

        if self.matches.is_empty() {
            Some("本页无匹配".to_string())
        } else {
            Some(format!("本页 {} 处匹配", self.matches.len()))
        }
    }

    /// 切换当前匹配；返回是否真的有匹配可跳。
    pub fn next(&mut self, forward: bool) -> bool {
        if self.matches.is_empty() {
            return false;
        }
        if forward {
            self.current = (self.current + 1) % self.matches.len();
        } else if self.current == 0 {
            self.current = self.matches.len() - 1;
        } else {
            self.current -= 1;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_core::RectPt;

    fn chars(text: &str) -> Vec<CharBox> {
        text.chars()
            .enumerate()
            .map(|(index, ch)| CharBox {
                ch,
                rect: RectPt {
                    left: index as f32 * 6.0,
                    bottom: 0.0,
                    right: index as f32 * 6.0 + 6.0,
                    top: 10.0,
                },
            })
            .collect()
    }

    #[test]
    fn finds_all_case_insensitive_matches() {
        let mut search = SearchState {
            open: true,
            query: "O".into(),
            ..Default::default()
        };
        search.recompute(&chars("hello world"));
        assert_eq!(search.matches.len(), 2);
    }

    #[test]
    fn next_wraps_around() {
        let mut search = SearchState {
            open: true,
            query: "o".into(),
            ..Default::default()
        };
        search.recompute(&chars("foo"));
        assert_eq!(search.matches.len(), 2);
        assert_eq!(search.current, 0);
        assert!(search.next(true));
        assert_eq!(search.current, 1);
        assert!(search.next(true));
        assert_eq!(search.current, 0);
        assert!(search.next(false));
        assert_eq!(search.current, 1);
    }

    #[test]
    fn close_clears_query_and_matches() {
        let mut search = SearchState {
            open: true,
            query: "o".into(),
            ..Default::default()
        };
        search.recompute(&chars("foo"));
        search.close();
        assert!(!search.open);
        assert!(search.query.is_empty());
        assert!(search.matches.is_empty());
    }
}