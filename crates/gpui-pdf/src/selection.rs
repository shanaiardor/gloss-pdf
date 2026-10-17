//! 文本选择状态：锚点/焦点字符索引，以及从字符包围盒重建矩形与文本。

use std::ops::RangeInclusive;

use pdf_core::{CharBox, RectPt};

use crate::geometry::merge_rects;

#[derive(Default)]
pub struct Selection {
    pub anchor: Option<usize>,
    pub focus: Option<usize>,
    pub dragging: bool,
}

impl Selection {
    pub fn clear(&mut self) {
        self.anchor = None;
        self.focus = None;
        self.dragging = false;
    }

    pub fn start_at(&mut self, index: usize) {
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

        for character in selected {
            if let Some((previous_rect, previous_char)) = previous {
                let new_line = (character.rect.bottom - previous_rect.bottom).abs()
                    > previous_rect.height() * 0.5;
                if new_line {
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                } else if !previous_char.is_whitespace()
                    && !character.ch.is_whitespace()
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
    pub fn select_word(&mut self, chars: &[CharBox], index: usize) {
        let Some(target) = chars.get(index) else {
            return;
        };

        if is_cjk(target.ch) || !is_word_char(target.ch) {
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
        selection.select_word(&chars, 7);
        assert_eq!(selection.text(&chars).as_deref(), Some("world"));
    }

    #[test]
    fn drag_selection_is_normalized() {
        let chars = chars();
        let mut selection = Selection::default();
        selection.start_at(6);
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
        selection.select_word(&chars, 1);
        assert_eq!(selection.text(&chars).as_deref(), Some("好"));
    }

    #[test]
    fn select_word_stops_at_line_boundary() {
        let mut chars = line("hello", 0.0, 0.0);
        chars.extend(line("world", 20.0, 0.0));
        let mut selection = Selection::default();
        selection.select_word(&chars, 7);
        assert_eq!(selection.text(&chars).as_deref(), Some("world"));
    }

    #[test]
    fn select_word_stops_at_wide_gap() {
        let mut chars = line("hello", 0.0, 0.0);
        chars.extend(line("world", 0.0, 120.0));
        let mut selection = Selection::default();
        selection.select_word(&chars, 6);
        assert_eq!(selection.text(&chars).as_deref(), Some("world"));
    }
}
