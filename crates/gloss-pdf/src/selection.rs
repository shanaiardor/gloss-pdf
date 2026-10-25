//! 文本选择状态：锚点/焦点字符索引，以及从字符包围盒重建矩形与文本。
//!
//! 连续滚动下选区属于某一页：`page` 记住这组字符下标是哪一页的，
//! 覆盖层和复制都按这一页取数据。

use std::ops::RangeInclusive;

use pdf_core::{CharBox, RectPt};

use crate::geometry::merge_rects;

/// 拖选松手后浮在选区旁的「词典 / AI」胶囊。
///
/// 松手不直接查询：拖选常常只是为了复制，自动发请求会打扰；点胶囊才算明确的意图。
pub struct SelectionCapsule {
    pub page: usize,
    /// 原始选区文本（保留标点、换行、词间空格）；AI 用这一份，词典另行清洗。
    pub text: String,
    pub rects: Vec<RectPt>,
}

#[derive(Default)]
pub struct Selection {
    /// 选区所在页；选区为空时为 `None`。
    pub page: Option<usize>,
    pub anchor: Option<usize>,
    pub focus: Option<usize>,
    pub dragging: bool,
}

impl Selection {
    pub fn clear(&mut self) {
        self.page = None;
        self.anchor = None;
        self.focus = None;
        self.dragging = false;
    }

    /// 开始一次拖选，选区归到 `page` 上。
    pub fn start_at(&mut self, page: usize, index: usize) {
        self.page = Some(page);
        self.anchor = Some(index);
        self.focus = Some(index);
        self.dragging = true;
    }

    pub fn extend_to(&mut self, index: usize) {
        self.focus = Some(index);
    }

    pub fn range(&self) -> Option<RangeInclusive<usize>> {
        let anchor = self.anchor?;
        let focus = self.focus?;
        Some(anchor.min(focus)..=anchor.max(focus))
    }

    /// 选中范围按行合并后的矩形（PDF 坐标）。
    pub fn rects(&self, chars: &[CharBox]) -> Vec<RectPt> {
        let Some(range) = self.range() else {
            return Vec::new();
        };
        chars
            .get(*range.start()..=*range.end())
            .map(|slice| merge_rects(slice.iter().map(|character| character.rect).collect()))
            .unwrap_or_default()
    }

    /// 复制用的纯文本：行尾补换行，词间大间隙补空格。
    pub fn text(&self, chars: &[CharBox]) -> Option<String> {
        let range = self.range()?;
        let selected = chars.get(*range.start()..=*range.end())?;

        let mut text = String::new();
        let mut previous: Option<(RectPt, char)> = None;

        for (index, character) in selected.iter().enumerate() {
            // 中文排版常在字与字之间塞空格（或字距大到会被当成空格）；
            // 选中多个词时这些空格只会干扰查词，夹在中文之间的空白直接丢掉。
            let space_between_cjk = character.ch.is_whitespace()
                && !matches!(character.ch, '\n' | '\r')
                && previous.is_some_and(|(_, previous_char)| is_cjk(previous_char))
                && selected[index + 1..]
                    .iter()
                    .find(|next| !next.ch.is_whitespace())
                    .is_some_and(|next| is_cjk(next.ch));
            if space_between_cjk {
                continue;
            }

            if let Some((previous_rect, previous_char)) = previous {
                let new_line = (character.rect.bottom - previous_rect.bottom).abs()
                    > previous_rect.height() * 0.5;
                if new_line {
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                } else if !previous_char.is_whitespace()
                    && !character.ch.is_whitespace()
                    && !(is_cjk(previous_char) && is_cjk(character.ch))
                    && character.rect.left - previous_rect.right > previous_rect.height() * 1.5
                {
                    text.push(' ');
                }
            }

            text.push(character.ch);
            previous = Some((character.rect, character.ch));
        }

        Some(text)
    }

