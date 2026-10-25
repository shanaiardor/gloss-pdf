//! PDF 引擎抽象：文档打开、页面渲染、文本/链接/目录提取。
//!
//! 这个 crate 不依赖任何 UI 框架，方便单元测试和以后替换渲染后端。
//!
//! ```no_run
//! use futures::channel::mpsc;
//! use pdf_core::{Command, PdfEngine};
//!
//! let (tx, _rx) = mpsc::unbounded();
//! let engine = PdfEngine::spawn(tx);
//! engine.send(Command::Open("book.pdf".into())).ok();
//! ```

mod engine;
mod metadata;
mod model;
mod pdfium_setup;
pub mod text_match;
mod thumbnail;

pub use engine::{Command, Event, PdfEngine, PdfiumGate, TileId, Visibility};
pub use model::{
    CharBox, DocumentMetadata, LinkBox, LinkTarget, OutlineItem, PageHits, PageSize, RectPt,
};
pub use thumbnail::{ThumbnailCommand, ThumbnailEngine, ThumbnailEvent};
