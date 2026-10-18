//! 搜索状态：查询串、当前页匹配、全文命中与跨页跳转。
//!
//! 两个层次：
//! - **本页匹配**：`matches` 是当前页字符下标的区间，用来画高亮块。
//! - **全文命中**：`pages` 记录每页命中数（由 PDF worker 统计），`doc_index` 是
//!   当前命中在全文里的序号。`Enter` 在全文范围内循环，跳到别的页时把该页的
//!   第 N 个匹配记在 `pending`，等页面渲染完再选中它。

use std::ops::RangeInclusive;

use pdf_core::{CharBox, PageHits};

#[derive(Default)]
pub struct SearchState {
    pub open: bool,
    pub query: String,
    /// 当前页的匹配区间。
    pub matches: Vec<RangeInclusive<usize>>,
    /// 当前页里被选中的匹配序号。
    pub current: usize,
    /// 全文命中：只含有命中的页，按页码升序。
    pub pages: Vec<PageHits>,
    /// 全文总命中数。
    pub total: usize,
    /// 当前命中在全文里的序号（0 起）；`None` 表示还没定位。
    pub doc_index: Option<usize>,
    /// 全文搜索是否在跑。
    pub scanning: bool,
    /// 等页面渲染完要选中的页内序号。
    pending: Option<(usize, usize)>,
}

impl SearchState {
    pub fn close(&mut self) {
        self.open = false;
        self.clear();
    }

