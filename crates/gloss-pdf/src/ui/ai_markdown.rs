//! 轻量 Markdown 渲染：只覆盖模型实际会写的那一小撮语法。
//!
//! 为什么不用 Zed 的 `markdown` crate：它是 **GPL-3.0-or-later**，链接进来会把整个
//! 项目拖成 GPL；而且它绑死了 zed 的 theme/settings/ui 一整套 crate。这里保留它的
//! 核心手法（`pulldown-cmark` 解析、未闭合语法按字面输出即可流式渲染），代码自己写。
//!
//! 解析放在渲染时同步做：卡片文本上限 6000 字符，pulldown-cmark 解析这点量在微秒级，
//! 不必像 Zed 那样丢到后台再合并重入。

use gpui::{FontStyle, FontWeight, StyledText, TextRun, UnderlineStyle, div, prelude::*, px};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::theme::Theme;

/// 行内片段；`code` / `link` 只影响样式，不改变内容。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MdBlock {
    Paragraph(Vec<Span>),
    /// 标题按级别降级成加粗文本：340px 宽的小卡里不需要真正的字号层级。
    Heading { level: u8, spans: Vec<Span> },
    List { ordered: bool, items: Vec<Vec<Span>> },
    Rule,
}

/// 解析成块。不认识的语法（表格、代码块围栏、脚注）退化成普通段落文本，
/// 流式过程中的半成品也不会让整段消失。
pub(crate) fn parse(text: &str) -> Vec<MdBlock> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(text, options);

    let mut blocks = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut bold = 0usize;
    let mut italic = 0usize;
    let mut link = 0usize;
    let mut heading: Option<u8> = None;
    let mut list: Option<(bool, Vec<Vec<Span>>)> = None;

    fn flush(spans: &mut Vec<Span>, out: &mut Vec<MdBlock>, heading: Option<u8>) {
        if spans.iter().all(|span| span.text.trim().is_empty()) {
            spans.clear();
            return;
        }
        let taken = std::mem::take(spans);
        match heading {
            Some(level) => out.push(MdBlock::Heading {
                level,
                spans: taken,
            }),
            None => out.push(MdBlock::Paragraph(taken)),
        }
    }

    for event in parser {
        match event {
            Event::Start(Tag::Paragraph) => spans.clear(),
            Event::End(TagEnd::Paragraph) => match &mut list {
                Some((_, items)) => {
                    let taken = std::mem::take(&mut spans);
                    if !taken.is_empty() {
                        items.push(taken);
                    }
                }
                None => flush(&mut spans, &mut blocks, heading),
            },
            Event::Start(Tag::Heading { level, .. }) => heading = Some(level as u8),
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut spans, &mut blocks, heading);
                heading = None;
            }
            Event::Start(Tag::List(start)) => list = Some((start.is_some(), Vec::new())),
            Event::End(TagEnd::List(_)) => {
                if let Some((ordered, items)) = list.take()
                    && !items.is_empty()
                {
                    blocks.push(MdBlock::List { ordered, items });
                }
            }
            Event::Start(Tag::Item) => spans.clear(),
            Event::End(TagEnd::Item) => {
                if let Some((_, items)) = &mut list {
                    let taken = std::mem::take(&mut spans);
                    if !taken.is_empty() {
                        items.push(taken);
                    }
                }
            }
            Event::Start(Tag::Strong) => bold += 1,
            Event::End(TagEnd::Strong) => bold = bold.saturating_sub(1),
            Event::Start(Tag::Emphasis) => italic += 1,
            Event::End(TagEnd::Emphasis) => italic = italic.saturating_sub(1),
            Event::Start(Tag::Link { .. }) => link += 1,
            Event::End(TagEnd::Link) => link = link.saturating_sub(1),
            Event::Start(Tag::CodeBlock(_)) => {}
            Event::End(TagEnd::CodeBlock) => flush(&mut spans, &mut blocks, heading),
            Event::Code(text) => spans.push(Span {
                text: text.to_string(),
                bold: bold > 0,
                italic: italic > 0,
                code: true,
                link: false,
            }),
            Event::Text(text) => spans.push(Span {
                text: text.to_string(),
                bold: bold > 0,
                italic: italic > 0,
                code: false,
                link: link > 0,
            }),
            Event::SoftBreak | Event::HardBreak => spans.push(Span {
                text: " ".to_string(),
                ..Span::default()
            }),
            Event::Rule => {
                flush(&mut spans, &mut blocks, heading);
                blocks.push(MdBlock::Rule);
            }
            _ => {}
        }
    }
    flush(&mut spans, &mut blocks, heading);
    blocks
}

