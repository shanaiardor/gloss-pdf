//! 首页最近阅读缩略图：请求去重、内存缓存与 PNG 磁盘缓存。
//!
//! 卡片显示的是 `recent.progress.page` 对应的页面，也就是上次读到的位置；页码变化时
//! 重新请求，旧图先留在卡片上，直到新图到达再替换，避免闪回占位图。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use gpui::{Context, RenderImage};
use pdf_core::{ThumbnailCommand, ThumbnailEngine, ThumbnailEvent};

use crate::app::AppState;
use crate::home::{RecentDocument, user_data_dir};

/// 缩略图缓存键：文件、页码和修改时间。文件被替换后缓存自然失效。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ThumbKey {
    path: PathBuf,
    page: usize,
    modified: u64,
}

/// 首页卡片要显示的一张缩略图：画面 + 原始像素尺寸。
#[derive(Clone)]
pub(crate) struct ThumbnailImage {
    pub image: Arc<RenderImage>,
    #[allow(dead_code)]
    pub width: u32,
    #[allow(dead_code)]
    pub height: u32,
}

struct CachedThumbnail {
    key: ThumbKey,
    image: ThumbnailImage,
}

#[derive(Default)]
pub(crate) struct Thumbnails {
    /// 每本最近阅读保留最后一张图；页码变化时原地替换，最多 18 条。
    entries: HashMap<PathBuf, CachedThumbnail>,
    pending: HashMap<u64, ThumbKey>,
    requested: HashSet<ThumbKey>,
    failed: HashSet<ThumbKey>,
    next_request: u64,
}

impl Thumbnails {
    /// 某本最近阅读当前可显示的画面（可能是上一页的旧图，正在等新图）。
    pub(crate) fn image_for(&self, path: &Path) -> Option<ThumbnailImage> {
        self.entries.get(path).map(|entry| entry.image.clone())
    }

    /// 补齐所有最近阅读的当前页缩略图；已有缓存或已排队的直接跳过。
    #[allow(dead_code)]
    pub(crate) fn ensure(&mut self, recents: &[RecentDocument], engine: &ThumbnailEngine) {
        for recent in recents {
            let page = recent.progress.map_or(0, |progress| progress.page);
            if self
                .entries
                .get(&recent.path)
                .is_some_and(|entry| entry.key.page == page)
            {
                continue;
            }

            // 只在缺图或页码变化时读 mtime，避免首页每帧对 18 个文件做 stat。
            let Some(modified) = modified_secs(&recent.path) else {
                continue;
            };
            let key = ThumbKey {
                path: recent.path.clone(),
                page,
                modified,
            };
            if self.failed.contains(&key) || self.requested.contains(&key) {
                continue;
            }

            if let Some(image) = load_cached(&key) {
                self.put(key, image);
                continue;
            }

            let request = self.next_request();
            self.pending.insert(request, key.clone());
            self.requested.insert(key.clone());
            if let Err(error) = engine.commands.send(ThumbnailCommand::Render {
                request,
                path: recent.path.clone(),
                page,
            }) {
                log::warn!("无法请求缩略图 {}: {error}", recent.path.display());
                self.pending.remove(&request);
                self.requested.remove(&key);
                self.failed.insert(key);
            }
        }
    }

    fn next_request(&mut self) -> u64 {
        self.next_request = self.next_request.wrapping_add(1).max(1);
        self.next_request
    }

    fn finish(&mut self, request: u64) -> Option<ThumbKey> {
        let key = self.pending.remove(&request)?;
        self.requested.remove(&key);
        Some(key)
    }

    /// 写入内存缓存；同一文件换页时顺手删掉上一页的 PNG，避免缓存无限增长。
    fn put(&mut self, key: ThumbKey, image: ThumbnailImage) {
        let old = self.entries.insert(
            key.path.clone(),
            CachedThumbnail {
                key: key.clone(),
                image,
            },
        );
        if let Some(old) = old
            && old.key != key
        {
            remove_cached(&old.key);
        }
    }

    fn fail(&mut self, request: u64) {
        if let Some(key) = self.finish(request) {
            self.failed.insert(key);
        }
    }

