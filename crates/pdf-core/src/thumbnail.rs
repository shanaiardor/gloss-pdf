//! 首页最近阅读的缩略图 worker。
//!
//! 和阅读器的主 PDF worker 分开：首页一次可能要补十几张缩略图，放在同一条队列
//! 里会拖慢翻页。这里只做「打开文件 → 渲染指定页 → 回传 BGRA」，缓存与去重都在 UI 层。
//!
//! PDFium 不是线程安全的：绑定实例在 crate 内全局共享，但对它的调用仍由
//! [`PdfiumGate`] 与阅读器 worker 串行化。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::{Context as _, Result, anyhow};
use futures::channel::mpsc::UnboundedSender;
use pdfium_render::prelude::*;

use crate::engine::{PdfiumGate, bind_pdfium};

/// 缩略图的最大像素尺寸。首页卡片预览高度只有几十像素，360 宽足够 2x 屏。
pub const THUMBNAIL_MAX_WIDTH: u32 = 360;
/// 超长页面（长卷、条幅）按高度再夹一次，避免渲染出几千像素高的位图。
pub const THUMBNAIL_MAX_HEIGHT: u32 = 640;

pub enum ThumbnailCommand {
    Render {
        request: u64,
        path: PathBuf,
        /// 0 基页码。
        page: usize,
    },
}

pub enum ThumbnailEvent {
    Rendered {
        request: u64,
        path: PathBuf,
        page: usize,
        width: u32,
        height: u32,
        /// BGRA 像素，与阅读器瓦片使用同一种缓冲区约定。
        bgra: Vec<u8>,
    },
    Failed {
        request: u64,
        path: PathBuf,
        page: usize,
        message: String,
    },
    /// 阅读器正打开着文档，缩略图让路；UI 清掉请求记录，回首页后会重新排队。
    Cancelled {
        request: u64,
    },
    /// 运行期进度提示（例如自动下载 PDFium）。
    Status(String),
}

pub struct ThumbnailEngine {
    pub commands: Sender<ThumbnailCommand>,
}

impl ThumbnailEngine {
    pub fn spawn(events: UnboundedSender<ThumbnailEvent>) -> Self {
        Self::spawn_with_gate(events, PdfiumGate::default())
    }

    /// 与阅读器引擎共用同一把闸门。
    pub fn spawn_with_gate(events: UnboundedSender<ThumbnailEvent>, gate: PdfiumGate) -> Self {
        let (commands, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("pdf-thumbnail-worker".into())
            .spawn(move || worker_loop(receiver, events, gate))
            .expect("failed to spawn pdf thumbnail worker");
        Self { commands }
    }
}

fn worker_loop(
    receiver: Receiver<ThumbnailCommand>,
    events: UnboundedSender<ThumbnailEvent>,
    gate: PdfiumGate,
) {
    let mut pdfium: Option<&'static Pdfium> = None;

    while let Ok(command) = receiver.recv() {
        let ThumbnailCommand::Render {
            request,
            path,
            page,
        } = command;

        // 阅读器有文档时不要让 PDFium 同时开工；正在渲染的那一张会先结束，
        // 阅读器的 Open 在闸门外等它。
        if gate.reader_active() {
            let _ = events.unbounded_send(ThumbnailEvent::Cancelled { request });
            continue;
        }
        let _guard = gate.lock();
        if gate.reader_active() {
            let _ = events.unbounded_send(ThumbnailEvent::Cancelled { request });
            continue;
        }

        if pdfium.is_none() {
            let mut status = |message: &str| {
                let _ = events.unbounded_send(ThumbnailEvent::Status(message.to_string()));
            };
            match bind_pdfium(&mut status) {
                Ok(instance) => pdfium = Some(instance),
                Err(error) => {
                    let _ = events.unbounded_send(ThumbnailEvent::Failed {
                        request,
                        path,
                        page,
                        message: format!("加载 PDFium 失败: {error:#}"),
                    });
                    continue;
                }
            }
        }

        let instance = pdfium.expect("pdfium bound above");
        match render_thumbnail(instance, &path, page) {
            Ok((width, height, bgra)) => {
                let _ = events.unbounded_send(ThumbnailEvent::Rendered {
                    request,
                    path,
                    page,
                    width,
                    height,
                    bgra,
                });
            }
            Err(error) => {
                log::debug!(
                    "thumbnail failed {} p{}: {error:#}",
                    path.display(),
                    page + 1
                );
                let _ = events.unbounded_send(ThumbnailEvent::Failed {
                    request,
                    path,
                    page,
                    message: format!("{error:#}"),
                });
            }
        }
    }
}

fn render_thumbnail(
    pdfium: &'static Pdfium,
    path: &Path,
    page_index: usize,
) -> Result<(u32, u32, Vec<u8>)> {
    let document = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|error| anyhow!(error))
        .with_context(|| format!("无法读取 {}", path.display()))?;
    let page = document
        .pages()
        .get(page_index as PdfPageIndex)
        .map_err(|error| anyhow!(error))
        .with_context(|| format!("第 {} 页不存在", page_index + 1))?;
    let (width, height) = thumbnail_size(
        page.width().value,
        page.height().value,
        THUMBNAIL_MAX_WIDTH,
        THUMBNAIL_MAX_HEIGHT,
    )?;

    let config = PdfRenderConfig::new()
        .set_fixed_size(width as i32, height as i32)
        .render_annotations(true)
        .render_form_data(true)
        // 与阅读器瓦片保持一致：GPUI 直接按 BGRA 解释缓冲区。
        .set_reverse_byte_order(false);
    let mut bitmap = PdfBitmap::empty(width as i32, height as i32, PdfBitmapFormat::BGRA)
        .map_err(|error| anyhow!(error))?;
    page.render_into_bitmap_with_config(&mut bitmap, &config)
        .map_err(|error| anyhow!(error))?;

    log::debug!(
        "thumbnail p{} {}x{} for {}",
        page_index + 1,
        width,
        height,
        path.display()
    );
    Ok((width, height, bitmap.as_raw_bytes()))
}

/// 在最大宽高内按页面比例算缩略图尺寸；非法页面返回错误。
fn thumbnail_size(
    page_width: f32,
    page_height: f32,
    max_width: u32,
    max_height: u32,
) -> Result<(u32, u32)> {
    if !page_width.is_finite()
        || !page_height.is_finite()
        || page_width <= 0.0
        || page_height <= 0.0
    {
        return Err(anyhow!("页面尺寸无效：{page_width} x {page_height}"));
    }

    let scale = (max_width as f32 / page_width).min(max_height as f32 / page_height);
    let width = (page_width * scale).round().clamp(1.0, max_width as f32) as u32;
    let height = (page_height * scale).round().clamp(1.0, max_height as f32) as u32;
    Ok((width.max(1), height.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portrait_pages_use_the_full_width() {
        assert_eq!(thumbnail_size(595.0, 842.0, 360, 640).unwrap(), (360, 509));
    }

    #[test]
    fn landscape_pages_fit_by_width_too() {
        assert_eq!(thumbnail_size(842.0, 595.0, 360, 640).unwrap(), (360, 254));
    }

    #[test]
    fn very_tall_pages_are_capped_by_height() {
        let (width, height) = thumbnail_size(200.0, 4000.0, 360, 640).unwrap();
        assert_eq!(height, 640);
        assert_eq!(width, 32);
    }

    #[test]
    fn invalid_page_sizes_are_rejected() {
        assert!(thumbnail_size(0.0, 842.0, 360, 640).is_err());
        assert!(thumbnail_size(f32::NAN, 842.0, 360, 640).is_err());
    }
}
