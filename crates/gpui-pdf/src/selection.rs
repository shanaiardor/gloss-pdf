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
                let new_line =
                    (character.rect.bottom - previous_rect.bottom).abs() > previous_rect.height() * 0.5;
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

    /// 双击选词：向两侧扩展到非字母数字边界。
    pub fn select_word(&mut self, chars: &[CharBox], index: usize) {
        let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';

        let mut start = index;
        while start > 0 && is_word(chars[start - 1].ch) {
            start -= 1;
        }

        let mut end = index;
        while end + 1 < chars.len() && is_word(chars[end + 1].ch) {
            end += 1;
        }

        self.anchor = Some(start);
        self.focus = Some(end);
    }
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
}