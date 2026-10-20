//! PDF 引擎：把 PDFium 关在一个独立线程里，UI 线程只通过消息通信。
//!
//! PDFium 不是线程安全的，而且 `PdfDocument` 的生命周期绑在 `Pdfium` 上，
//! 所以这里把 `Pdfium` 泄漏成 `'static`，文档只存在于工作线程内部。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::{Context, Result, anyhow};
use futures::channel::mpsc::UnboundedSender;
use pdfium_render::prelude::*;

use crate::model::{
    CharBox, DocumentMetadata, LinkBox, LinkTarget, OutlineItem, PageHits, PageSize, RectPt,
};
use crate::text_match::count_matches;

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
    /// 页面文字与链接覆盖层；坐标一律是整页点坐标，与瓦片无关。
    PageElements {
        page: usize,
        request: u64,
        chars: Vec<CharBox>,
        links: Vec<LinkBox>,
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

/// 引擎句柄：持有命令通道，worker 在后台线程跑。
pub struct PdfEngine {
    pub commands: Sender<Command>,
}

impl PdfEngine {
    pub fn spawn(events: UnboundedSender<Event>) -> Self {
        let (commands, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("pdfium-worker".into())
            .spawn(move || worker_loop(receiver, events))
            .expect("failed to spawn pdf worker");
        Self { commands }
    }
}

fn worker_loop(commands: Receiver<Command>, events: UnboundedSender<Event>) {
    let mut pdfium: Option<&'static Pdfium> = None;
    let mut document: Option<PdfDocument<'static>> = None;
    // 每页的纯文本，全文搜索用；换文档时清空。
    let mut page_text: Vec<Option<String>> = Vec::new();
    // 每页的文字/链接覆盖层，与缩放无关；换文档时清空。
    let mut page_elements: Vec<Option<(Vec<CharBox>, Vec<LinkBox>)>> = Vec::new();

    while let Ok(command) = commands.recv() {
        match command {
            Command::Open(path) => {
                if pdfium.is_none() {
                    match bind_pdfium() {
                        Ok(instance) => pdfium = Some(instance),
                        Err(error) => {
                            let _ = events.unbounded_send(Event::Error(format!(
                                "加载 PDFium 失败: {error:#}"
                            )));
                            continue;
                        }
                    }
                }
                let instance = pdfium.expect("pdfium bound above");
                match open_document(instance, &path) {
                    Ok((opened, pages, outline, metadata)) => {
                        document = Some(opened);
                        page_text.clear();
                        page_elements.clear();
                        let _ = events.unbounded_send(Event::Opened {
                            path,
                            pages,
                            outline,
                            metadata,
                        });
                    }
                    Err(error) => {
                        let _ = events
                            .unbounded_send(Event::Error(format!("打开 PDF 失败: {error:#}")));
                    }
                }
            }
            Command::RenderTile {
                page,
                scale,
                origin_x,
                origin_y,
                width,
                height,
                request,
            } => {
                let Some(document) = document.as_ref() else {
                    continue;
                };
                match render_tile(
                    document, page, scale, origin_x, origin_y, width, height, request,
                ) {
                    Ok(event) => {
                        let _ = events.unbounded_send(event);
                    }
                    Err(error) => {
                        let _ =
                            events.unbounded_send(Event::Error(format!("渲染页面失败: {error:#}")));
                    }
                }
            }
            Command::PageElements { page, request } => {
                let Some(document) = document.as_ref() else {
                    continue;
                };
                if page_elements.len() <= page {
                    page_elements.resize_with(page + 1, || None);
                }
                let elements = match &page_elements[page] {
                    Some(elements) => elements.clone(),
                    None => match extract_elements(document, page) {
                        Ok(elements) => {
                            page_elements[page] = Some(elements.clone());
                            elements
                        }
                        Err(error) => {
                            let _ = events.unbounded_send(Event::Error(format!(
                                "提取页面内容失败: {error:#}"
                            )));
                            continue;
                        }
                    },
                };
                let _ = events.unbounded_send(Event::PageElements {
                    page,
                    request,
                    chars: elements.0,
                    links: elements.1,
                });
            }
            Command::Search { query, request } => {
                let Some(document) = document.as_ref() else {
                    continue;
                };
                let result = search_document(document, &mut page_text, &query, request, &events);
                let _ = events.unbounded_send(result);
            }
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
    use super::count_matches;

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