    /// 阅读器打开文档时缩略图让路；不算失败，回首页后会重新请求。
    fn cancel(&mut self, request: u64) {
        self.finish(request);
    }
}

impl AppState {
    /// 首页每帧调用：把还没有缩略图的条目排进 worker。已有图时是纯内存查表。
    pub(crate) fn ensure_home_thumbnails(&mut self, cx: &mut Context<Self>) {
        // 打开/阅读文档期间不要排缩略图：既避免和主 worker 抢 PDFium，
        // 也避免每帧请求 -> Cancelled -> 再请求的空转。
        if self.pdfium_gate.is_reader_active() {
            return;
        }

        for recent in &self.home.recents {
            let page = recent.progress.map_or(0, |progress| progress.page);
            if self
                .thumbnails
                .entries
                .get(&recent.path)
                .is_some_and(|entry| entry.key.page == page)
            {
                continue;
            }

            // 只在缺图或页码变化时读 mtime，避免首页每帧对 18 个文件做 stat。
            let Some(modified) = modified_secs(&recent.path) else {
                continue;
            };
            let key = ThumbKey {
                path: recent.path.clone(),
                page,
                modified,
            };
            if self.thumbnails.failed.contains(&key) || self.thumbnails.requested.contains(&key) {
                continue;
            }

            self.thumbnails.requested.insert(key.clone());

            // 若磁盘已有 PNG 缓存，丢到后台线程池异步解码，绝不在 UI 主线程同步读写阻塞渲染
            if let Some(path) = cache_path(&key)
                && path.exists()
            {
                let key_clone = key.clone();
                cx.spawn(async move |this, cx| {
                    let raw = cx
                        .background_executor()
                        .spawn(async move { load_cached_raw(&path) })
                        .await;
                    if let Some((width, height, bgra)) = raw {
                        let _ = this.update(cx, |this, cx| {
                            this.thumbnails.requested.remove(&key_clone);
                            if let Some(buffer) = image::RgbaImage::from_raw(width, height, bgra) {
                                let image = ThumbnailImage {
                                    image: Arc::new(RenderImage::new([image::Frame::new(buffer)])),
                                    width,
                                    height,
                                };
                                this.thumbnails.put(key_clone, image);
                                if !this.viewer.has_document() {
                                    cx.notify();
                                }
                            }
                        });
                    } else {
                        let _ = this.update(cx, |this, _cx| {
                            this.thumbnails.requested.remove(&key_clone);
                            this.thumbnails.failed.insert(key_clone);
                        });
                    }
                })
                .detach();
                continue;
            }

            let request = self.thumbnails.next_request();
            self.thumbnails.pending.insert(request, key.clone());
            if let Err(error) = self.thumbnail_engine.commands.send(ThumbnailCommand::Render {
                request,
                path: recent.path.clone(),
                page,
            }) {
                log::warn!("无法请求缩略图 {}: {error}", recent.path.display());
                self.thumbnails.pending.remove(&request);
                self.thumbnails.requested.remove(&key);
                self.thumbnails.failed.insert(key);
            }
        }
    }

    pub(super) fn handle_thumbnail_event(&mut self, event: ThumbnailEvent, cx: &mut Context<Self>) {
        match event {
            ThumbnailEvent::Rendered {
                request,
                path: _,
                page: _,
                width,
                height,
                bgra,
            } => {
                let Some(key) = self.thumbnails.finish(request) else {
                    return;
                };
                let Some(buffer) = image::RgbaImage::from_raw(width, height, bgra.clone()) else {
                    log::warn!("缩略图位图尺寸不匹配：{width}x{height}");
                    return;
                };

                // 磁盘缓存按正常 RGBA 存；将 PNG 编码与落盘移到后台线程池，绝不拖慢主线程绘制
                let save_key = key.clone();
                let mut rgba = bgra;
                swap_red_blue(&mut rgba);
                cx.background_executor()
                    .spawn(async move {
                        save_cached(&save_key, width, height, &rgba);
                    })
                    .detach();

                let image = ThumbnailImage {
                    image: Arc::new(RenderImage::new([image::Frame::new(buffer)])),
                    width,
                    height,
                };
                self.thumbnails.put(key, image);
                if !self.viewer.has_document() {
                    cx.notify();
                }
            }
            ThumbnailEvent::Failed {
                request,
                path,
                page,
                message,
            } => {
                self.thumbnails.fail(request);
                log::warn!(
                    "缩略图渲染失败 {} 第 {} 页: {message}",
                    path.display(),
                    page + 1
                );
            }
            ThumbnailEvent::Cancelled { request } => {
                self.thumbnails.cancel(request);
            }
            ThumbnailEvent::Status(message) => {
                log::info!("{message}");
                self.set_status(message, cx);
                cx.notify();
            }
        }
    }
}

