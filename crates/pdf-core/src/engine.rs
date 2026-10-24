//! PDF 引擎：把 PDFium 关在一个独立线程里，UI 线程只通过消息通信。
//!
//! PDFium 不是线程安全的，而且 `PdfDocument` 的生命周期绑在 `Pdfium` 上，
//! 所以这里把 `Pdfium` 泄漏成 `'static`，文档只存在于工作线程内部。
//!
//! 只有一条命令队列，但工作线程按优先级取活：位图和「打开文档」拿到就做，
//! 文字/链接提取、全文搜索这类慢任务排进本地待办，等位图安静下来才开工——
//! 读者在翻页时，时间应该花在让当前页出图上。
//!
//! 积压的请求也不一定还值得做：快速翻页时槽里大半瓦片已经滚出视口，渲完只会
//! 被 UI 丢掉。UI 每帧发布一份 [`Visibility`] 快照，工作线程动手之前先查一下，
//! 不在快照里的直接跳过并回一条 [`Event::TileSkipped`] / [`Event::ElementsSkipped`]，
//! 让 UI 把对应的「在跑」标记清掉，滚回来时能重新排队。

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures::channel::mpsc::UnboundedSender;
use pdfium_render::prelude::*;

use crate::model::{
    CharBox, DocumentMetadata, LinkBox, LinkTarget, OutlineItem, PageHits, PageSize, RectPt,
};
use crate::text_match::count_matches;

/// 一个瓦片在整个文档像素坐标系里的唯一标识：(页号, 整页像素坐标原点)。
///
/// 和调用方切瓦片用的键是同一个东西（见 `gloss_pdf::viewer::TileKey`），
/// 所以发布快照时不用做任何换算。
pub type TileId = (usize, i32, i32);

/// UI 线程每帧发布、工作线程只读的「现在真正需要什么」。
///
/// 快速翻页时请求槽里积压的大多是已经滚出视口的东西，渲完/提取完也只会被
/// UI 丢掉；有这份快照，工作线程在动手之前就能把它们整批作废，省下的时间
/// 正好留给屏幕上的那一页。
#[derive(Clone, Default)]
pub struct Visibility {
    inner: Arc<Mutex<Snapshot>>,
}

#[derive(Default)]
struct Snapshot {
    tiles: HashSet<TileId>,
    /// 允许提取文字/链接的页：可视范围再往外扩 `PREFETCH_PAGES` 页，和
    /// `ensure_visible_pages` 实际发请求的页完全一致。
    pages: HashSet<usize>,
}

impl Visibility {
    /// 用这一帧的可视范围替换上一帧的快照。
    pub fn publish(
        &self,
        tiles: impl IntoIterator<Item = TileId>,
        pages: impl IntoIterator<Item = usize>,
    ) {
        let mut snapshot = self.lock();
        snapshot.tiles.clear();
        snapshot.pages.clear();
        snapshot.tiles.extend(tiles);
        snapshot.pages.extend(pages);
    }

    fn wants_tile(&self, page: usize, origin_x: i32, origin_y: i32) -> bool {
        self.lock().tiles.contains(&(page, origin_x, origin_y))
    }

    fn wants_page(&self, page: usize) -> bool {
        self.lock().pages.contains(&page)
    }