    /// 清空查询与所有结果（关闭搜索、切换文档时调用）。
    pub fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.current = 0;
        self.clear_results();
    }

    /// 只清空搜索结果，保留查询串。
    pub fn clear_results(&mut self) {
        self.matches.clear();
        self.current = 0;
        self.pages.clear();
        self.total = 0;
        self.doc_index = None;
        self.scanning = false;
        self.pending = None;
    }

    /// 重新计算当前页匹配。`page` 用来判断刚跳过来的目标页要不要定位到第 N 个。
    pub fn recompute(&mut self, chars: &[CharBox], page: usize) {
        self.matches.clear();
        self.current = 0;

        if !self.open || self.query.is_empty() {
            return;
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
            return;
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

        // 目标页还没渲染时 `chars` 是空的，这里不能把待定位的序号丢掉，
        // 等这一页的字符数据到了再定位。
        if !self.matches.is_empty()
            && let Some((target_page, local)) = self.pending
            && target_page == page
        {
            self.current = local.min(self.matches.len() - 1);
            self.pending = None;
        }
    }

    /// 收到全文搜索结果。
    pub fn set_doc_hits(&mut self, pages: Vec<PageHits>, total: usize) {
        self.pages = pages;
        self.total = total;
        self.scanning = false;
        if self.doc_index.is_some_and(|index| index >= total) {
            self.doc_index = if total == 0 { None } else { Some(total - 1) };
        }
    }

    /// 全文第 `index` 个命中所在页与页内序号。
    fn target_of(&self, mut index: usize) -> Option<(usize, usize)> {
        for hits in &self.pages {
            if index < hits.count {
                return Some((hits.page, index));
            }
            index -= hits.count;
        }
        None
    }

    /// 只在当前页内切换匹配（全文结果还没回来时用）。
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

    /// 前进/后退一个全文命中，返回目标页与页内序号。
    pub fn next_doc(&mut self, forward: bool) -> Option<(usize, usize)> {
        if self.total == 0 {
            return None;
        }

        let next = match (self.doc_index, forward) {
            (None, true) => 0,
            (None, false) => self.total - 1,
            (Some(index), true) => (index + 1) % self.total,
            (Some(index), false) => {
                if index == 0 {
                    self.total - 1
                } else {
                    index - 1
                }
            }
        };

        self.doc_index = Some(next);
        let target = self.target_of(next);
        if let Some(target) = target {
            self.pending = Some(target);
        }
        target
    }

    /// 记录“当前命中在第 `page` 页”（切页后用于状态栏显示）。
    pub fn hit_position(&self) -> Option<(usize, usize)> {
        self.doc_index.map(|index| (index + 1, self.total))
    }

    /// 把命中指针退到「当前页及之后的第一处命中」之前，这样第一次 Enter 就落在它上面。
    pub fn rewind_to(&mut self, page: usize) {
        let mut index = 0;
        for hits in &self.pages {
            if hits.page >= page {
                break;
            }
            index += hits.count;
        }
        self.doc_index = index.checked_sub(1);
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
        search.recompute(&chars("hello world"), 0);
        assert_eq!(search.matches.len(), 2);
    }

    #[test]
    fn next_wraps_around() {
        let mut search = SearchState {
            open: true,
            query: "o".into(),
            ..Default::default()
        };
        search.recompute(&chars("foo"), 0);
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
        search.recompute(&chars("foo"), 0);
        search.close();
        assert!(!search.open);
        assert!(search.query.is_empty());
        assert!(search.matches.is_empty());
    }

    #[test]
    fn doc_navigation_maps_global_index_to_page_and_local() {
        let mut search = SearchState {
            open: true,
            query: "x".into(),
            ..Default::default()
        };
        search.set_doc_hits(
            vec![
                PageHits { page: 2, count: 2 },
                PageHits { page: 5, count: 3 },
            ],
            5,
        );

        assert_eq!(search.next_doc(true), Some((2, 0)));
        assert_eq!(search.next_doc(true), Some((2, 1)));
        assert_eq!(search.next_doc(true), Some((5, 0)));
        assert_eq!(search.next_doc(false), Some((2, 1)));
        // 回头再往前，从最后一条继续。
        search.doc_index = Some(4);
        assert_eq!(search.next_doc(true), Some((2, 0)));
    }

    #[test]
    fn jump_marks_local_match_after_render() {
        let mut search = SearchState {
            open: true,
            query: "o".into(),
            ..Default::default()
        };
        search.set_doc_hits(vec![PageHits { page: 1, count: 2 }], 2);
        assert_eq!(search.next_doc(true), Some((1, 0)));
        assert_eq!(search.next_doc(true), Some((1, 1)));

        // 目标页渲染完成后定位到第二个匹配。
        search.recompute(&chars("foo"), 1);
        assert_eq!(search.matches.len(), 2);
        assert_eq!(search.current, 1);

        // 其他页不受影响。
        search.recompute(&chars("foo"), 0);
        assert_eq!(search.current, 0);
    }

    #[test]
    fn finds_chinese_queries_by_character_index() {
        let text = "这是一段中文，里面有两个汉字，汉字要能找到";
        let mut search = SearchState {
            open: true,
            query: "汉字".into(),
            ..Default::default()
        };
        search.recompute(&chars(text), 0);

        assert_eq!(search.matches.len(), 2);

        // 区间是字符下标，不是字节下标：直接拿去索引 chars/rects 才对得上高亮。
        let chars: Vec<char> = text.chars().collect();
        let first = *search.matches[0].start();
        assert_eq!(chars[first], '汉');
        assert_eq!(chars[*search.matches[0].end()], '字');
        let second = *search.matches[1].start();
        assert_eq!(chars[second], '汉');
        assert_eq!(chars[*search.matches[1].end()], '字');
    }

    #[test]
    fn chinese_query_matches_single_character_repeatedly() {
        let mut search = SearchState {
            open: true,
            query: "字".into(),
            ..Default::default()
        };
        search.recompute(&chars("字字不同字"), 0);
        assert_eq!(search.matches.len(), 3);
    }

    #[test]
    fn chinese_query_does_not_match_out_of_order_characters() {
        let mut search = SearchState {
            open: true,
            query: "字汉".into(),
            ..Default::default()
        };
        search.recompute(&chars("汉字之间"), 0);
        assert!(search.matches.is_empty());
    }

    #[test]
    fn empty_query_clears_page_matches() {
        let mut search = SearchState {
            open: true,
            query: "".into(),
            ..Default::default()
        };
        search.recompute(&chars("anything"), 0);
        assert!(search.matches.is_empty());
    }
}
