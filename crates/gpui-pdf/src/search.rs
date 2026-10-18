//! 搜索状态：查询串、逐页匹配、全文命中与跨页跳转。
//!
//! 三个层次：
//! - **逐页匹配**：`page_matches` 记录每一页的匹配区间，连续滚动时多页同时可见都能画高亮。
//! - **本页匹配**：`matches` 是「活动页」（锚线所在页）的匹配，`current` 指出当前第几个。
//! - **全文命中**：`pages` 记录每页命中数（由 PDF worker 统计），`doc_index` 是
//!   当前命中在全文里的序号。`Enter` 在全文范围内循环，跳到别的页时把该页的
//!   第 N 个匹配记在 `pending`，等页面渲染完再选中它。

use std::collections::HashMap;
use std::ops::RangeInclusive;

use pdf_core::{CharBox, PageHits};

#[derive(Default)]
pub struct SearchState {
    pub open: bool,
    pub query: String,
    /// 活动页的匹配区间。
    pub matches: Vec<RangeInclusive<usize>>,
    /// 活动页里被选中的匹配序号。
    pub current: usize,
    /// 全文命中：只含有命中的页，按页码升序。
    pub pages: Vec<PageHits>,
    /// 全文总命中数。
    pub total: usize,
    /// 当前命中在全文里的序号（0 起）；`None` 表示还没定位。
    pub doc_index: Option<usize>,
    /// 全文搜索是否在跑。
    pub scanning: bool,
    /// 全文扫描进度：已扫描页数与总页数；`scanned_total` 为 0 表示还没开始报进度。
    pub scanned: usize,
    pub scanned_total: usize,
    /// 每页的匹配区间，连续滚动时逐页画高亮用。
    page_matches: HashMap<usize, Vec<RangeInclusive<usize>>>,
    /// `matches` / `current` 属于哪一页。
    active_page: usize,
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
        self.scanned = 0;
        self.scanned_total = 0;
        self.page_matches.clear();
        self.pending = None;
    }

    /// 只清逐页缓存（查询串变了，旧的匹配全部作废）。
    pub fn clear_page_matches(&mut self) {
        self.page_matches.clear();
        self.matches.clear();
        self.current = 0;
    }

    pub fn active_page(&self) -> usize {
        self.active_page
    }

    /// 某一页的匹配区间，画高亮用。
    pub fn matches_for(&self, page: usize) -> &[RangeInclusive<usize>] {
        self.page_matches
            .get(&page)
            .map(|matches| matches.as_slice())
            .unwrap_or_default()
    }

    /// 重新计算活动页的匹配，并把结果写进逐页缓存。
    ///
    /// 目标页还没渲染时 `chars` 是空的，这里不能把待定位的序号丢掉，
    /// 等这一页的字符数据到了再定位。
    pub fn recompute(&mut self, chars: &[CharBox], page: usize) {
        let matches = find_matches(&self.query, chars);
        self.active_page = page;
        self.page_matches.insert(page, matches.clone());
        self.matches = matches;
        self.current = 0;

        if !self.matches.is_empty()
            && let Some((target_page, local)) = self.pending
            && target_page == page
        {
            self.current = local.min(self.matches.len() - 1);
            self.pending = None;
        }
    }

    /// 缓存非活动页的匹配，不动 `matches` / `current`。
    pub fn cache_page(&mut self, chars: &[CharBox], page: usize) {
        if !self.open || self.query.is_empty() {
            self.page_matches.remove(&page);
            return;
        }
        let matches = find_matches(&self.query, chars);
        if matches.is_empty() {
            self.page_matches.remove(&page);
        } else {
            self.page_matches.insert(page, matches);
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

    /// 开始一次全文扫描：清掉上一轮结果与进度。
    pub fn begin_scan(&mut self) {
        self.doc_index = None;
        self.total = 0;
        self.pages.clear();
        self.scanning = true;
        self.scanned = 0;
        self.scanned_total = 0;
    }

    /// 更新扫描进度；`total` 由 worker 在开始时给出。
    pub fn update_scan(&mut self, scanned: usize, total: usize) {
        if !self.scanning {
            return;
        }
        self.scanned = scanned;
        self.scanned_total = total;
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

/// 页内匹配：把 `CharBox` 序列交给 [`pdf_core::text_match`] 的共享规则。
///
/// 规则（大小写折叠、汉字空格容错、不重叠）只在那里定义一次，worker 的全文
/// 计数走同一份实现，避免两侧命中数对不上。
fn find_matches(query: &str, chars: &[CharBox]) -> Vec<RangeInclusive<usize>> {
    let plain: Vec<char> = chars.iter().map(|character| character.ch).collect();
    pdf_core::text_match::find_matches(query, &plain)
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
    fn chinese_query_matches_spaced_characters() {
        // 排版软件常在汉字之间插空格：查询「汉字」要命中「汉 字」，
        // 区间仍用原字符下标，高亮才能盖住中间的空格。
        let mut search = SearchState {
            open: true,
            query: "汉字".into(),
            ..Default::default()
        };
        search.recompute(&chars("汉 字 之 间"), 0);
        assert_eq!(search.matches.len(), 1);
        assert_eq!(*search.matches[0].start(), 0);
        assert_eq!(*search.matches[0].end(), 2);
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

    #[test]
    fn cached_matches_are_kept_per_page() {
        let mut search = SearchState {
            open: true,
            query: "o".into(),
            ..Default::default()
        };
        search.recompute(&chars("foo"), 0);
        // 连续滚动时别的页也要画高亮，但活动页仍是 0。
        search.cache_page(&chars("tool"), 3);

        assert_eq!(search.matches_for(0).len(), 2);
        assert_eq!(search.matches_for(3).len(), 2);
        assert!(search.matches_for(1).is_empty());
        assert_eq!(search.active_page(), 0);
        assert_eq!(search.matches.len(), 2);
        assert_eq!(search.current, 0);
    }

    #[test]
    fn clearing_page_matches_keeps_the_query() {
        let mut search = SearchState {
            open: true,
            query: "o".into(),
            ..Default::default()
        };
        search.recompute(&chars("foo"), 0);
        search.clear_page_matches();

        assert_eq!(search.query, "o");
        assert!(search.matches_for(0).is_empty());
        assert!(search.matches.is_empty());
    }
}
