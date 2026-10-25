//! 查词时提供给模型的只读工具。
//!
//! 工具实现留在 `ai-core`（`rig` 只允许出现在这里），文档数据通过
//! [`LookupContext`] 注入：`gloss-pdf` 在查词那一刻拍一份快照、再给一条取页通道，
//! 工具因此不依赖 GPUI，也能用假上下文单测。
//!
//! 取页是有预算的：模型每次查询最多额外读 `max_pages` 页，超了直接报错让它收手。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::Deserialize;

/// 单页文本送入模型前的截断长度（字符）。
///
/// 一页古籍可能 3000–5000 汉字，加上工具定义与系统提示，一次调用很容易冲到 8k token。
/// 6000 字符覆盖两页普通篇幅，超出部分对「这句话里它指谁」几乎无增益。
pub const MAX_PAGE_CHARS: usize = 6000;

/// 上下文窗口：查词那一刻的文档快照 + 取页通道。
pub trait LookupContext: Send + Sync + 'static {
    fn document(&self) -> Arc<DocumentInfo>;
    fn current_page(&self) -> usize;
    /// 取某一页（0 基）的文本；实现方通过通道请求引擎、在事件到达时回填。
    ///
    /// 当前页也走这条路：文本缓存和全文搜索共用一份，命中时是纯内存读，
    /// 但接口统一成异步，工具里就不用区分「本地页」和「远端页」。
    fn page_text(&self, page: usize)
    -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentInfo {
    pub file_name: String,
    pub page_count: usize,
    /// 只保留有值的元数据字段，避免把一长串「—」塞给模型。
    pub metadata: Vec<(String, String)>,
    /// 目录条目（标题 + 页码），可能为空（扫描件常见）。
    pub outline: Vec<OutlineEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineEntry {
    pub title: String,
    /// 0 基页号。
    pub page: usize,
}

/// 取页预算；`read_page` 每调一次消耗一页。
pub struct PageBudget {
    max: u32,
    used: AtomicU32,
}

impl PageBudget {
    pub fn new(max: u32) -> Self {
        Self {
            max,
            used: AtomicU32::new(0),
        }
    }

    fn take(&self) -> Result<(), ToolError> {
        let used = self.used.fetch_add(1, Ordering::Relaxed);
        if used >= self.max {
            self.used.store(self.max, Ordering::Relaxed);
            return Err(ToolError::PageBudgetExhausted { max: self.max });
        }
        Ok(())
    }

    pub fn used(&self) -> u32 {
        self.used.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolError {
    /// 页码越界。
    PageOutOfRange { page: usize, pages: usize },
    /// 引擎取页失败（文件被移动、页面无文本层等）。
    PageUnavailable(String),
    /// 本次查询的取页预算用完了。
    PageBudgetExhausted { max: u32 },
    /// 在当前页找不到要查的词。
    TermNotFound(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PageOutOfRange { page, pages } => {
                write!(f, "第 {page} 页不存在（本书共 {pages} 页）")
            }
            Self::PageUnavailable(message) => write!(f, "无法读取页面：{message}"),
            Self::PageBudgetExhausted { max } => {
                write!(f, "本次查询最多再读 {max} 页，已经用完")
            }
            Self::TermNotFound(term) => write!(f, "当前页没有找到「{term}」"),
        }
    }
}

impl std::error::Error for ToolError {}

fn clip(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_PAGE_CHARS).collect();
    if out.chars().count() < text.chars().count() {
        out.push_str("\n…（文本已截断）");
    }
    out
}

// ---------- 工具一：文档信息 ----------

#[derive(Debug, Default, Deserialize)]
pub struct DocumentInfoArgs {}

pub struct DocumentInfoTool {
    context: Arc<dyn LookupContext>,
}

impl DocumentInfoTool {
    pub fn new(context: Arc<dyn LookupContext>) -> Self {
        Self { context }
    }
}

impl Tool for DocumentInfoTool {
    const NAME: &'static str = "document_info";
    type Args = DocumentInfoArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "当前 PDF 的文件名、总页数、元数据（标题/作者等）".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}, "required": []})
    }

    async fn call(&self, _context: &mut ToolContext, _args: Self::Args) -> Result<String, ToolError> {
        let info = self.context.document();
        let mut out = format!("文件：{}\n页数：{}\n", info.file_name, info.page_count);
        if info.metadata.is_empty() {
            out.push_str("元数据：无\n");
        } else {
            out.push_str("元数据：\n");
            for (key, value) in &info.metadata {
                out.push_str(&format!("- {key}：{value}\n"));
            }
        }
        Ok(out)
    }
}

// ---------- 工具二：当前页文本 ----------

pub struct CurrentPageTextTool {
    context: Arc<dyn LookupContext>,
}

impl CurrentPageTextTool {
    pub fn new(context: Arc<dyn LookupContext>) -> Self {
        Self { context }
    }
}

