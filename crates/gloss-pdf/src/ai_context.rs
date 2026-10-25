//! 查词那一刻的文档快照 + 取页通道。
//!
//! 工具跑在后台 executor 上，碰不到 `AppState`，所以对话开始前先把「不会变的」
//! 拍成快照（文件名、页数、元数据、目录、当前页），需要额外页时再通过通道向
//! 引擎要——请求由 `AppState` 转成 `Command::PageText`，事件回来时把结果塞回
//! oneshot。
//!
//! 快照会「过期」：一轮对话里用户翻页了，工具看到的仍是查词那一刻的位置。
//! 这是刻意的取舍——比让工具实时读 UI 状态简单，也更符合「就这段文本提问」的直觉。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ai_core::{DocumentInfo, LookupContext, OutlineEntry};
use futures::channel::{mpsc, oneshot};

use crate::app::AppState;
use crate::viewer::Document;

/// 一次取页请求；`AppState` 消费后把结果写回 `reply`。
pub(crate) struct PageTextRequest {
    pub page: usize,
    pub reply: oneshot::Sender<Result<String, String>>,
}

/// 工具侧持有的取页句柄。
#[derive(Clone)]
pub(crate) struct PageReader {
    sender: mpsc::UnboundedSender<PageTextRequest>,
}

impl PageReader {
    pub fn new(sender: mpsc::UnboundedSender<PageTextRequest>) -> Self {
        Self { sender }
    }

    fn request(&self, page: usize) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        let (reply, receiver) = oneshot::channel();
        let queued = self
            .sender
            .unbounded_send(PageTextRequest { page, reply })
            .is_ok();
        Box::pin(async move {
            if !queued {
                return Err("文档已关闭".to_string());
            }
            receiver.await.map_err(|_| "文档已关闭".to_string())?
        })
    }
}

/// 一轮对话期间的文档快照。
pub(crate) struct LookupSnapshot {
    info: Arc<DocumentInfo>,
    page: usize,
    reader: PageReader,
}

impl LookupSnapshot {
    pub fn new(info: Arc<DocumentInfo>, page: usize, reader: PageReader) -> Self {
        Self { info, page, reader }
    }

    /// 从打开的文档拍一份元数据快照；只保留有值的字段，避免把一串空标签发给模型。
    pub fn info_from(document: &Document) -> DocumentInfo {
        let file_name = document
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| document.path.display().to_string());

        let metadata = [
            ("标题", &document.metadata.title),
            ("作者", &document.metadata.author),
            ("主题", &document.metadata.subject),
            ("关键词", &document.metadata.keywords),
            ("创建工具", &document.metadata.creator),
        ]
        .into_iter()
        .filter_map(|(label, value)| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| (label.to_string(), text.to_string()))
        })
        .collect();

        DocumentInfo {
            file_name,
            page_count: document.pages.len(),
            metadata,
            outline: document
                .outline
                .iter()
                .map(|item| OutlineEntry {
                    title: item.title.clone(),
                    page: item.page,
                })
                .collect(),
        }
    }
}

impl LookupContext for LookupSnapshot {
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
        self.reader.request(page)
    }
}

impl AppState {
    /// 工具请求一页文本：登记回包槽位，再让引擎去取。
    pub(crate) fn begin_page_read(&mut self, request: PageTextRequest) {
        self.page_text_request_id = self.page_text_request_id.wrapping_add(1);
        let id = self.page_text_request_id;
        self.pending_page_reads.insert(id, request.reply);
        if self
            .engine
            .commands
            .send(pdf_core::Command::PageText {
                page: request.page,
                request: id,
            })
            .is_err()
        {
            // 引擎已经退出：立刻回错，别让工具永远等着。
            self.complete_page_read(id, Err("PDF 引擎已停止".to_string()));
        }
    }

    /// 引擎回包：把文本交给等待中的工具 future。
    pub(crate) fn complete_page_read(&mut self, request: u64, result: Result<String, String>) {
        if let Some(reply) = self.pending_page_reads.remove(&request) {
            let _ = reply.send(result);
        }
    }

    /// 拍一份当前文档的快照给 AI 工具；没有打开文档时返回 `None`。
    pub(crate) fn lookup_snapshot(&self) -> Option<Arc<LookupSnapshot>> {
        let document = self.viewer.doc.as_ref()?;
        let info = Arc::new(LookupSnapshot::info_from(document));
        Some(Arc::new(LookupSnapshot::new(
            info,
            self.viewer.current_page,
            PageReader::new(self.page_text_requests.clone()),
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pdf_core::{DocumentMetadata, OutlineItem, PageSize};

    use super::*;

    fn sample_document() -> Document {
        Document {
            path: PathBuf::from("C:/books/资治通鉴.pdf"),
            pages: vec![PageSize::default(); 12],
            outline: vec![OutlineItem {
                title: "卷一".to_string(),
                page: 0,
                depth: 0,
            }],
            metadata: DocumentMetadata {
                title: Some("资治通鉴".to_string()),
                author: Some("   ".to_string()),
                ..DocumentMetadata::default()
            },
        }
    }

    #[test]
    fn info_keeps_file_name_and_drops_blank_metadata() {
        let info = LookupSnapshot::info_from(&sample_document());
        assert_eq!(info.file_name, "资治通鉴.pdf");
        assert_eq!(info.page_count, 12);
        assert_eq!(
            info.metadata,
            vec![("标题".to_string(), "资治通鉴".to_string())],
            "空白字段不该出现在提示里"
        );
        assert_eq!(info.outline.len(), 1);
        assert_eq!(info.outline[0].page, 0);
    }

    #[tokio::test]
    async fn page_requests_round_trip_through_the_channel() {
        let (tx, mut rx) = mpsc::unbounded::<PageTextRequest>();
        let reader = PageReader::new(tx);
        let snapshot = LookupSnapshot::new(
            Arc::new(LookupSnapshot::info_from(&sample_document())),
            2,
            reader,
        );

        let pending = snapshot.page_text(5);
        let request = rx.try_recv().expect("应有一次取页请求");
        assert_eq!(request.page, 5);
        request.reply.send(Ok("第五页正文".to_string())).unwrap();

        assert_eq!(pending.await.unwrap(), "第五页正文");
        assert_eq!(snapshot.current_page(), 2);
    }

    #[tokio::test]
    async fn dropped_channel_reports_closed_document() {
        let (tx, rx) = mpsc::unbounded::<PageTextRequest>();
        drop(rx);
        let reader = PageReader::new(tx);
        let error = reader.request(0).await.unwrap_err();
        assert_eq!(error, "文档已关闭");
    }
}