/// 渲染成 GPUI 元素树；`size` 是正文字号。
pub(crate) fn render(text: &str, theme: &Theme, size: f32) -> gpui::Div {
    let blocks = parse(text);
    // w_full + min_w(0) 是必须的：里面是横向 flex 行，没有宽度约束时
    // StyledText 的固有宽度会把行撑出卡片，右侧被裁掉半个字。
    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .w_full()
        .min_w(px(0.))
        .text_size(px(size));

    for block in blocks {
        match block {
            MdBlock::Paragraph(spans) => {
                body = body.child(
                    div()
                        .flex()
                        .w_full()
                        .min_w(px(0.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(styled_line(&spans, theme, size, FontWeight::NORMAL)),
                        ),
                );
            }
            MdBlock::Heading { level, spans } => {
                let weight = if level <= 2 {
                    FontWeight::BOLD
                } else {
                    FontWeight::SEMIBOLD
                };
                body = body.child(
                    div()
                        .flex()
                        .w_full()
                        .min_w(px(0.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(styled_line(&spans, theme, size + 0.5, weight)),
                        ),
                );
            }
            MdBlock::List { ordered, items } => {
                for (index, item) in items.iter().enumerate() {
                    let marker = if ordered {
                        format!("{}.", index + 1)
                    } else {
                        "•".to_string()
                    };
                    body = body.child(
                        div()
                            .flex()
                            .items_start()
                            .w_full()
                            .min_w(px(0.))
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(theme.text_muted)
                                    .child(marker),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(styled_line(item, theme, size, FontWeight::NORMAL)),
                            ),
                    );
                }
            }
            MdBlock::Rule => {
                body = body.child(div().h(px(1.)).w_full().bg(theme.border));
            }
        }
    }

    body
}

/// 把 Markdown 压成纯文本：紧凑卡（钉住的笺注）只要一两行预览，
/// 渲染标题/列表的排版反而会撑高卡片，而去掉标记后读起来更干净。
pub(crate) fn plain_text(markdown: &str) -> String {
    let mut out = String::new();
    for block in parse(markdown) {
        let (spans, prefix) = match block {
            MdBlock::Paragraph(spans) => (spans, ""),
            MdBlock::Heading { spans, .. } => (spans, ""),
            MdBlock::List { ordered, items } => {
                let mut buffer = String::new();
                for (index, item) in items.into_iter().enumerate() {
                    if !buffer.is_empty() {
                        buffer.push(' ');
                    }
                    if ordered {
                        buffer.push_str(&format!("{}. ", index + 1));
                    } else {
                        buffer.push_str("· ");
                    }
                    buffer.push_str(&spans_text(&item));
                }
                out.push_str(buffer.trim_end());
                out.push(' ');
                continue;
            }
            MdBlock::Rule => continue,
        };
        let text = spans_text(&spans);
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(prefix);
        out.push_str(&text);
    }
    out.trim().to_string()
}

fn spans_text(spans: &[Span]) -> String {
    let mut text = String::new();
    for span in spans {
        text.push_str(&span.text);
    }
    text.trim().to_string()
}

fn styled_line(
    spans: &[Span],
    theme: &Theme,
    _size: f32,
    base_weight: FontWeight,
) -> StyledText {
    let mut text = String::new();
    let mut runs = Vec::new();

    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        text.push_str(&span.text);
        let family = if span.code {
            theme.mono_font.clone()
        } else {
            theme.font_family.clone()
        };
        let mut font = gpui::Font {
            family,
            ..Default::default()
        };
        font.weight = if span.bold {
            FontWeight::BOLD
        } else {
            base_weight
        };
        font.style = if span.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };

        runs.push(TextRun {
            len: span.text.len(),
            font,
            color: if span.link { theme.accent } else { theme.text }.into(),
            background_color: span.code.then_some(theme.surface_hover.into()),
            underline: span.link.then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(theme.accent.into()),
                wavy: false,
            }),
            strikethrough: None,
        });
    }

    StyledText::new(text).with_runs(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_and_code_become_spans_with_flags() {
        let blocks = parse("这是 **重点** 和 `代码`。");
        let [MdBlock::Paragraph(spans)] = blocks.as_slice() else {
            panic!("应为一段：{blocks:?}");
        };
        assert!(spans.iter().any(|s| s.bold && s.text == "重点"));
        assert!(spans.iter().any(|s| s.code && s.text == "代码"));
    }

    #[test]
    fn lists_and_headings_are_recognised() {
        let blocks = parse("## 出处\n- 甲\n- 乙\n\n1. 一\n2. 二");
        assert!(matches!(blocks[0], MdBlock::Heading { level: 2, .. }));
        let unordered = blocks.iter().any(|block| {
            matches!(
                block,
                MdBlock::List {
                    ordered: false,
                    items
                } if items.len() == 2
            )
        });
        let ordered = blocks.iter().any(|block| {
            matches!(
                block,
                MdBlock::List {
                    ordered: true,
                    items
                } if items.len() == 2
            )
        });
        assert!(unordered && ordered, "{blocks:?}");
    }

    #[test]
    fn unclosed_markup_still_renders_the_text() {
        // 流式常见：`**` 还没闭合。按字面输出，不能丢字。
        let blocks = parse("正在解释 **子产");
        let [MdBlock::Paragraph(spans)] = blocks.as_slice() else {
            panic!("应为一段：{blocks:?}");
        };
        let joined: String = spans.iter().map(|span| span.text.as_str()).collect();
        assert!(joined.contains("子产"), "{joined}");
    }

    #[test]
    fn plain_text_drops_markers_but_keeps_the_words() {
        let md = "## 出处\n- **月淡星稀**：夜将尽。\n- 出自《警世通言》。";
        let text = plain_text(md);
        assert!(!text.contains('*'), "{text}");
        assert!(!text.contains('#'), "{text}");
        assert!(text.contains("月淡星稀"), "{text}");
        assert!(text.contains("警世通言"), "{text}");
        assert!(text.contains('·'), "列表项要有分隔符：{text}");
        assert_eq!(plain_text(""), "");
    }

    #[test]
    fn rule_and_empty_input_are_handled() {
        assert!(parse("").is_empty());
        assert_eq!(parse("---"), vec![MdBlock::Rule]);
    }
}