impl Tool for CurrentPageTextTool {
    const NAME: &'static str = "current_page_text";
    type Args = DocumentInfoArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "用户正在阅读的那一页的全文（可能被截断）".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}, "required": []})
    }

    async fn call(&self, _context: &mut ToolContext, _args: Self::Args) -> Result<String, ToolError> {
        let index = self.context.current_page();
        let text = self
            .context
            .page_text(index)
            .await
            .map_err(ToolError::PageUnavailable)?;
        let page = index + 1;
        if text.trim().is_empty() {
            return Ok(format!("第 {page} 页没有文本层（可能是扫描页）。"));
        }
        Ok(format!("第 {page} 页：\n{}", clip(&text)))
    }
}

// ---------- 工具三：词语上下文 ----------

#[derive(Debug, Deserialize)]
pub struct SurroundingTextArgs {
    /// 要定位的词。
    pub term: String,
    /// 前后各取多少字符；默认 100（合计 200）。
    #[serde(default)]
    pub radius: Option<u32>,
}

pub struct SurroundingTextTool {
    context: Arc<dyn LookupContext>,
}

impl SurroundingTextTool {
    pub fn new(context: Arc<dyn LookupContext>) -> Self {
        Self { context }
    }
}

impl Tool for SurroundingTextTool {
    const NAME: &'static str = "surrounding_text";
    type Args = SurroundingTextArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "在当前页里定位一个词，返回它前后的一段原文，用来判断上下文含义".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "term": {"type": "string", "description": "要定位的词"},
                "radius": {"type": "integer", "description": "前后各取多少字，默认 100"}
            },
            "required": ["term"]
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, ToolError> {
        let page_text = self
            .context
            .page_text(self.context.current_page())
            .await
            .map_err(ToolError::PageUnavailable)?;
        let radius = args.radius.unwrap_or(100).min(400) as usize;
        let snippet = surrounding(&page_text, &args.term, radius)
            .ok_or_else(|| ToolError::TermNotFound(args.term.clone()))?;
        Ok(snippet)
    }
}

/// 纯函数：在文本中定位 `term` 并取前后各 `radius` 个字符。
///
/// 按字符而不是字节切，避免把汉字切成半个；找不到返回 `None`。
pub fn surrounding(text: &str, term: &str, radius: usize) -> Option<String> {
    let haystack: Vec<char> = text.chars().collect();
    let needle: Vec<char> = term.chars().collect();
    if needle.is_empty() {
        return None;
    }
    let start = haystack
        .windows(needle.len())
        .position(|window| window == needle.as_slice())?;
    let from = start.saturating_sub(radius);
    let to = (start + needle.len() + radius).min(haystack.len());
    let mut snippet: String = haystack[from..to].iter().collect();
    if from > 0 {
        snippet.insert_str(0, "…");
    }
    if to < haystack.len() {
        snippet.push('…');
    }
    Some(snippet)
}

// ---------- 工具四：最近目录条目 ----------

pub struct TocNearestTool {
    context: Arc<dyn LookupContext>,
}

impl TocNearestTool {
    pub fn new(context: Arc<dyn LookupContext>) -> Self {
        Self { context }
    }
}

impl Tool for TocNearestTool {
    const NAME: &'static str = "toc_nearest";
    type Args = DocumentInfoArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "当前页所属的章节（取目录里页码不超过当前页的最后两条），用来获得书名/篇名语境".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}, "required": []})
    }

    async fn call(&self, _context: &mut ToolContext, _args: Self::Args) -> Result<String, ToolError> {
        let info = self.context.document();
        let page = self.context.current_page();
        if info.outline.is_empty() {
            return Ok("这本 PDF 没有目录。".to_string());
        }
        let mut preceding: Vec<&OutlineEntry> = info
            .outline
            .iter()
            .filter(|entry| entry.page <= page)
            .collect();
        if preceding.is_empty() {
            return Ok("当前页在第一个目录条目之前。".to_string());
        }
        let tail = preceding.split_off(preceding.len().saturating_sub(2));
        let mut out = String::new();
        for entry in tail {
            out.push_str(&format!(
                "{}（第 {} 页）\n",
                entry.title,
                entry.page + 1
            ));
        }
        Ok(out.trim_end().to_string())
    }
}

// ---------- 工具五：读任意页（受限） ----------

#[derive(Debug, Deserialize)]
pub struct ReadPageArgs {
    /// 1 基页码，和用户在界面上看到的一致。
    pub page: u32,
}

pub struct ReadPageTool {
    context: Arc<dyn LookupContext>,
    budget: Arc<PageBudget>,
}

impl ReadPageTool {
    pub fn new(context: Arc<dyn LookupContext>, budget: Arc<PageBudget>) -> Self {
        Self { context, budget }
    }
}