fn modified_secs(path: &Path) -> Option<u64> {
    let metadata = std::fs::metadata(path).ok()?;
    // 某些网络盘/只读介质拿不到 mtime；退回 0 仍可生成缩略图，只是缓存不随修改失效。
    Some(
        metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs()),
    )
}

/// 缓存文件放在 `%APPDATA%\gloss-pdf\thumbnails\<hash>.png`。
fn cache_path(key: &ThumbKey) -> Option<PathBuf> {
    let directory = user_data_dir()?.join("thumbnails");
    Some(directory.join(format!("{:016x}.png", cache_hash(key))))
}

fn cache_hash(key: &ThumbKey) -> u64 {
    // FNV-1a：比 `DefaultHasher` 稳定，跨 Rust 版本不会整批失效。
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let page = key.page.to_le_bytes();
    let modified = key.modified.to_le_bytes();
    for byte in key
        .path
        .to_string_lossy()
        .as_bytes()
        .iter()
        .chain(page.iter())
        .chain(modified.iter())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn load_cached_raw(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let decoded = image::open(path).ok()?.to_rgba8();
    let (width, height) = decoded.dimensions();
    let mut bgra = decoded.into_raw();
    swap_red_blue(&mut bgra);
    Some((width, height, bgra))
}

#[allow(dead_code)]
fn load_cached(key: &ThumbKey) -> Option<ThumbnailImage> {
    let path = cache_path(key)?;
    let (width, height, bgra) = load_cached_raw(&path)?;
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(ThumbnailImage {
        image: Arc::new(RenderImage::new([image::Frame::new(buffer)])),
        width,
        height,
    })
}

fn save_cached(key: &ThumbKey, width: u32, height: u32, rgba: &[u8]) {
    let Some(path) = cache_path(key) else {
        return;
    };
    if let Some(directory) = path.parent()
        && let Err(error) = std::fs::create_dir_all(directory)
    {
        log::warn!("无法创建缩略图缓存目录 {}: {error}", directory.display());
        return;
    }
    if let Err(error) = image::save_buffer(&path, rgba, width, height, image::ColorType::Rgba8) {
        log::warn!("无法写入缩略图缓存 {}: {error}", path.display());
    }
}

fn remove_cached(key: &ThumbKey) {
    if let Some(path) = cache_path(key) {
        let _ = std::fs::remove_file(path);
    }
}

/// PDFium 给的是 BGRA，PNG 要 RGBA；两处都调用这一份交换逻辑。
fn swap_red_blue(bytes: &mut [u8]) {
    for pixel in bytes.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_and_rgba_are_swapped_in_place() {
        let mut bytes = vec![1, 2, 3, 255, 4, 5, 6, 255];
        swap_red_blue(&mut bytes);
        assert_eq!(bytes, vec![3, 2, 1, 255, 6, 5, 4, 255]);
    }

    #[test]
    fn cache_hash_depends_on_page_and_modification_time() {
        let base = ThumbKey {
            path: PathBuf::from("C:/books/a.pdf"),
            page: 3,
            modified: 100,
        };
        let other_page = ThumbKey {
            page: 4,
            ..base.clone()
        };
        let other_time = ThumbKey {
            modified: 101,
            ..base.clone()
        };
        assert_ne!(cache_hash(&base), cache_hash(&other_page));
        assert_ne!(cache_hash(&base), cache_hash(&other_time));
    }
}
