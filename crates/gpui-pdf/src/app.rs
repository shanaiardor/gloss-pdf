//! 应用控制器：把 PDF 引擎事件、键盘/鼠标输入接到 Viewer 状态和 UI 上。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Point, Render, RenderImage,
    Size, Window, div, point, prelude::*, px, size,
};
use pdf_core::{Command, Event, LinkTarget, PageSize, PdfEngine};

use crate::highlights::sidecar_path;
use crate::home::HomeState;
use crate::layout::{
    FitArea, FitMode, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH, SCREEN_MARGIN, VERTICAL_INSETS,
    WINDOW_FILL, window_size_for_page,
};
use crate::progress::{ReadingProgress, unix_now};
use crate::theme::Theme;
use crate::viewer::{Document, PageData, Viewer, scroll_offset_for_ratio};

pub(crate) use crate::layout::{BOTTOM_INSET, PAGE_PAD, TOP_INSET};

/// 页内滚动没有独立事件，用固定间隔采样阅读位置。
const PROGRESS_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// 连续滚动时合并写盘，避免每一帧都碰磁盘。
const PROGRESS_FLUSH_DELAY: Duration = Duration::from_millis(800);

/// 页面在滚动容器里的尺寸与居中偏移，恢复阅读进度时也用它算内容高度。
#[derive(Clone, Copy, Debug)]
pub(crate) struct PageLayout {
    pub page_width: f32,
    pub page_height: f32,
    pub center: f32,
}

impl PageLayout {
    pub fn new(page: PageSize, zoom: f32, viewport: Size<gpui::Pixels>) -> Self {
        let page_width = page.width * zoom;
        let page_height = page.height * zoom;
        let available = f32::from(viewport.width) - PAGE_PAD * 2.0;
        let center = ((available - page_width) / 2.0).max(0.0);
        Self {
            page_width,
            page_height,
            center,
        }
    }

    /// 滚动内容的实际尺寸：左右页边距 + 居中留白，以及上下浮动胶囊留白。
    pub fn content_size(&self) -> (f32, f32) {
        (
            PAGE_PAD * 2.0 + self.center + self.page_width,
            VERTICAL_INSETS + self.page_height,
        )
    }
}

/// 窗口当前还能长到多大（内容尺寸）。
///
/// 不能只按屏幕大小算：GPUI 的 `resize` 不会改窗口左上角位置，窗口靠下时
/// 再按整屏高度长就会伸出屏幕，所以要扣掉当前位置。
fn window_room(window: &Window, cx: &App) -> (f32, f32) {
    let work = window
        .display(cx)
        .map(|display| display.visible_bounds())
        .unwrap_or_else(|| Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(800.))));
    let current = window.bounds();

    let room_width = (f32::from(work.right()) - f32::from(current.origin.x) - SCREEN_MARGIN)
        .min(f32::from(work.size.width) * WINDOW_FILL)
        .max(MIN_WINDOW_WIDTH);
    let room_height = (f32::from(work.bottom()) - f32::from(current.origin.y) - SCREEN_MARGIN)
        .min(f32::from(work.size.height) * WINDOW_FILL)
        .max(MIN_WINDOW_HEIGHT);

    (room_width, room_height)
}

pub struct AppState {
    pub(crate) theme: Theme,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) engine: std::sync::mpsc::Sender<Command>,
    pub(crate) viewer: Viewer,
    pub(crate) home: HomeState,
    pub(crate) viewport: Size<gpui::Pixels>,
    pub(crate) scale_factor: f32,
    pub(crate) request_id: u64,
    pub(crate) outline_open: bool,
    pub(crate) status: String,
    pub(crate) status_version: u64,
    /// 当前文档上一次采样到的阅读位置，用来跳过重复写盘。
    last_progress: Option<ReadingProgress>,
    /// 是否已经安排了一次延迟写盘。
    progress_flush_scheduled: bool,
    /// 打开文档后按纸张比例调整窗口；在 `render` 里执行（那里能拿到 `Window`）。
    pending_window_fit: Option<PageSize>,
    /// 回到首页时恢复阅读前的窗口尺寸。
    pending_window_resize: Option<Size<gpui::Pixels>>,
    /// 进入阅读器之前的窗口尺寸，关闭文档时恢复。
    reader_window_size: Option<Size<gpui::Pixels>>,
}