impl Tool for ReadPageTool {
    const NAME: &'static str = "read_page";
    type Args = ReadPageArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "读取指定页（1 基页码）的文本。只在当前页信息不足时使用，有次数上限".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "page": {"type": "integer", "description": "页码，从 1 开始"}
            },
            "required": ["page"]
        })
    }

    async fn call(&self, _context: &mut ToolContext, args: Self::Args) -> Result<String, ToolError> {
        let info = self.context.document();
        let page = args.page as usize;
        if page == 0 || page > info.page_count {
            return Err(ToolError::PageOutOfRange {
                page,
                pages: info.page_count,
            });
        }
        let index = page - 1;
        // 当前页本来就在读，不该消耗预算。
        if index != self.context.current_page() {
            self.budget.take()?;
        }
        let text = self
            .context
            .page_text(index)
            .await
            .map_err(ToolError::PageUnavailable)?;
        if text.trim().is_empty() {
            return Ok(format!("第 {page} 页没有文本层（可能是扫描页）。"));
        }
        Ok(format!("第 {page} 页：\n{}", clip(&text)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeContext {
        info: Arc<DocumentInfo>,
        page: usize,
        text: String,
    }

    impl LookupContext for FakeContext {
        fn document(&self) -> Arc<DocumentInfo> {
            self.info.clone()
        }
        fn current_page(&self) -> usize {
            self.page
        }
        fn page_text(
            &self,
            page: usize,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
            let text = if page == self.page {
                self.text.clone()
            } else {
                format!("第 {} 页的正文", page + 1)
            };
            Box::pin(async move { Ok(text) })
        }
    }

    fn fake(page_count: usize) -> Arc<FakeContext> {
        Arc::new(FakeContext {
            info: Arc::new(DocumentInfo {
                file_name: "资治通鉴.pdf".to_string(),
                page_count,
                metadata: vec![("Title".to_string(), "资治通鉴".to_string())],
                outline: vec![
                    OutlineEntry {
                        title: "卷一".to_string(),
                        page: 0,
                    },
                    OutlineEntry {
                        title: "卷二".to_string(),
                        page: 5,
                    },
                    OutlineEntry {
                        title: "卷三".to_string(),
                        page: 10,
                    },
                ],
            }),
            page: 5,
            text: "郑人游于乡校，以论执政。子产不毁乡校。".to_string(),
        })
    }

    #[test]
    fn surrounding_clips_on_char_boundaries() {
        let text = "郑人游于乡校，以论执政。";
        let snippet = surrounding(text, "乡校", 3).unwrap();
        assert!(snippet.contains("乡校"));
        assert!(snippet.starts_with('…'), "左侧被截断应加省略号：{snippet}");
        assert!(snippet.ends_with('…'), "右侧被截断也应加省略号：{snippet}");
    }

    #[test]
    fn surrounding_reports_missing_terms() {
        assert_eq!(surrounding("甲", "乙", 10), None);
        assert_eq!(surrounding("甲", "", 10), None);
    }

    #[test]
    fn budget_allows_only_the_configured_number_of_pages() {
        let budget = PageBudget::new(2);
        assert!(budget.take().is_ok());
        assert!(budget.take().is_ok());
        assert_eq!(
            budget.take().unwrap_err(),
            ToolError::PageBudgetExhausted { max: 2 }
        );
        assert_eq!(budget.used(), 2, "超限调用不能把计数推高");
    }

    #[tokio::test]
    async fn read_page_rejects_out_of_range_pages() {
        let ctx = fake(10);
        let tool = ReadPageTool::new(ctx, Arc::new(PageBudget::new(2)));
        let mut tool_ctx = ToolContext::default();
        let error = tool
            .call(&mut tool_ctx, ReadPageArgs { page: 99 })
            .await
            .unwrap_err();
        assert_eq!(
            error,
            ToolError::PageOutOfRange {
                page: 99,
                pages: 10
            }
        );
    }

    #[tokio::test]
    async fn reading_the_current_page_costs_no_budget() {
        let ctx = fake(10);
        let budget = Arc::new(PageBudget::new(0));
        let tool = ReadPageTool::new(ctx, budget.clone());
        let mut tool_ctx = ToolContext::default();
        // 当前页（0 基 5 → 1 基 6）已在内存里，预算为 0 也该成功。
        let out = tool.call(&mut tool_ctx, ReadPageArgs { page: 6 }).await.unwrap();
        assert!(out.contains("子产不毁乡校"));
        assert_eq!(budget.used(), 0);
    }

    #[tokio::test]
    async fn read_page_fetches_other_pages_through_the_context() {
        let ctx = fake(10);
        let tool = ReadPageTool::new(ctx, Arc::new(PageBudget::new(2)));
        let mut tool_ctx = ToolContext::default();
        let out = tool.call(&mut tool_ctx, ReadPageArgs { page: 3 }).await.unwrap();
        assert!(out.contains("第 3 页"), "{out}");
        assert!(out.contains("正文"), "{out}");
    }
}
