//! 应用控制器：把 PDF 引擎事件、键盘/鼠标输入接到 Viewer 状态和 UI 上。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gpui::{
    App, ClipboardItem, Context, FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PathPromptOptions, Point, Render, RenderImage, Size, Window, div,
    point, prelude::*, px,
};
use pdf_core::{Command, Event, LinkTarget, PdfEngine};

use crate::highlights::sidecar_path;
use crate::theme::Theme;
use crate::viewer::{Document, PageData, Viewer};

pub(crate) const PAGE_PAD: f32 = 28.0;
pub(crate) const BOTTOM_INSET: f32 = 88.0;

pub struct AppState {
    pub(crate) theme: Theme,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) engine: std::sync::mpsc::Sender<Command>,
    pub(crate) viewer: Viewer,
    pub(crate) viewport: Size<gpui::Pixels>,
    pub(crate) scale_factor: f32,
    pub(crate) request_id: u64,
    pub(crate) outline_open: bool,
    pub(crate) status: String,
    pub(crate) status_version: u64,
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

        Self {
            theme: Theme::dark(),
            focus_handle: cx.focus_handle(),
            engine,
            viewer: Viewer::new(),
            viewport: Size {
                width: px(1040.),
                height: px(720.),
            },
            scale_factor: 1.0,
            request_id: 0,
            outline_open: false,
            status: String::new(),
            status_version: 0,
        }
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
            cx.background_executor()
                .timer(Duration::from_secs(4))
                .await;

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
        let Some(size) = self.viewer.page_size() else {
            return;
        };
        let _ = size;
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
        self.request_render();
        self.recompute_matches(cx);
        cx.notify();
    }

    pub(crate) fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.viewer.set_zoom(zoom);
        self.request_render();
        cx.notify();
    }

    pub(crate) fn fit_to_width(&mut self) {
        let available = f32::from(self.viewport.width) - PAGE_PAD * 2.0;
        self.viewer.fit_to_width(available);
    }

    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Opened {
                path,
                pages,
                outline,
            } => {
                let count = pages.len();
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());

                let available = f32::from(self.viewport.width) - PAGE_PAD * 2.0;
                self.viewer.reset(
                    Document {
                        path,
                        pages,
                        outline,
                    },
                    available,
                );

                if let Some(doc) = &self.viewer.doc {
                    let path = sidecar_path(&doc.path);
                    self.viewer.highlights.load(&path);
                }

                self.request_render();
                self.set_status(format!("{name} · {count} 页"), cx);
                log::info!(
                    "opened {} ({} pages, {} outline items)",
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
            px(PAGE_PAD + f32::from(offset.y)),
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
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.set_status(format!("已复制 {count} 个字符"), cx);
                log::info!("copied {count} chars to clipboard");
            }
            _ => {
                self.set_status("没有选中文本", cx);
            }
        }
        cx.notify();
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
        self.viewer.selection.dragging = false;
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

    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                    self.fit_to_width();
                    let zoom = self.viewer.zoom;
                    self.set_zoom(zoom, cx);
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
        self.viewport = window.viewport_size();
        self.scale_factor = window.scale_factor();
        let theme = self.theme.clone();

        let content: gpui::AnyElement = if self.viewer.has_document() {
            self.page_content(&theme, cx).into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .child(crate::ui::empty::empty_state(&theme, cx))
                .into_any_element()
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
            .child(content)
            .child(crate::ui::chrome::drag_strip());

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
