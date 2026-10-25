//! AI 配置与模型构造。
//!
//! 这一层不依赖 GPUI，也不碰文档：它只回答两个问题——「配置是什么形状」和
//! 「按这份配置怎么造出一次调用」。工具、流式、卡片状态都在上层
//! （`gloss-pdf`）里，方便单测也方便以后换掉 rig。
//!
//! 约定：`rig` 的类型不越过本 crate 的公开 API，外部只拿到 [`DynModel`]。

pub mod agent;
pub mod client;
pub mod settings;
pub mod tools;

pub use agent::{
    AnswerSections, ErrorKind, LookupEvent, MAX_TURNS, WORD_MAX_CHARS, build_question,
    classify_error, is_word_query, parse_answer_sections, run_lookup,
};
pub use client::completion_model;
pub use settings::{
    AiSettings, ConfigError, DEFAULT_MAX_PAGES, LookupFeature, ProviderEntry, ProviderKind,
    mask_secret,
};
pub use tools::{
    CurrentPageTextTool, DocumentInfo, DocumentInfoTool, LookupContext, MAX_PAGE_CHARS,
    OutlineEntry, PageBudget, ReadPageTool, SurroundingTextTool, TocNearestTool, ToolError,
};