    fn lock(&self) -> MutexGuard<'_, Snapshot> {
        // 持锁线程 panic 只会留下半帧快照：多做一个不影响正确性，少做的那个会
        // 收到「作废」回执、下一帧重新排队，所以中毒时直接取回数据。
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 发给 PDF 工作线程的命令。
pub enum Command {
    Open(PathBuf),
    /// 渲染一个页面瓦片。`origin_x`/`origin_y` 是瓦片左上角在整页像素坐标中的位置，
    /// 由调用方按固定瓦片尺寸切分；`width`/`height` 是瓦片位图尺寸。
    RenderTile {
        page: usize,
        scale: f32,
        origin_x: i32,
        origin_y: i32,
        width: u32,
        height: u32,
        request: u64,
    },
    /// 提取页面的文字与链接覆盖层（与缩放无关，每页只需一次）。
    PageElements {
        page: usize,
        request: u64,
    },
    /// 全文搜索：逐页提取文本统计命中数（文本会缓存，重复搜索只算一遍）。
    Search {
        query: String,
        request: u64,
    },
}

/// 工作线程回传给 UI 的事件。
pub enum Event {
    Opened {
        path: PathBuf,
        pages: Vec<PageSize>,
        outline: Vec<OutlineItem>,
        metadata: DocumentMetadata,
    },
    RenderedTile {
        page: usize,
        request: u64,
        origin_x: i32,
        origin_y: i32,
        width: u32,
        height: u32,
        /// BGRA 像素，直接对应 GPUI `RenderImage` 的缓冲区约定。
        bgra: Vec<u8>,
    },
    /// 一条积压的瓦片请求在渲染前就被判定为过期（这一瓦片已经滚出视口），
    /// 所以没有位图。UI 收到后清掉「在跑」标记即可，画面不用重绘。
    TileSkipped {
        page: usize,
        request: u64,
        origin_x: i32,
        origin_y: i32,
    },
    /// 页面文字与链接覆盖层；坐标一律是整页点坐标，与瓦片无关。
    PageElements {
        page: usize,
        request: u64,
        chars: Vec<CharBox>,
        links: Vec<LinkBox>,
    },
    /// 同 [`Event::TileSkipped`]，针对文字/链接提取：这一页已经滚出可视范围。
    ElementsSkipped {
        page: usize,
        request: u64,
    },
    /// 全文搜索结果：只包含有命中的页，`total` 是全文总命中数。
    SearchResults {
        request: u64,
        query: String,
        pages: Vec<PageHits>,
        total: usize,
    },
    /// 全文搜索的扫描进度；大文档第一次搜索要提取文本，界面据此显示进度。
    SearchProgress {
        request: u64,
        scanned: usize,
        total: usize,
    },
    Error(String),
}

/// 引擎句柄：持有命令通道与可视快照，worker 在后台线程跑。
#[derive(Clone)]
pub struct PdfEngine {
    pub commands: Sender<Command>,
    /// UI 每帧发布的可视范围；工作线程据此把积压的过期请求整批作废。
    pub visibility: Visibility,
}

impl PdfEngine {
    pub fn spawn(events: UnboundedSender<Event>) -> Self {
        let (commands, receiver) = mpsc::channel();
        let visibility = Visibility::default();
        let worker_visibility = visibility.clone();
        std::thread::Builder::new()
            .name("pdfium-worker".into())
            .spawn(move || worker_loop(receiver, worker_visibility, events))
            .expect("failed to spawn pdf worker");
        Self {
            commands,
            visibility,
        }
    }