impl AppState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (event_tx, mut event_rx) = futures::channel::mpsc::unbounded();
        let engine = PdfEngine::spawn(event_tx).commands;

        cx.spawn(async move |this, cx| {
            while let Some(event) = event_rx.next().await {
                if this
                    .update(cx, |this, cx| this.handle_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // 页内滚动不会触发专门的事件，按固定间隔采样，空闲时立即停手。
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(PROGRESS_SAMPLE_INTERVAL)
                    .await;
                if this
                    .update(cx, |this, cx| this.sample_progress(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // 关窗/退出时兜底保存一次，避免最后不到一秒的滚动丢失。
        cx.on_release(|this, _cx| this.flush_progress()).detach();

        Self {
            theme: Theme::light(),
            focus_handle: cx.focus_handle(),
            engine,
            viewer: Viewer::new(),
            home: HomeState::load(),
            viewport: Size {
                width: px(1040.),
                height: px(720.),
            },
            scale_factor: 1.0,
            request_id: 0,
            outline_open: false,
            status: String::new(),
            status_version: 0,
            last_progress: None,
            progress_flush_scheduled: false,
            pending_window_fit: None,
            pending_window_resize: None,
            reader_window_size: None,
        }
    }

    // ---------- 阅读进度 ----------

    /// 采样当前阅读位置；有变化时更新内存记录并安排延迟写盘。
    fn sample_progress(&mut self, cx: &mut Context<Self>) {
        if self.capture_progress() {
            self.schedule_progress_flush(cx);
        }
    }

    /// 把当前页、页内滚动比例和缩放写进最近阅读（只改内存）。
    ///
    /// 返回 `false` 表示没有文档或位置没变。
    fn capture_progress(&mut self) -> bool {
        let Some(progress) = self.current_progress() else {
            return false;
        };
        if self
            .last_progress
            .is_some_and(|last| last.same_position(&progress))
        {
            return false;
        }
        self.last_progress = Some(progress);
        if let Some(path) = self.viewer.doc.as_ref().map(|doc| doc.path.clone()) {
            self.home.set_progress(&path, progress);
        }
        true
    }

    fn current_progress(&self) -> Option<ReadingProgress> {
        let doc = self.viewer.doc.as_ref()?;
        let (scroll_x, scroll_y) = self.viewer.scroll_ratios();
        // serde_json 不接受 NaN/Infinity，缩放异常时退回 1.0。
        let zoom = if self.viewer.zoom.is_finite() {
            self.viewer.zoom
        } else {
            1.0
        };
        Some(ReadingProgress {
            page: self.viewer.current_page,
            pages: doc.pages.len(),
            zoom,
            // 新记录总是带模式，包括手动缩放（free）；缺字段表示旧版本。
            fit: Some(self.viewer.fit_mode),
            scroll_x,
            scroll_y,
            updated_at: unix_now(),
        })
    }

    /// 立即把进度落盘（切页、缩放、回到首页、退出时调用）。
    pub(crate) fn flush_progress(&mut self) {
        self.capture_progress();
        self.home.flush();
    }

    fn schedule_progress_flush(&mut self, cx: &mut Context<Self>) {
        if self.progress_flush_scheduled {
            return;
        }
        self.progress_flush_scheduled = true;

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PROGRESS_FLUSH_DELAY).await;
            let _ = this.update(cx, |this, _cx| {
                this.progress_flush_scheduled = false;
                this.home.flush();
            });
        })
        .detach();
    }

    /// 用当前视口把恢复出来的页内比例换算成滚动偏移。
    fn apply_pending_scroll(&mut self) {
        if let Some(ratios) = self.viewer.take_pending_scroll() {
            self.set_scroll_ratios(ratios);
        }
    }

    /// 按当前缩放和视口设置页内滚动位置。
    fn set_scroll_ratios(&mut self, (ratio_x, ratio_y): (f32, f32)) {
        let Some(page) = self.viewer.page_size() else {
            return;
        };

        let layout = PageLayout::new(page, self.viewer.zoom, self.viewport);
        let (content_width, content_height) = layout.content_size();
        let viewport_width = f32::from(self.viewport.width);
        let viewport_height = f32::from(self.viewport.height);
        self.viewer.scroll.set_offset(point(
            px(scroll_offset_for_ratio(
                ratio_x,
                content_width,
                viewport_width,
            )),
            px(scroll_offset_for_ratio(
                ratio_y,
                content_height,
                viewport_height,
            )),
        ));
    }

    // ---------- 文档与渲染 ----------

    pub(crate) fn open_pdf(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 PDF 文件".into()),
        });
        let engine = self.engine.clone();

        cx.spawn(async move |_this, _cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = engine.send(Command::Open(path));
            }
        })
        .detach();
    }

    /// 从命令行/外部直接打开一个 PDF。
    pub fn open_path(&mut self, path: PathBuf) {
        let _ = self.engine.send(Command::Open(path));
    }

    /// 打开「最近阅读」中的一条；文件已被移动时自动移除。
    pub(crate) fn open_recent(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(recent) = self.home.recents.get(index).cloned() else {
            return;
        };
        self.home.menu_open = None;
        self.home.search_focused = false;

        if !recent.path.is_file() {
            self.home.remove(index);
            self.set_status("文件已不存在，已从最近阅读移除", cx);
            cx.notify();
            return;
        }

        self.open_path(recent.path);
    }

    /// 处理拖进窗口的文件，只接受第一个 PDF。
    pub(crate) fn open_dropped(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let pdf = paths
            .iter()
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
            })
            .cloned();

        match pdf {
            Some(path) => self.open_path(path),
            None => {
                self.set_status("请拖入 PDF 文件", cx);
                cx.notify();
            }
        }
    }

    pub(crate) fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.theme = self.theme.clone().toggled();
        self.set_status(format!("主题：{}", self.theme.name()), cx);
        cx.notify();
    }

    /// 左下角浮动提示，4 秒后自动消失。
    pub(crate) fn set_status(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.status = text.into();
        self.status_version = self.status_version.wrapping_add(1);
        let version = self.status_version;

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;

            let _ = this.update(cx, |this, cx| {
                if this.status_version == version {
                    this.status.clear();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn request_render(&mut self) {
        if self.viewer.page_size().is_none() {
            return;
        }
        self.request_id += 1;
        let scale = (self.viewer.zoom * self.scale_factor).clamp(0.1, 8.0);
        let _ = self.engine.send(Command::Render {
            page: self.viewer.current_page,
            scale,
            request: self.request_id,
        });
    }

    pub(crate) fn set_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if !self.viewer.set_page(page) {
            return;
        }
        self.refit();
        self.request_render();
        self.recompute_matches(cx);
        self.flush_progress();
        cx.notify();
    }

    pub(crate) fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.viewer.set_zoom(zoom);
        self.request_render();
        self.flush_progress();
        cx.notify();
    }

    // ---------- 适宽 / 适页 / 窗口尺寸 ----------

    fn fit_area(&self) -> FitArea {
        FitArea::from_viewport(
            f32::from(self.viewport.width),
            f32::from(self.viewport.height),
        )
    }

    /// 视口变化或切页后，按当前适宽/适页模式重算缩放。
    ///
    /// 返回 `true` 表示缩放或模式变了，调用方需要重新渲染页面。
    fn refit(&mut self) -> bool {
        let mode = self.viewer.fit_mode;
        if mode == FitMode::Free {
            return false;
        }

        let ratios = self.viewer.scroll_ratios();
        if !self.viewer.fit(mode, self.fit_area()) {
            return false;
        }
        self.set_scroll_ratios(ratios);
        true
    }

    /// 用户主动切换适宽/适页：重算缩放并立即重渲染。
    pub(crate) fn apply_fit(&mut self, mode: FitMode, cx: &mut Context<Self>) {
        let ratios = self.viewer.scroll_ratios();
        if !self.viewer.fit(mode, self.fit_area()) {
            return;
        }
        self.set_scroll_ratios(ratios);
        self.request_render();
        self.flush_progress();
        cx.notify();
    }

    pub(crate) fn fit_to_page(&mut self, cx: &mut Context<Self>) {
        self.apply_fit(FitMode::Page, cx);
    }

    pub(crate) fn fit_to_width(&mut self, cx: &mut Context<Self>) {
        self.apply_fit(FitMode::Width, cx);
    }

    /// 打开文档后按纸张比例调窗口，或回到首页时恢复原窗口尺寸。
    ///
    /// 窗口尺寸要在 `Render` 里改，因为事件处理拿不到 `Window`。
    fn apply_pending_window_resize(&mut self, window: &mut Window, cx: &App) {
        let page = self.pending_window_fit.take();
        let restore = self.pending_window_resize.take();

        // 最大化/全屏时不要改窗口尺寸，整页适应会直接用当前视口。
        if window.is_maximized() || window.is_fullscreen() {
            return;
        }

        if let Some(page) = page {
            if self.reader_window_size.is_none() {
                // 记内容尺寸，`resize` 接收的也是内容尺寸，避免把边框算进去。
                self.reader_window_size = Some(window.viewport_size());
            }

            let (room_width, room_height) = window_room(window, cx);
            let (width, height) = window_size_for_page(page, room_width, room_height);
            log::info!(
                "fit window to page {:.0}x{:.0}: {width:.0}x{height:.0}",
                page.width,
                page.height
            );
            window.resize(size(px(width), px(height)));
        } else if let Some(target) = restore {
            window.resize(target);
        }
    }

    pub(crate) fn close_document(&mut self, cx: &mut Context<Self>) {
        self.flush_progress();
        self.viewer.close();
        self.last_progress = None;
        self.outline_open = false;
        self.home.menu_open = None;
        self.home.search_focused = false;
        self.home.query.clear();
        self.pending_window_fit = None;
        self.pending_window_resize = self.reader_window_size.take();
        cx.notify();
    }

    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Opened {
                path,
                pages,
                outline,
            } => {
                // 切换文档前先记下上一份文档的最后位置，record 会顺带落盘。
                self.capture_progress();

                let count = pages.len();
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                let saved = self.home.progress_for(&path);

                let area = self.fit_area();
                self.home.record(path.clone(), name.clone());
                self.home.query.clear();
                self.home.search_focused = false;
                self.viewer.reset(
                    Document {
                        path,
                        pages,
                        outline,
                    },
                    area,
                );

                let restored =
                    saved.is_some_and(|progress| self.viewer.restore_progress(&progress, area));
                self.apply_pending_scroll();

                // 等 render 拿到当前显示器工作区后，按这张纸的比例调整窗口；
                // 随后视口变化会再按适宽/适页模式重算缩放。
                self.pending_window_fit = self.viewer.page_size().filter(|page| {
                    page.width.is_finite()
                        && page.height.is_finite()
                        && page.width > 0.0
                        && page.height > 0.0
                });
                self.pending_window_resize = None;
                // 恢复成功时以保存的位置为基线，等布局完成后采样再修正；
                // 失败时清空基线，让下一次采样覆盖掉过期的旧进度。
                self.last_progress = if restored { saved } else { None };

                if let Some(doc) = &self.viewer.doc {
                    let path = sidecar_path(&doc.path);
                    self.viewer.highlights.load(&path);
                }

                self.request_render();
                if restored {
                    self.set_status(
                        format!(
                            "已恢复到第 {} / {} 页 · {name}",
                            self.viewer.current_page + 1,
                            count
                        ),
                        cx,
                    );
                } else {
                    self.set_status(format!("{name} · {count} 页"), cx);
                }
                log::info!(
                    "opened {} ({} pages, {} outline items, restored: {restored})",
                    name,
                    count,
                    self.viewer.doc.as_ref().map_or(0, |doc| doc.outline.len())
                );
            }
            Event::Rendered {
                page,
                request,
                width,
                height,
                bgra,
                chars,
                links,
            } => {
                if page != self.viewer.current_page || request != self.request_id {
                    return;
                }
                let Some(buffer) = image::RgbaImage::from_raw(width, height, bgra) else {
                    self.set_status("页面位图格式错误", cx);
                    cx.notify();
                    return;
                };
                let render_image = RenderImage::new([image::Frame::new(buffer)]);
                self.viewer.page = Some(PageData {
                    image: Arc::new(render_image),
                    chars,
                    links,
                });
                self.recompute_matches(cx);
                log::info!(
                    "rendered page {}: {}x{}, {} chars, {} links",
                    page,
                    width,
                    height,
                    self.viewer.page.as_ref().map_or(0, |page| page.chars.len()),
                    self.viewer.page.as_ref().map_or(0, |page| page.links.len())
                );
            }
            Event::Error(message) => {
                log::error!("{message}");
                self.set_status(message, cx);
            }
        }
        cx.notify();
    }

    // ---------- 坐标 ----------

    pub(crate) fn page_origin(&self, window: &Window) -> Point<gpui::Pixels> {
        let viewport = window.viewport_size();
        let body_width = f32::from(viewport.width);
        let page_width = self.viewer.page_size().map_or(0.0, |page| page.width) * self.viewer.zoom;
        let center = ((body_width - PAGE_PAD * 2.0 - page_width) / 2.0).max(0.0);
        let offset = self.viewer.scroll.offset();

        point(
            px(PAGE_PAD + center + f32::from(offset.x)),
            px(TOP_INSET + f32::from(offset.y)),
        )
    }

    fn to_local(&self, position: Point<gpui::Pixels>, window: &Window) -> (f32, f32) {
        let origin = self.page_origin(window);
        (
            f32::from(position.x) - f32::from(origin.x),
            f32::from(position.y) - f32::from(origin.y),
        )
    }

    fn to_pdf(&self, local: (f32, f32)) -> (f32, f32) {
        let page = self.viewer.page_size().unwrap_or_default();
        (
            local.0 / self.viewer.zoom,
            page.height - local.1 / self.viewer.zoom,
        )
    }

    fn hit_char(&self, x: f32, y: f32) -> Option<usize> {
        let page = self.viewer.page.as_ref()?;
        let mut best: Option<(f32, usize)> = None;

        for (index, character) in page.chars.iter().enumerate() {
            let rect = character.rect;
            if x >= rect.left && x <= rect.right && y >= rect.bottom && y <= rect.top {
                return Some(index);
            }
            let (cx, cy) = rect.center();
            let distance = ((cx - x).powi(2) + (cy - y).powi(2)).sqrt();
            if best.is_none_or(|(best_distance, _)| distance < best_distance) {
                best = Some((distance, index));
            }
        }

        best.and_then(|(distance, index)| (distance < 24.0).then_some(index))
    }

    fn char_at_point(&self, window: &Window, position: Point<gpui::Pixels>) -> Option<usize> {
        let (x, y) = self.to_pdf(self.to_local(position, window));
        self.hit_char(x, y)
    }

    // ---------- 选择 / 高亮 / 搜索 ----------

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        match self.viewer.selection.text(self.viewer.chars()) {
            Some(text) if !text.is_empty() => {
                let count = text.chars().count();
                log::info!("copied {count} chars: {text:?}");
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.set_status(format!("已复制 {count} 个字符"), cx);
            }
            _ => {
                self.set_status("没有选中文本", cx);
            }
        }
        cx.notify();
    }

    /// 调试图：把当前选中的文字打到终端（`RUST_LOG` 默认 info），方便核对选区内容。
    fn print_selection(&self) {
        let Some(range) = self.viewer.selection.range() else {
            return;
        };
        let Some(text) = self.viewer.selection.text(self.viewer.chars()) else {
            return;
        };
        log::info!(
            "selection page {} chars {}..={} ({}): {text:?}",
            self.viewer.current_page + 1,
            range.start(),
            range.end(),
            text.chars().count(),
        );
    }

    fn select_word_at(&mut self, index: usize) {
        let Some(page) = &self.viewer.page else {
            return;
        };
        self.viewer.selection.select_word(&page.chars, index);
    }

    pub(crate) fn add_highlight(&mut self, cx: &mut Context<Self>) {
        let rects = self.viewer.selection.rects(self.viewer.chars());
        if rects.is_empty() {
            self.set_status("先选中要标注的文本", cx);
            cx.notify();
            return;
        }

        let page = self.viewer.current_page;
        self.viewer.highlights.add(page, rects);
        self.set_status("已添加高亮，Ctrl+S 保存", cx);
        cx.notify();
    }

    pub(crate) fn save_highlights(&mut self, cx: &mut Context<Self>) {
        if self.viewer.highlights.is_empty() {
            self.set_status("还没有高亮", cx);
            cx.notify();
            return;
        }
        let Some(doc) = &self.viewer.doc else {
            return;
        };
        let path = sidecar_path(&doc.path);

        match self.viewer.highlights.save(&path) {
            Ok(()) => self.set_status(format!("高亮已保存到 {}", path.display()), cx),
            Err(error) => self.set_status(format!("保存高亮失败: {error}"), cx),
        }
        cx.notify();
    }

    fn recompute_matches(&mut self, cx: &mut Context<Self>) {
        let chars = self
            .viewer
            .page
            .as_ref()
            .map(|page| page.chars.as_slice())
            .unwrap_or_default();
        let message = self.viewer.search.recompute(chars);
        if let Some(message) = message {
            self.set_status(message, cx);
        }
    }

    fn next_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.viewer.search.next(forward) {
            self.set_status("本页无匹配", cx);
        }
        cx.notify();
    }

    // ---------- 交互 ----------

    pub(crate) fn on_page_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.char_at_point(window, event.position) else {
            return;
        };
        if event.click_count >= 2 {
            self.select_word_at(index);
            self.viewer.selection.dragging = true;
        } else {
            self.viewer.selection.start_at(index);
        }
        cx.notify();
    }

    pub(crate) fn on_page_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.viewer.selection.dragging || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let Some(index) = self.char_at_point(window, event.position) {
            self.viewer.selection.extend_to(index);
            cx.notify();
        }
    }

    pub(crate) fn on_page_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_dragging = self.viewer.selection.dragging;
        self.viewer.selection.dragging = false;
        // 选择结束（拖选或双击选词）后打印选区，便于测试选中文字是否正确。
        if was_dragging {
            self.print_selection();
        }
        cx.notify();
    }

    pub(crate) fn follow_link(&mut self, target: LinkTarget, cx: &mut Context<Self>) {
        match target {
            LinkTarget::Uri(uri) => {
                self.set_status(format!("打开链接 {uri}"), cx);
                cx.open_url(&uri);
            }
            LinkTarget::Page(page) => {
                self.set_page(page, cx);
            }
        }
        cx.notify();
    }

    fn on_home_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let control = keystroke.modifiers.control || keystroke.modifiers.platform;

        if control {
            match keystroke.key.as_str() {
                "o" => self.open_pdf(window, cx),
                "d" => self.toggle_theme(cx),
                "q" => {
                    self.flush_progress();
                    cx.quit();
                }
                "f" => {
                    self.home.search_focused = true;
                    self.set_status("输入关键词筛选最近阅读，Esc 清空", cx);
                    cx.notify();
                }
                _ => {}
            }
            return;
        }

        match keystroke.key.as_str() {
            "escape" => {
                self.home.query.clear();
                self.home.menu_open = None;
                self.home.search_focused = false;
                cx.notify();
            }
            "backspace" => {
                if self.home.query.pop().is_some() {
                    self.home.menu_open = None;
                    cx.notify();
                }
            }
            "enter" => {
                if let Some(index) = self.home.filtered().first().copied() {
                    self.open_recent(index, cx);
                }
            }
            _ => {
                if let Some(key_char) = &keystroke.key_char
                    && !key_char.chars().all(char::is_control)
                {
                    self.home.query.push_str(key_char);
                    self.home.search_focused = true;
                    self.home.menu_open = None;
                    cx.notify();
                }
            }
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.viewer.has_document() {
            self.on_home_key_down(event, window, cx);
            return;
        }

        let keystroke = &event.keystroke;
        let control = keystroke.modifiers.control || keystroke.modifiers.platform;

        if self.viewer.search.open {
            match keystroke.key.as_str() {
                "escape" => {
                    self.viewer.search.close();
                    cx.notify();
                    return;
                }
                "enter" => {
                    self.next_match(!keystroke.modifiers.shift, cx);
                    return;
                }
                "backspace" => {
                    self.viewer.search.query.pop();
                    self.recompute_matches(cx);
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }

        if control {
            match keystroke.key.as_str() {
                "o" => self.open_pdf(window, cx),
                "c" => self.copy_selection(cx),
                "f" => {
                    self.viewer.search.open = true;
                    self.set_status("输入关键词，Enter 下一个，Shift+Enter 上一个", cx);
                    cx.notify();
                    return;
                }
                "=" | "+" => {
                    let zoom = self.viewer.zoom * 1.15;
                    self.set_zoom(zoom, cx);
                    return;
                }
                "-" => {
                    let zoom = self.viewer.zoom / 1.15;
                    self.set_zoom(zoom, cx);
                    return;
                }
                "0" => {
                    self.fit_to_page(cx);
                    return;
                }
                "1" => {
                    self.fit_to_width(cx);
                    return;
                }
                "d" => {
                    self.toggle_theme(cx);
                    return;
                }
                "b" => {
                    self.outline_open = !self.outline_open;
                    cx.notify();
                    return;
                }
                "s" => {
                    self.save_highlights(cx);
                    return;
                }
                "q" => {
                    self.flush_progress();
                    cx.quit();
                    return;
                }
                _ => {}
            }
        } else {
            match keystroke.key.as_str() {
                "pagedown" | "right" | "space" => {
                    let next = self.viewer.current_page + 1;
                    self.set_page(next, cx);
                    return;
                }
                "pageup" | "left" => {
                    if self.viewer.current_page > 0 {
                        let previous = self.viewer.current_page - 1;
                        self.set_page(previous, cx);
                    }
                    return;
                }
                "home" => {
                    self.set_page(0, cx);
                    return;
                }
                "end" => {
                    let last = self.viewer.page_count().saturating_sub(1);
                    self.set_page(last, cx);
                    return;
                }
                "h" => {
                    self.add_highlight(cx);
                    return;
                }
                "escape" => {
                    self.viewer.selection.clear();
                    self.outline_open = false;
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }

        if self.viewer.search.open
            && !control
            && let Some(key_char) = &keystroke.key_char
            && !key_char.chars().all(char::is_control)
        {
            self.viewer.search.query.push_str(key_char);
            self.recompute_matches(cx);
            cx.notify();
        }
    }
}

impl Focusable for AppState {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 窗口尺寸只能在拿到 Window 的地方改；改完等平台的 resize 事件再重算缩放。
        self.apply_pending_window_resize(window, &**cx);

        let viewport = window.viewport_size();
        let viewport_changed = self.viewport != viewport;
        self.viewport = viewport;
        self.scale_factor = window.scale_factor();

        if viewport_changed && self.viewer.has_document() && self.refit() {
            self.request_render();
        }

        let theme = self.theme.clone();

        let content: gpui::AnyElement = if self.viewer.has_document() {
            self.page_content(&theme, cx).into_any_element()
        } else {
            self.home_view(&theme, cx).into_any_element()
        };

        let mut root = div()
            .id("root")
            .role(gpui::Role::Document)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_family.clone())
            .child(content);

        // 首页整条顶栏已经是拖拽区；阅读器仍只保留顶部 14px 的拖拽条。
        if self.viewer.has_document() {
            root = root.child(crate::ui::chrome::drag_strip());
        }

        if let Some(toast) = self.toast(&theme) {
            root = root.child(toast);
        }

        root = root.child(self.window_controls(&theme));

        if self.viewer.has_document() {
            root = root
                .child(self.floating_top(&theme, cx))
                .child(self.floating_bottom(&theme, cx));
        }

        if let Some(panel) = self.outline_panel(&theme, cx) {
            root = root.child(panel);
        }
        if self.viewer.search.open {
            root = root.child(self.search_overlay(&theme));
        }

        root
    }
}
