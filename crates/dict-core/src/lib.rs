//! MDict 词典引擎：解析 `.mdx`、查询词条并转成 UI 无关的结构。
//!
//! UI 侧只通过 `Command` / `Event` 与这里交互；`MdxFile` 只活在 worker 线程内。

mod css;
pub mod engine;
pub mod fonts;
pub mod html;
pub mod model;

pub use css::Stylesheet;
pub use engine::{Command, DictEngine, DictionaryConfig, DictionaryInfo, Event};
pub use fonts::FontLibrary;
pub use html::{NoResources, RenderContext, ResourceResolver, extract_phonetic, render, to_blocks};
pub use model::{
    Align, AlignItems, AlignSelf, Block, BlockKind, BlockStyle, Border, Color, Container,
    DictEntry, Edges, FlexDirection, FlexItem, FlexStyle, FlexWrap, ImageFormat, ImageSpan,
    Justify, LinkTarget, Paragraph, Span, SpanStyle, Table, TableCell, TableRow, TextSpan,
};