    /// 发送一条命令；引擎线程已经退出时把命令原样退回。
    pub fn send(&self, command: Command) -> Result<(), mpsc::SendError<Command>> {
        self.commands.send(command)
    }
}

/// 位图安静多久之后，才允许文字提取/全文搜索这类慢任务开工。
///
/// 快速翻页时每一帧都有新瓦片，慢任务就一直往后排；手一停下来，30 ms 之后
/// 它们立刻轮到，所以不会饿死。
const SLOW_JOB_SETTLE: Duration = Duration::from_millis(30);

/// 本地待办：把「位图优先、慢任务等空档」的规则独立出来，便于单测。
#[derive(Default)]
struct Backlog {
    slow: VecDeque<Command>,
}

impl Backlog {
    /// 收下一条命令；要立刻执行就把命令还回去。
    ///
    /// 换文档时顺带丢掉旧文档排着的慢任务——那些页在新文档里没有意义。
    fn accept(&mut self, command: Command) -> Option<Command> {
        match command {
            Command::RenderTile { .. } => Some(command),
            Command::Open(_) => {
                self.slow.clear();
                Some(command)
            }
            _ => {
                self.slow.push_back(command);
                None
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.slow.is_empty()
    }

    fn next_slow(&mut self) -> Option<Command> {
        self.slow.pop_front()
    }
}

/// 工作线程的全部私有状态：PDFium 与文档都只活在这里。
struct Worker {
    pdfium: Option<&'static Pdfium>,
    document: Option<PdfDocument<'static>>,
    /// 每页的纯文本，全文搜索用；换文档时清空。
    page_text: Vec<Option<String>>,
    /// 每页的文字/链接覆盖层，与缩放无关；换文档时清空。
    page_elements: Vec<Option<(Vec<CharBox>, Vec<LinkBox>)>>,
    /// UI 发布的可视范围。
    wanted: Visibility,
    events: UnboundedSender<Event>,
}

impl Worker {
    fn new(wanted: Visibility, events: UnboundedSender<Event>) -> Self {
        Self {
            pdfium: None,
            document: None,
            page_text: Vec::new(),
            page_elements: Vec::new(),
            wanted,
            events,
        }
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::Open(path) => self.run_open(&path),
            Command::RenderTile {
                page,
                scale,
                origin_x,
                origin_y,
                width,
                height,
                request,
            } => self.run_tile(page, scale, origin_x, origin_y, width, height, request),
            Command::PageElements { page, request } => self.run_elements(page, request),
            Command::Search { query, request } => self.run_search(&query, request),
        }
    }

    fn report(&self, message: String) {
        let _ = self.events.unbounded_send(Event::Error(message));
    }

    fn run_open(&mut self, path: &Path) {
        if self.pdfium.is_none() {
            match bind_pdfium() {
                Ok(instance) => self.pdfium = Some(instance),
                Err(error) => {
                    self.report(format!("加载 PDFium 失败: {error:#}"));
                    return;
                }
            }
        }
        let instance = self.pdfium.expect("pdfium bound above");
        match open_document(instance, path) {
            Ok((opened, pages, outline, metadata)) => {
                self.document = Some(opened);
                self.page_text.clear();
                self.page_elements.clear();
                let _ = self.events.unbounded_send(Event::Opened {
                    path: path.to_path_buf(),
                    pages,
                    outline,
                    metadata,
                });
            }
            Err(error) => self.report(format!("打开 PDF 失败: {error:#}")),
        }
    }

    /// 渲染一个瓦片；已经不在可视快照里的请求直接作废。
    fn run_tile(
        &mut self,
        page: usize,
        scale: f32,
        origin_x: i32,
        origin_y: i32,
        width: u32,
        height: u32,
        request: u64,
    ) {
        if !self.wanted.wants_tile(page, origin_x, origin_y) {
            log::debug!("丢弃过期瓦片 p{page} ({origin_x},{origin_y})");
            let _ = self.events.unbounded_send(Event::TileSkipped {
                page,
                request,
                origin_x,
                origin_y,
            });
            return;
        }
        let Some(document) = self.document.as_ref() else {
            return;
        };
        match render_tile(
            document, page, scale, origin_x, origin_y, width, height, request,
        ) {
            Ok(event) => {
                let _ = self.events.unbounded_send(event);
            }
            Err(error) => self.report(format!("渲染页面失败: {error:#}")),
        }
    }

    /// 提取一页的文字/链接；页已经滚出可视范围就作废，别让过期提取拖住当前页。
    fn run_elements(&mut self, page: usize, request: u64) {
        if !self.wanted.wants_page(page) {
            log::debug!("丢弃过期覆盖层 p{page}");
            let _ = self
                .events
                .unbounded_send(Event::ElementsSkipped { page, request });
            return;
        }
        let elements = match self.page_elements.get(page).cloned().flatten() {
            Some(elements) => elements,
            None => {
                let Some(document) = self.document.as_ref() else {
                    return;
                };
                match extract_elements(document, page) {
                    Ok(elements) => {
                        if self.page_elements.len() <= page {
                            self.page_elements.resize_with(page + 1, || None);
                        }
                        self.page_elements[page] = Some(elements.clone());
                        elements
                    }
                    Err(error) => {
                        self.report(format!("提取页面内容失败: {error:#}"));
                        return;
                    }
                }
            }
        };
        let _ = self.events.unbounded_send(Event::PageElements {
            page,
            request,
            chars: elements.0,
            links: elements.1,
        });
    }

    fn run_search(&mut self, query: &str, request: u64) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let event = search_document(document, &mut self.page_text, query, request, &self.events);
        let _ = self.events.unbounded_send(event);
    }
}

fn worker_loop(
    commands: Receiver<Command>,
    visibility: Visibility,
    events: UnboundedSender<Event>,
) {
    let mut worker = Worker::new(visibility, events);
    let mut backlog = Backlog::default();

    loop {
        // 先把槽里排着的一口气收下来：位图和「打开文档」当场做，慢任务进待办。
        // 待办只在下面「安静窗口」那一步被取走，所以排着的位图永远先于慢任务。
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    if let Some(command) = backlog.accept(command) {
                        worker.run(command);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if backlog.is_empty() {
            // 没有慢任务：阻塞等下一条命令，空闲时不空转。
            match commands.recv() {
                Ok(command) => {
                    if let Some(command) = backlog.accept(command) {
                        worker.run(command);
                    }
                }
                Err(_) => return,
            }
            continue;
        }

        // 有慢任务：只等一个短窗口。位图还在来就继续渲位图（读者还在翻页），
        // 安静够久才开工，而且一条一条来——每条之前都会重新看一眼有没有新位图。
        match commands.recv_timeout(SLOW_JOB_SETTLE) {
            Ok(command) => {
                if let Some(command) = backlog.accept(command) {
                    worker.run(command);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(command) = backlog.next_slow() {
                    worker.run(command);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// 每扫描多少页回一次进度。取 8 是为了让进度条在大文档上够跟手，
/// 又不至于每页都往 UI 线程塞一条消息。
const SEARCH_PROGRESS_PAGES: usize = 8;

/// 逐页统计命中数。文本按页缓存，第二遍搜索只做字符串比较。
fn search_document(
    document: &PdfDocument<'static>,
    cache: &mut Vec<Option<String>>,
    query: &str,
    request: u64,
    events: &UnboundedSender<Event>,
) -> Event {
    let needle = query.to_lowercase();
    let mut pages = Vec::new();
    let mut total = 0;

    if !needle.is_empty() {
        let page_count = document.pages().len() as usize;
        // 先报 0/N：界面立刻从「搜索中…」切成具体进度。
        let _ = events.unbounded_send(Event::SearchProgress {
            request,
            scanned: 0,
            total: page_count,
        });

        for index in 0..page_count {
            if cache.len() <= index {
                cache.resize(index + 1, None);
            }
            let text = match &cache[index] {
                Some(text) => text.clone(),
                None => {
                    let extracted = match document.pages().get(index as PdfPageIndex) {
                        Ok(page) => match page.text() {
                            Ok(text) => text.all(),
                            Err(_) => String::new(),
                        },
                        Err(_) => String::new(),
                    };
                    cache[index] = Some(extracted.clone());
                    extracted
                }
            };

            let count = count_matches(&text.to_lowercase(), &needle);
            if count > 0 {
                total += count;
                pages.push(PageHits { page: index, count });
            }

            let scanned = index + 1;
            if scanned % SEARCH_PROGRESS_PAGES == 0 || scanned == page_count {
                let _ = events.unbounded_send(Event::SearchProgress {
                    request,
                    scanned,
                    total: page_count,
                });
            }
        }
    }

    Event::SearchResults {
        request,
        query: query.to_string(),
        pages,
        total,
    }
}

fn bind_pdfium() -> Result<&'static Pdfium> {
    let mut directories = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        directories.push(parent.to_path_buf());
        // macOS app bundle: Contents/MacOS -> Contents/Frameworks
        #[cfg(target_os = "macos")]
        {
            let frameworks = parent.join("../Frameworks");
            if frameworks.is_dir() {
                directories.push(frameworks);
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        directories.push(cwd);
    }

    // 常见包管理器或系统库路径（如 macOS Homebrew）
    #[cfg(target_os = "macos")]
    {
        directories.push(std::path::PathBuf::from("/opt/homebrew/lib"));
        directories.push(std::path::PathBuf::from("/usr/local/lib"));
    }
    #[cfg(target_os = "linux")]
    {
        directories.push(std::path::PathBuf::from("/usr/local/lib"));
        directories.push(std::path::PathBuf::from("/usr/lib"));
    }

    let mut last_error: Option<anyhow::Error> = None;
    for directory in directories {
        let library = Pdfium::pdfium_platform_library_name_at_path(&directory);
        if !library.exists() {
            continue;
        }
        match Pdfium::bind_to_library(&library) {
            Ok(bindings) => {
                log::info!("pdfium bound from {}", library.display());
                return Ok(Box::leak(Box::new(Pdfium::new(bindings))));
            }
            Err(error) => {
                last_error = Some(anyhow!(error));
            }
        }
    }

    let lib_name = if cfg!(target_os = "windows") {
        "pdfium.dll"
    } else if cfg!(target_os = "macos") {
        "libpdfium.dylib"
    } else {
        "libpdfium.so"
    };

    match Pdfium::bind_to_system_library() {
        Ok(bindings) => Ok(Box::leak(Box::new(Pdfium::new(bindings)))),
        Err(error) => Err(last_error.unwrap_or_else(|| anyhow!(error)))
            .with_context(|| format!("找不到 {lib_name}（请放到可执行文件目录、项目根目录，或通过系统包管理器安装）")),
    }
}

fn open_document(
    pdfium: &'static Pdfium,
    path: &Path,
) -> Result<(
    PdfDocument<'static>,
    Vec<PageSize>,
    Vec<OutlineItem>,
    DocumentMetadata,
)> {
    let document = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|error| anyhow!(error))
        .with_context(|| format!("无法读取 {}", path.display()))?;

    let mut pages = Vec::new();
    for page in document.pages().iter() {
        pages.push(PageSize {
            width: page.width().value,
            height: page.height().value,
        });
    }

    let mut outline = Vec::new();
    for bookmark in document.bookmarks().iter() {
        collect_bookmark(&bookmark, 0, &mut outline);
    }

    let metadata = DocumentMetadata {
        title: metadata_value(&document, PdfDocumentMetadataTagType::Title),
        author: metadata_value(&document, PdfDocumentMetadataTagType::Author),
        subject: metadata_value(&document, PdfDocumentMetadataTagType::Subject),
        keywords: metadata_value(&document, PdfDocumentMetadataTagType::Keywords),
        creator: metadata_value(&document, PdfDocumentMetadataTagType::Creator),
        producer: metadata_value(&document, PdfDocumentMetadataTagType::Producer),
        creation_date: metadata_value(&document, PdfDocumentMetadataTagType::CreationDate),
        // pdfium-render 0.9.4 查错了键名（用的是 ModificationDate），标准键是 ModDate；
        // 先走高层 API（将来修复后自动生效），失败再从原始字节补读。
        modification_date: metadata_value(&document, PdfDocumentMetadataTagType::ModificationDate)
            .or_else(|| crate::metadata::read_modification_date(path)),
    };

    Ok((document, pages, outline, metadata))
}

/// 读取一个元数据标签，去掉首尾空白；空字符串按缺失处理。
fn metadata_value(
    document: &PdfDocument<'static>,
    tag: PdfDocumentMetadataTagType,
) -> Option<String> {
    document
        .metadata()
        .get(tag)
        .map(|tag| tag.value().trim().to_string())
        .filter(|value| !value.is_empty())
}

fn collect_bookmark(bookmark: &PdfBookmark, depth: usize, output: &mut Vec<OutlineItem>) {
    let title = bookmark.title().unwrap_or_else(|| "（无标题）".to_string());
    let page = bookmark
        .destination()
        .and_then(|destination| destination.page_index().ok())
        .map(|index| index as usize)
        .unwrap_or(0);
    output.push(OutlineItem { title, page, depth });

    for child in bookmark.iter_direct_children() {
        collect_bookmark(&child, depth + 1, output);
    }
}

fn render_tile(
    document: &PdfDocument<'static>,
    page_index: usize,
    scale: f32,
    origin_x: i32,
    origin_y: i32,
    width: u32,
    height: u32,
    request: u64,
) -> Result<Event> {
    let page = document
        .pages()
        .get(page_index as PdfPageIndex)
        .map_err(|error| anyhow!(error))?;

    let full_width = (page.width().value * scale).round().max(1.0) as i32;
    let full_height = (page.height().value * scale).round().max(1.0) as i32;
    let width = width.max(1) as i32;
    let height = height.max(1) as i32;
    // 页面的逻辑尺寸仍是整页，负的 origin 把目标区域平移到瓦片上；
    // 这样表单、注释和文字坐标都保持整页坐标系，和覆盖层完全对齐。
    let config = PdfRenderConfig::new()
        .set_fixed_size(full_width, full_height)
        .set_origin(-origin_x, -origin_y)
        .render_annotations(true)
        .render_form_data(true)
        // GPUI 的 RenderImage 缓冲区是 BGRA；不要让 Pdfium 把字节序反转成 RGBA。
        .set_reverse_byte_order(false);
    let mut bitmap =
        PdfBitmap::empty(width, height, PdfBitmapFormat::BGRA).map_err(|error| anyhow!(error))?;
    page.render_into_bitmap_with_config(&mut bitmap, &config)
        .map_err(|error| anyhow!(error))?;
    log::debug!(
        "page {} tile ({origin_x},{origin_y}) {}x{}",
        page_index,
        width,
        height
    );

    Ok(Event::RenderedTile {
        page: page_index,
        request,
        origin_x,
        origin_y,
        width: width as u32,
        height: height as u32,
        bgra: bitmap.as_raw_bytes(),
    })
}

/// 提取整页的字符与链接。两者都是 PDF 点坐标，可直接用于整页覆盖层。
fn extract_elements(
    document: &PdfDocument<'static>,
    page_index: usize,
) -> Result<(Vec<CharBox>, Vec<LinkBox>)> {
    let page = document
        .pages()
        .get(page_index as PdfPageIndex)
        .map_err(|error| anyhow!(error))?;

    let mut chars = Vec::new();
    if let Ok(text) = page.text() {
        for character in text.chars().iter() {
            let Some(ch) = character.unicode_char() else {
                continue;
            };
            let Ok(bounds) = character.tight_bounds() else {
                continue;
            };
            chars.push(CharBox {
                ch,
                rect: rect_from_pdfium(bounds),
            });
        }
    }

    let mut links = Vec::new();
    for link in page.links().iter() {
        let Ok(rect) = link.rect() else {
            continue;
        };
        let target = match link.action() {
            Some(PdfAction::Uri(uri)) => uri.uri().ok().map(LinkTarget::Uri),
            Some(PdfAction::LocalDestination(destination)) => destination
                .destination()
                .ok()
                .and_then(|target| target.page_index().ok())
                .map(|index| LinkTarget::Page(index as usize)),
            _ => link
                .destination()
                .and_then(|destination| destination.page_index().ok())
                .map(|index| LinkTarget::Page(index as usize)),
        };
        if let Some(target) = target {
            links.push(LinkBox {
                rect: rect_from_pdfium(rect),
                target,
            });
        }
    }

    Ok((chars, links))
}

fn rect_from_pdfium(rect: PdfRect) -> RectPt {
    RectPt {
        left: rect.left().value,
        bottom: rect.bottom().value,
        right: rect.right().value,
        top: rect.top().value,
    }
}

#[cfg(test)]
mod tests {
    use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};

    use super::*;

    fn tile_request(page: usize, origin_x: i32, origin_y: i32) -> Command {
        Command::RenderTile {
            page,
            scale: 1.0,
            origin_x,
            origin_y,
            width: 512,
            height: 512,
            request: page as u64,
        }
    }

    fn element_request(page: usize) -> Command {
        Command::PageElements {
            page,
            request: page as u64,
        }
    }

    fn search_request() -> Command {
        Command::Search {
            query: "笺释".into(),
            request: 9,
        }
    }

    /// 等一条引擎事件。事件在毫秒级回来；2 秒还没到就是调度坏了，直接失败而不是挂住测试。
    fn next_event(rx: &mut UnboundedReceiver<Event>) -> Event {
        for _ in 0..2000 {
            match rx.try_recv() {
                Ok(event) => return event,
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(1)),
                Err(TryRecvError::Closed) => break,
            }
        }
        panic!("等待引擎事件超时");
    }

    #[test]
    fn visibility_snapshot_replaces_the_previous_frame() {
        let visibility = Visibility::default();
        assert!(!visibility.wants_tile(0, 0, 0), "还没发布时什么都不需要");
        assert!(!visibility.wants_page(0));

        visibility.publish([(0, 0, 0), (0, 512, 0)], [0, 1]);
        assert!(visibility.wants_tile(0, 0, 0));
        assert!(visibility.wants_tile(0, 512, 0));
        assert!(!visibility.wants_tile(0, 1024, 0), "没发布的瓦片不要");
        assert!(visibility.wants_page(1));

        // 翻到下一页：上一帧的瓦片整批作废，页范围也跟着换。
        visibility.publish([(1, 0, 0)], [1]);
        assert!(!visibility.wants_tile(0, 0, 0));
        assert!(visibility.wants_tile(1, 0, 0));
        assert!(!visibility.wants_page(0));
        assert!(visibility.wants_page(1));
    }

    #[test]
    fn empty_snapshot_wants_nothing() {
        let visibility = Visibility::default();
        visibility.publish([], []);
        assert!(!visibility.wants_tile(0, 0, 0));
        assert!(!visibility.wants_page(0));
    }

    #[test]
    fn tiles_and_opens_run_immediately() {
        let mut backlog = Backlog::default();
        assert!(backlog.accept(tile_request(0, 0, 0)).is_some());
        assert!(backlog.is_empty(), "位图不该进待办");
        assert!(backlog.accept(Command::Open(PathBuf::from("a.pdf"))).is_some());
    }

    #[test]
    fn slow_jobs_wait_for_a_gap_and_keep_their_order() {
        let mut backlog = Backlog::default();
        assert!(backlog.accept(element_request(0)).is_none(), "慢任务该先进待办");
        assert!(backlog.accept(search_request()).is_none());
        assert!(!backlog.is_empty());

        // 先进先出：元素提取排在搜索前面，和 UI 发出请求的顺序一致。
        assert!(matches!(
            backlog.next_slow(),
            Some(Command::PageElements { page: 0, .. })
        ));
        assert!(matches!(backlog.next_slow(), Some(Command::Search { .. })));
        assert!(backlog.next_slow().is_none());
    }

    #[test]
    fn opening_a_document_drops_stale_slow_jobs() {
        let mut backlog = Backlog::default();
        backlog.accept(element_request(3));
        backlog.accept(search_request());

        assert!(backlog.accept(Command::Open(PathBuf::from("b.pdf"))).is_some());
        assert!(backlog.is_empty(), "换文档后旧文档的慢任务都该作废");
    }

    #[test]
    fn tiles_outside_the_snapshot_come_back_as_skipped() {
        let (events, mut rx) = futures::channel::mpsc::unbounded();
        let engine = PdfEngine::spawn(events);
        engine.visibility.publish([(0, 0, 0)], [0]);

        // 还在视口里的瓦片：没有文档可渲，但也不该被当成过期请求回执。
        engine.send(tile_request(0, 0, 0)).unwrap();
        // 已经滚出视口（快照里没有它）：立即作废。
        engine.send(tile_request(0, 512, 0)).unwrap();

        let event = next_event(&mut rx);
        assert!(
            matches!(
                event,
                Event::TileSkipped {
                    page: 0,
                    origin_x: 512,
                    origin_y: 0,
                    ..
                }
            ),
            "只有快照外的瓦片该被作废"
        );
    }

    #[test]
    fn elements_outside_the_visible_pages_come_back_as_skipped() {
        let (events, mut rx) = futures::channel::mpsc::unbounded();
        let engine = PdfEngine::spawn(events);
        engine.visibility.publish([], [2]);

        engine.send(Command::PageElements {
            page: 5,
            request: 11,
        })
        .unwrap();

        let event = next_event(&mut rx);
        assert!(matches!(
            event,
            Event::ElementsSkipped {
                page: 5,
                request: 11
            }
        ));
    }

    #[test]
    fn counts_every_non_overlapping_hit() {
        assert_eq!(count_matches("hello world", "o"), 2);
        assert_eq!(count_matches("aaaa", "aa"), 2);
        assert_eq!(count_matches("汉字里找汉字", "汉字"), 2);
        assert_eq!(count_matches("abc", "x"), 0);
        assert_eq!(count_matches("abc", ""), 0);
    }

    #[test]
    fn counts_chinese_hits_across_spaces() {
        assert_eq!(count_matches("汉 字 与 汉 字", "汉字"), 2);
    }
}