    /// 双击选词：中文按单字，英文按词；不跨行、不跨栏。
    pub fn select_word(&mut self, page: usize, chars: &[CharBox], index: usize) {
        let Some(target) = chars.get(index) else {
            return;
        };

        if is_cjk(target.ch) || !is_word_char(target.ch) {
            self.page = Some(page);
            self.anchor = Some(index);
            self.focus = Some(index);
            return;
        }

        let line_height = target.rect.height().max(1.0);
        let same_line = |left: &CharBox, right: &CharBox| {
            (left.rect.bottom - right.rect.bottom).abs() <= line_height * 0.5
        };
        let close_enough = |left: &CharBox, right: &CharBox| {
            right.rect.left - left.rect.right <= line_height * 1.5
        };

        let mut start = index;
        while start > 0 {
            let previous = &chars[start - 1];
            let current = &chars[start];
            if !is_word_char(previous.ch)
                || !same_line(previous, current)
                || !close_enough(previous, current)
            {
                break;
            }
            start -= 1;
        }

        let mut end = index;
        while end + 1 < chars.len() {
            let current = &chars[end];
            let next = &chars[end + 1];
            if !is_word_char(next.ch) || !same_line(current, next) || !close_enough(current, next) {
                break;
            }
            end += 1;
        }

        self.page = Some(page);
        self.anchor = Some(start);
        self.focus = Some(end);
    }
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '\'' || ch == '-'
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars() -> Vec<CharBox> {
        let mut chars = Vec::new();
        let mut x = 0.0;
        for ch in "hello world".chars() {
            let width = if ch == ' ' { 3.0 } else { 6.0 };
            chars.push(CharBox {
                ch,
                rect: RectPt {
                    left: x,
                    bottom: 0.0,
                    right: x + width,
                    top: 10.0,
                },
            });
            x += width;
        }
        chars
    }

    #[test]
    fn select_word_expands_both_sides() {
        let chars = chars();
        let mut selection = Selection::default();
        selection.select_word(0, &chars, 7);
        assert_eq!(selection.text(&chars).as_deref(), Some("world"));
    }

    #[test]
    fn drag_selection_is_normalized() {
        let chars = chars();
        let mut selection = Selection::default();
        selection.start_at(0, 6);
        selection.extend_to(0);
        assert_eq!(selection.text(&chars).as_deref(), Some("hello w"));
    }

    #[test]
    fn empty_selection_has_no_rects() {
        let selection = Selection::default();
        assert!(selection.rects(&chars()).is_empty());
        assert!(selection.text(&chars()).is_none());
    }

    fn line(text: &str, bottom: f32, x_start: f32) -> Vec<CharBox> {
        let mut chars = Vec::new();
        let mut x = x_start;
        for ch in text.chars() {
            let width = if ch == ' ' { 3.0 } else { 6.0 };
            chars.push(CharBox {
                ch,
                rect: RectPt {
                    left: x,
                    bottom,
                    right: x + width,
                    top: bottom + 10.0,
                },
            });
            x += width;
        }
        chars
    }

    #[test]
    fn double_click_cjk_selects_single_char() {
        let chars = line("你好世界", 0.0, 0.0);
        let mut selection = Selection::default();
        selection.select_word(0, &chars, 1);
        assert_eq!(selection.text(&chars).as_deref(), Some("好"));
    }

    #[test]
    fn spaces_inside_chinese_are_dropped() {
        let chars = line("汉 字", 0.0, 0.0);
        let mut selection = Selection::default();
        selection.start_at(0, 0);
        selection.extend_to(2);
        assert_eq!(selection.text(&chars).as_deref(), Some("汉字"));
    }

    #[test]
    fn wide_gaps_between_chinese_do_not_become_spaces() {
        let mut chars = line("汉", 0.0, 0.0);
        chars.extend(line("字", 0.0, 40.0));
        let mut selection = Selection::default();
        selection.start_at(0, 0);
        selection.extend_to(1);
        assert_eq!(selection.text(&chars).as_deref(), Some("汉字"));
    }

    #[test]
    fn spaces_between_latin_words_are_kept() {
        let chars = chars();
        let mut selection = Selection::default();
        selection.start_at(0, 0);
        selection.extend_to(chars.len() - 1);
        assert_eq!(selection.text(&chars).as_deref(), Some("hello world"));
    }

    #[test]
    fn select_word_stops_at_line_boundary() {
        let mut chars = line("hello", 0.0, 0.0);
        chars.extend(line("world", 20.0, 0.0));
        let mut selection = Selection::default();
        selection.select_word(0, &chars, 7);
        assert_eq!(selection.text(&chars).as_deref(), Some("world"));
    }

    #[test]
    fn select_word_stops_at_wide_gap() {
        let mut chars = line("hello", 0.0, 0.0);
        chars.extend(line("world", 0.0, 120.0));
        let mut selection = Selection::default();
        selection.select_word(0, &chars, 6);
        assert_eq!(selection.text(&chars).as_deref(), Some("world"));
    }
}
