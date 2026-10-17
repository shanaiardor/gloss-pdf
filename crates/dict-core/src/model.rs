//! UI 无关的词典条目结构：worker 解析 HTML/CSS 后交给界面渲染。
//!
//! 这里刻意只描述"画什么"，不出现 GPUI 类型；颜色、字号、边距都用普通数值。

use std::fmt;
use std::sync::Arc;

/// 一次查询的完整释义。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DictEntry {
    pub headword: String,
    pub phonetic: Option<String>,
    pub blocks: Vec<Block>,
}

impl DictEntry {
    /// 纯文本形式，测试与日志用。
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .map(Block::text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// 块级内容：段落或表格。
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
}

impl Block {
    /// 纯文本形式（表格按行拼接）。
    pub fn text(&self) -> String {
        match self {
            Block::Paragraph(paragraph) => paragraph.text(),
            Block::Table(table) => table
                .rows
                .iter()
                .map(|row| {
                    row.cells
                        .iter()
                        .map(|cell| {
                            cell.blocks
                                .iter()
                                .map(Block::text)
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .collect::<Vec<_>>()
                        .join("\t")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// 是否没有任何可见内容。
    pub fn is_empty(&self) -> bool {
        match self {
            Block::Paragraph(paragraph) => paragraph.spans.is_empty(),
            Block::Table(table) => table.rows.iter().all(|row| row.cells.is_empty()),
        }
    }
}

/// 段落：一段带统一样式的行内内容。
#[derive(Clone, Debug, PartialEq)]
pub struct Paragraph {
    pub kind: BlockKind,
    pub style: BlockStyle,
    pub spans: Vec<Span>,
}

impl Paragraph {
    pub fn text(&self) -> String {
        self.spans
            .iter()
            .map(Span::text)
            .collect::<Vec<_>>()
            .join("")
    }
}

/// 表格：MDX 释义常用表格做左右分栏。
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub style: BlockStyle,
    pub rows: Vec<TableRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableRow {
    pub style: BlockStyle,
    pub cells: Vec<TableCell>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableCell {
    pub style: BlockStyle,
    pub colspan: u16,
    pub blocks: Vec<Block>,
}

/// 段落的语义分类，界面据此给默认字号/颜色。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockKind {
    Heading(u8),
    #[default]
    Paragraph,
    ListItem,
    Quote,
    /// `<hr>`：没有文本，只画一条分隔线。
    Rule,
}

/// 段落级样式：来自 CSS 的块属性，`None` 表示交给界面按主题决定。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockStyle {
    pub size: Option<f32>,
    pub line_height: Option<f32>,
    pub color: Option<Color>,
    pub family: Option<Vec<String>>,
    pub align: Option<Align>,
    pub margin_top: Option<f32>,
    pub margin_bottom: Option<f32>,
    pub padding: Edges,
    /// 额外的左侧缩进（列表层级用）。
    pub indent: f32,
    pub background: Option<Color>,
    pub border_top: Option<Border>,
    pub border_right: Option<Border>,
    pub border_bottom: Option<Border>,
    pub border_left: Option<Border>,
    pub width: Option<f32>,
    /// 列表项前缀，例如 `•` 或 `1.`。
    pub marker: Option<String>,
}

/// 行内内容：文字或图片。
#[derive(Clone, Debug, PartialEq)]
pub enum Span {
    Text(TextSpan),
    Image(ImageSpan),
}

impl Span {
    /// 行内文本；图片返回替代文字。
    pub fn text(&self) -> String {
        match self {
            Span::Text(text) => text.text.clone(),
            Span::Image(image) => image.alt.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextSpan {
    pub text: String,
    pub style: SpanStyle,
}

/// 行内样式：能落到 GPUI text run 上的属性。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// 下划线颜色（来自 `border-bottom` 之类的近似）。
    pub underline_color: Option<Color>,
    pub strike: bool,
    pub mono: bool,
    pub size: Option<f32>,
    pub color: Option<Color>,
    pub background: Option<Color>,
    /// 已经按字符覆盖范围挑好的字体名。
    pub family: Option<String>,
}

/// 行内图片：字节来自 `.mdd` 或 `data:` URI。
#[derive(Clone, PartialEq)]
pub struct ImageSpan {
    pub data: Arc<[u8]>,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    /// CSS / 属性指定的显示尺寸（px）。
    pub display_width: Option<f32>,
    pub display_height: Option<f32>,
    pub alt: String,
}

impl fmt::Debug for ImageSpan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageSpan")
            .field("format", &self.format)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.data.len())
            .field("alt", &self.alt)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
}

impl ImageFormat {
    /// 按文件头猜格式；认不出就不渲染。
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if bytes.starts_with(b"BM") {
            Some(Self::Bmp)
        } else {
            None
        }
    }
}

/// sRGB 颜色，避免把 GPUI 的 `Rgba` 带进 core。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn is_transparent(self) -> bool {
        self.a == 0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    pub width: f32,
    pub color: Option<Color>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}
