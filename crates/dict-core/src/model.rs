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
    /// 命中的词典名（MDX 头部 Title，缺失时用文件名）；聚合查询时标注来源。
    pub source: Option<String>,
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

/// 块级内容：段落、表格或 flex 容器。
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    Container(Container),
}

impl Block {
    /// 纯文本形式（表格按行拼接，容器按子项顺序拼接）。
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
            Block::Container(container) => container
                .items
                .iter()
                .map(|item| {
                    item.blocks
                        .iter()
                        .map(Block::text)
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// 是否没有任何可见内容。
    pub fn is_empty(&self) -> bool {
        match self {
            Block::Paragraph(paragraph) => paragraph.spans.is_empty(),
            Block::Table(table) => table.rows.iter().all(|row| row.cells.is_empty()),
            Block::Container(container) => container
                .items
                .iter()
                .all(|item| item.blocks.iter().all(|block| block.is_empty())),
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

/// `display: flex` 的容器：子元素按主轴排布。
///
/// 只在词典释义里用得上，因此只保留 flex 排版必需的子集；
/// `grid`、`inline-flex` 仍按原有近似处理。
#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    pub style: BlockStyle,
    pub flex: FlexStyle,
    pub items: Vec<FlexItem>,
}

/// 一个 flex 子项：外壳样式（内外边距、边框、伸缩比例）加内部块。
#[derive(Clone, Debug, PartialEq)]
pub struct FlexItem {
    pub style: BlockStyle,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FlexStyle {
    pub direction: FlexDirection,
    pub wrap: FlexWrap,
    pub justify: Justify,
    pub align_items: AlignItems,
    /// 主轴与交叉轴共用一个间距（`gap: 8px`）；分轴写法取最后一次声明。
    pub gap: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlexDirection {
    #[default]
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

impl FlexDirection {
    pub fn is_row(self) -> bool {
        matches!(self, FlexDirection::Row | FlexDirection::RowReverse)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlexWrap {
    #[default]
    NoWrap,
    Wrap,
    WrapReverse,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Justify {
    #[default]
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlignItems {
    Start,
    End,
    Center,
    Baseline,
    #[default]
    Stretch,
}

/// `align-self`：`Auto` 表示跟随容器的 `align-items`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlignSelf {
    #[default]
    Auto,
    Start,
    End,
    Center,
    Baseline,
    Stretch,
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
    pub margin_left: Option<f32>,
    pub margin_right: Option<f32>,
    pub padding: Edges,
    /// 额外的左侧缩进（列表层级用）。
    pub indent: f32,
    pub background: Option<Color>,
    pub border_top: Option<Border>,
    pub border_right: Option<Border>,
    pub border_bottom: Option<Border>,
    pub border_left: Option<Border>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// 列表项前缀，例如 `•` 或 `1.`。
    pub marker: Option<String>,
    /// 左右浮动；当前只做块级对齐近似，不做文字环绕。
    pub float: Option<FloatSide>,
    /// 行内块的垂直对齐。
    pub vertical_align: Option<AlignSelf>,
    /// flex 子项的伸缩比例；`None` 表示沿用默认。
    pub grow: Option<f32>,
    pub shrink: Option<f32>,
    /// flex 基准宽度（px）；`None` 表示 `auto`。
    pub basis: Option<f32>,
    pub align_self: Option<AlignSelf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatSide {
    Left,
    Right,
}

/// 行内内容：文字或图片。
#[derive(Clone, Debug, PartialEq)]
pub enum Span {
    Text(TextSpan),
    Image(ImageSpan),
    /// `display: inline-block`：行内原子盒子，内部可以再有块。
    Box(InlineBox),
}

#[derive(Clone, Debug, PartialEq)]
pub struct InlineBox {
    pub style: BlockStyle,
    pub blocks: Vec<Block>,
}

impl Span {
    /// 行内文本；图片返回替代文字。
    pub fn text(&self) -> String {
        match self {
            Span::Text(text) => text.text.clone(),
            Span::Image(image) => image.alt.clone(),
            Span::Box(inline) => inline
                .blocks
                .iter()
                .map(Block::text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
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
    /// 行内链接（`<a href>`），界面据此把区间做成可点击。
    pub link: Option<LinkTarget>,
    /// CSS 是否显式声明过 `text-decoration`（含 `none`）。
    /// 为 `false` 时界面可以给链接补默认下划线，`true` 则完全按 CSS 走。
    pub decorated: bool,
}

/// 行内链接目标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// 词典内部词条：`entry://xxx`、`bword://xxx`。
    Entry(String),
    /// 外部地址：`http(s)://`、`mailto:`。
    Uri(String),
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
