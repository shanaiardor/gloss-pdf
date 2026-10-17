//! PDF 引擎：把 PDFium 关在一个独立线程里，UI 线程只通过消息通信。
//!
//! PDFium 不是线程安全的，而且 `PdfDocument` 的生命周期绑在 `Pdfium` 上，
//! 所以这里把 `Pdfium` 泄漏成 `'static`，文档只存在于工作线程内部。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::{anyhow, Context, Result};
use futures::channel::mpsc::UnboundedSender;
use pdfium_render::prelude::*;

use crate::model::{CharBox, LinkBox, LinkTarget, OutlineItem, PageSize, RectPt};

/// 发给 PDF 工作线程的命令。
pub enum Command {
    Open(PathBuf),
    Render {
        page: usize,
        scale: f32,
        request: u64,
    },
}

/// 工作线程回传给 UI 的事件。
pub enum Event {
    Opened {
        path: PathBuf,
        pages: Vec<PageSize>,
        outline: Vec<OutlineItem>,
    },
    Rendered {
        page: usize,
        request: u64,
        width: u32,
        height: u32,
        /// BGRA 像素，直接对应 GPUI `RenderImage` 的缓冲区约定。
        bgra: Vec<u8>,
        chars: Vec<CharBox>,
        links: Vec<LinkBox>,
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

    while let Ok(command) = commands.recv() {
        match command {
            Command::Open(path) => {
                if pdfium.is_none() {
                    match bind_pdfium() {
                        Ok(instance) => pdfium = Some(instance),
                        Err(error) => {
                            let _ = events
                                .unbounded_send(Event::Error(format!("加载 PDFium 失败: {error:#}")));
                            continue;
                        }
                    }
                }
                let instance = pdfium.expect("pdfium bound above");
                match open_document(instance, &path) {
                    Ok((opened, pages, outline)) => {
                        document = Some(opened);
                        let _ = events.unbounded_send(Event::Opened { path, pages, outline });
                    }
                    Err(error) => {
                        let _ =
                            events.unbounded_send(Event::Error(format!("打开 PDF 失败: {error:#}")));
                    }
                }
            }
            Command::Render {
                page,
                scale,
                request,
            } => {
                let Some(document) = document.as_ref() else {
                    continue;
                };
                match render_page(document, page, scale, request) {
                    Ok(event) => {
                        let _ = events.unbounded_send(event);
                    }
                    Err(error) => {
                        let _ = events
                            .unbounded_send(Event::Error(format!("渲染页面失败: {error:#}")));
                    }
                }
            }
        }
    }
}

fn bind_pdfium() -> Result<&'static Pdfium> {
    let mut directories = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        directories.push(parent.to_path_buf());
    }
    if let Ok(cwd) = std::env::current_dir() {
        directories.push(cwd);
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

    match Pdfium::bind_to_system_library() {
        Ok(bindings) => Ok(Box::leak(Box::new(Pdfium::new(bindings)))),
        Err(error) => Err(last_error.unwrap_or_else(|| anyhow!(error)))
            .context("找不到 pdfium.dll（放到 exe 或项目根目录）"),
    }
}

fn open_document(
    pdfium: &'static Pdfium,
    path: &Path,
) -> Result<(PdfDocument<'static>, Vec<PageSize>, Vec<OutlineItem>)> {
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

    Ok((document, pages, outline))
}

fn collect_bookmark(bookmark: &PdfBookmark, depth: usize, output: &mut Vec<OutlineItem>) {
    let title = bookmark
        .title()
        .unwrap_or_else(|| "（无标题）".to_string());
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

fn render_page(
    document: &PdfDocument<'static>,
    page_index: usize,
    scale: f32,
    request: u64,
) -> Result<Event> {
    let page = document
        .pages()
        .get(page_index as PdfPageIndex)
        .map_err(|error| anyhow!(error))?;

    let target_width = (page.width().value * scale).round().max(1.0) as i32;
    let target_height = (page.height().value * scale).round().max(1.0) as i32;
    let config = PdfRenderConfig::new()
        .set_target_width(target_width)
        .set_target_height(target_height)
        .render_annotations(true)
        .render_form_data(true)
        // GPUI 的 RenderImage 缓冲区是 BGRA；不要让 Pdfium 把字节序反转成 RGBA。
        .set_reverse_byte_order(false);
    let bitmap = page
        .render_with_config(&config)
        .map_err(|error| anyhow!(error))?;
    log::debug!(
        "page {} has {} annotations",
        page_index,
        page.annotations().len()
    );
    let width = bitmap.width() as u32;
    let height = bitmap.height() as u32;
    let bgra = bitmap.as_raw_bytes();

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

    Ok(Event::Rendered {
        page: page_index,
        request,
        width,
        height,
        bgra,
        chars,
        links,
    })
}

fn rect_from_pdfium(rect: PdfRect) -> RectPt {
    RectPt {
        left: rect.left().value,
        bottom: rect.bottom().value,
        right: rect.right().value,
        top: rect.top().value,
    }
}
