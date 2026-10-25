//! 文档生命周期：打开/关闭文档、恢复与保存阅读进度、高亮与页边笺注 sidecar、全文检索
//! 会话，以及词典资源的加载与查词。
//!
//! 边界是「资源状态机」：什么被打开、上次读到哪、关闭前要写什么。这里会直接
//! 产生磁盘与引擎副作用（`Command::Open` / `Command::Search`、sidecar 写入），
//! 但不做布局换算、不发渲染请求（见 `render`），也不处理窗口尺寸（见 `window`）
//! 与输入分发（见 `events`）。

use std::path::PathBuf;
use std::time::Duration;

use dict_core::{Command as DictCommand, DictionaryConfig};
use gpui::{Context, PathPromptOptions, Window};
use pdf_core::{Command, DocumentMetadata, OutlineItem, PageHits, PageSize};

use crate::ai_query::AiPhase;
use crate::dict_suggest::{MAX_SUGGESTIONS, SUGGEST_DEBOUNCE};
use crate::dictionary::{DictLoadState, normalize_term, summarize_entries};
use crate::highlights::sidecar_path;
use crate::progress::{ReadingProgress, unix_now};
use crate::viewer::Document;

use super::FocusTarget;

/// 页内滚动没有独立事件，用固定间隔采样阅读位置。
pub(super) const PROGRESS_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// 连续滚动时合并写盘，避免每一帧都碰磁盘。
const PROGRESS_FLUSH_DELAY: Duration = Duration::from_millis(800);
/// 手动输入查词时，活动笺注卡在页内的默认纵向位置（PDF pt，距页顶）。
const MANUAL_ANCHOR_TOP: f32 = 24.0;

impl super::AppState {
    // ---------- 打开与关闭 ----------

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

    /// 清空首页筛选（内含让输入框同步清空）。
    pub(crate) fn clear_home_query(&mut self, cx: &mut Context<Self>) {
        self.home_input.update(cx, |input, cx| input.clear(cx));
        self.home.query.clear();
        self.home.search_focused = false;
        self.home.menu_open = None;
        cx.notify();
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

    /// 引擎报告文档已打开：重置会话状态、恢复进度、加载高亮与页边笺注。
    pub(super) fn on_document_opened(
        &mut self,
        path: PathBuf,
        pages: Vec<PageSize>,
        outline: Vec<OutlineItem>,
        metadata: DocumentMetadata,
        cx: &mut Context<Self>,
    ) {
        // 切换文档前先记下上一份文档的最后位置，record 会顺带落盘。
        self.capture_progress();

        let count = pages.len();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        // 首页与窗口标题优先用 PDF 的 Title，缺失时回退文件名。
        let display_title = metadata.title_or(&name).to_string();
        let saved = self.home.progress_for(&path);

        self.home.record(path.clone(), display_title.clone());
        self.home.query.clear();
        self.home.search_focused = false;
        self.dictionary.clear_results();
        self.dictionary.open = false;
        self.dict_input.update(cx, |input, cx| input.clear(cx));
        self.search_input.update(cx, |input, cx| input.clear(cx));
        self.panning = None;
        // 卡片高度是上一份文档实测的，换文档后重新测量。
        self.gloss_card_heights.clear();
        // 换文档时重置胶囊可见性：新文档先亮着让用户看见按钮。
        self.chrome = crate::chrome_visibility::ChromeVisibility::new();

        let initial_area = self.fit_area();
        self.viewer.reset(
            Document {
                path,
                pages,
                outline,
                metadata,
            },
            initial_area,
        );

        // 先加载 sidecar（可能含已钉住的页边笺注，会影响 fit_area 预留的页边栏宽度）。
        if let Some(doc) = &self.viewer.doc {
            let path = sidecar_path(&doc.path);
            self.viewer.highlights.load(&path);
        }

        let area = self.fit_area();
        let _ = self.viewer.fit(self.viewer.fit_mode, area);
        let restored = saved.is_some_and(|progress| self.viewer.restore_progress(&progress, area));
        self.apply_pending_scroll();
        self.refit();

        // 等 render 拿到当前显示器工作区后，按这张纸的比例调整窗口；
        // 随后视口变化会再按适宽/适页模式重算缩放。
        self.pending_window_fit = self.viewer.page_size_at(0).filter(|page| {
            page.width.is_finite()
                && page.height.is_finite()
                && page.width > 0.0
                && page.height > 0.0
        });
        self.pending_window_resize = None;
        self.last_progress = if restored { saved } else { None };
        self.window_title = format!("{display_title} · 笺释");
        self.window_title_dirty = true;

        self.ensure_visible_pages();
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

    pub(crate) fn close_document(&mut self, cx: &mut Context<Self>) {
        self.flush_progress();
        self.viewer.close();
        // 让 PDFium worker 释放文档；首页缩略图 worker 随即可以继续。
        let _ = self.engine.commands.send(Command::Close);
        self.panning = None;
        self.page_input_open = false;
        self.search_input.update(cx, |input, cx| input.clear(cx));
        self.dict_input.update(cx, |input, cx| input.clear(cx));
        self.pending_focus = Some(FocusTarget::Root);
        self.last_progress = None;
        self.outline_open = false;
        self.settings_open = false;
        self.window_title = "笺释".into();
        self.window_title_dirty = true;
        self.dictionary.clear_results();
        self.dictionary.open = false;
        self.gloss_card_heights.clear();
        self.home.menu_open = None;
        self.home.search_focused = false;
        self.home.query.clear();
        // 回到首页时先看最近阅读，而不是上次停留的设置页。
        self.home.nav = crate::home::HomeNav::Home;
        self.home_input.update(cx, |input, cx| input.clear(cx));
        self.pending_window_fit = None;
        self.pending_window_resize = self.reader_window_size.take();
        cx.notify();
    }

    // ---------- 阅读进度 ----------

    /// 采样当前阅读位置；有变化时更新内存记录并安排延迟写盘。
    pub(super) fn sample_progress(&mut self, cx: &mut Context<Self>) {
        if self.capture_progress() {
            self.schedule_progress_flush(cx);
        }
    }

    /// 把当前页、页内滚动比例和缩放写进最近阅读（只改内存）。
    pub(super) fn capture_progress(&mut self) -> bool {
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
        let layout = self.doc_layout()?;
        let (page, ratio_y) = layout.anchor_at(self.scroll_top(), f32::from(self.viewport.height));
        let zoom = if self.viewer.zoom.is_finite() {
            self.viewer.zoom
        } else {
            1.0
        };
        Some(ReadingProgress {
            page,
            pages: doc.pages.len(),
            zoom,
            fit: Some(self.viewer.fit_mode),
            scroll_x: self.scroll_ratio_x(&layout),
            scroll_y: ratio_y,
            updated_at: unix_now(),
        })
    }

    /// 立即把进度落盘（切页、缩放、回到首页、退出时调用）。
    pub(crate) fn flush_progress(&mut self) {
        self.capture_progress();
        self.home.flush();
    }

    pub(super) fn schedule_progress_flush(&mut self, cx: &mut Context<Self>) {
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

    // ---------- 高亮与页边笺注 ----------

    pub(crate) fn add_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.viewer.selection.page else {
            self.set_status("先选中要标注的文本", cx);
            cx.notify();
            return;
        };
        let chars = self.viewer.chars_of(page);
        let rects = self.viewer.selection.rects(chars);
        if rects.is_empty() {
            self.set_status("先选中要标注的文本", cx);
            cx.notify();
            return;
        }

        let text = self.viewer.selection.text(chars).unwrap_or_default();
        let added = self.viewer.highlights.add(page, rects, text).is_some();
        if added {
            let key = if cfg!(target_os = "macos") { "⌘S" } else { "Ctrl+S" };
            self.set_status(format!("已添加高亮，{key} 保存"), cx);
        }
        cx.notify();
    }

    /// 批注侧栏开关。
    pub(crate) fn toggle_highlight_panel(&mut self, cx: &mut Context<Self>) {
        self.highlight_panel_open = !self.highlight_panel_open;
        if self.highlight_panel_open {
            self.outline_open = false;
        }
        cx.notify();
    }

    /// 删除一条高亮或页边笺注并立即写盘，避免下次打开又冒出来。
    pub(crate) fn delete_highlight(&mut self, id: u64, cx: &mut Context<Self>) {
        let had_rail = self.has_gloss_rail();
        if !self.viewer.highlights.remove(id) {
            return;
        }
        if had_rail != self.has_gloss_rail() {
            self.refit();
            self.ensure_visible_pages();
        }
        match self.persist_highlights() {
            Ok(()) => self.set_status("已移除批注", cx),
            Err(error) => self.set_status(format!("移除后保存失败: {error}"), cx),
        }
        cx.notify();
    }

    /// 钉住或取消钉住当前激活态查词卡中的词条，并立即落盘到 sidecar。
    pub(crate) fn toggle_pin_current_gloss(&mut self, cx: &mut Context<Self>) {
        let word = self.dictionary.word.trim().to_string();
        if word.is_empty() {
            return;
        }
        let page = self.dictionary.anchor_page;
        if let Some(existing) = self.viewer.highlights.find_gloss(page, &word) {
            let id = existing.id;
            self.delete_highlight(id, cx);
            return;
        }

        let (phonetic, summary) = summarize_entries(&self.dictionary.entries, &word);
        // 只有当前词确实对应正文选区时才沿用高亮坐标；手动输入、词条内链接
        // 或前缀建议没有正文锚点，钉住时不能把上一个词的框画到这个词身上。
        let rects = self
            .dictionary
            .anchor_rects_for(&word)
            .map(<[pdf_core::RectPt]>::to_vec)
            .unwrap_or_default();
        let anchor_top = self.dictionary.anchor_top;
        let had_rail = self.has_gloss_rail();

        if self
            .viewer
            .highlights
            .add_gloss(
                page,
                rects,
                anchor_top,
                word.clone(),
                phonetic,
                summary,
                false,
                String::new(),
            )
            .is_some()
        {
            if self.refit() || had_rail != self.has_gloss_rail() {
                self.ensure_visible_pages();
                self.flush_progress();
            }
            match self.persist_highlights() {
                Ok(()) => self.set_status(format!("已钉住页边笺注「{word}」"), cx),
                Err(error) => self.set_status(format!("笺注保存失败: {error}"), cx),
            }
            cx.notify();
        }
    }

    /// 把当前 AI 答案钉成页边笺注。
    ///
    /// 长句也允许：高亮按选区矩形画（`PinnedGloss.rects` 本来就是矩形列表）。
    /// 紧凑卡展示模型给出的「标题」而不是截断正文，避免折叠态读到半句话。
    pub(crate) fn pin_current_ai(&mut self, cx: &mut Context<Self>) {
        let text = match &self.ai.phase {
            AiPhase::Done { text, .. } => text.clone(),
            AiPhase::Streaming { text, .. } if !text.trim().is_empty() => text.clone(),
            _ => return,
        };
        let sections = ai_core::parse_answer_sections(&text);
        if sections.is_empty() {
            return;
        }
        let word = self.ai.term.trim().to_string();
        if word.is_empty() {
            return;
        }
        let page = self.ai.page;
        let rects = self.ai.rects.clone();
        let anchor_top = rects
            .first()
            .and_then(|rect| {
                self.viewer
                    .page_size_at(page)
                    .map(|size| (size.height - rect.top).max(0.0))
            })
            .unwrap_or(0.0);
        let answer = text.trim().to_string();
        let summary = if sections.title.is_empty() {
            crate::ui::ai_markdown::plain_text(&sections.content)
                .chars()
                .take(200)
                .collect()
        } else {
            sections.title
        };

        // `add_gloss` 自己按「同页同词」原地更新，不会堆两条重复笺注。
        let had_rail = self.has_gloss_rail();
        if self
            .viewer
            .highlights
            .add_gloss(
                page,
                rects,
                anchor_top,
                word.clone(),
                None,
                summary,
                true,
                answer,
            )
            .is_none()
        {
            return;
        }
        if self.refit() || had_rail != self.has_gloss_rail() {
            self.ensure_visible_pages();
            self.flush_progress();
        }
        match self.persist_highlights() {
            Ok(()) => self.set_status(format!("已钉住「{word}」"), cx),
            Err(error) => self.set_status(format!("笺注保存失败: {error}"), cx),
        }
        cx.notify();
    }

    /// 点击已钉住的紧凑笺注卡（或正文下划线词条）：在原锚点位置展开为完整查词卡。
    pub(crate) fn open_pinned_gloss(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(gloss) = self
            .viewer
            .highlights
            .glosses
            .iter()
            .find(|g| g.id == id)
            .cloned()
        else {
            return;
        };

        // AI 钉下来的笺注没有词典条目：直接复用存好的答案，
        // 走词典链路只会「查不到 → 触发一次新的 AI 查询」，等于重复计费。
        if gloss.from_ai {
            self.close_dictionary(cx);
            self.ai.term = gloss.word.clone();
            self.ai.page = gloss.page;
            self.ai.rects = gloss.rects.clone();
            self.ai.phase = AiPhase::Done {
                text: if gloss.answer.is_empty() {
                    gloss.summary.clone()
                } else {
                    gloss.answer.clone()
                },
                model: String::new(),
            };
            cx.notify();
            return;
        }

        self.dictionary
            .set_anchor_for_word(gloss.page, gloss.rects, gloss.anchor_top, &gloss.word);
        self.dict_input
            .update(cx, |input, cx| input.set_value(gloss.word.clone(), cx));
        self.lookup_word(gloss.word, cx);
    }

    /// 写 sidecar，不弹提示。
    fn persist_highlights(&self) -> Result<(), String> {
        let Some(doc) = &self.viewer.doc else {
            return Ok(());
        };
        let path = sidecar_path(&doc.path);
        self.viewer
            .highlights
            .save(&path)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn save_highlights(&mut self, cx: &mut Context<Self>) {
        if self.viewer.highlights.is_empty() {
            self.set_status("还没有高亮或页边笺注", cx);
            cx.notify();
            return;
        }

        match self.persist_highlights() {
            Ok(()) => {
                let path = self
                    .viewer
                    .doc
                    .as_ref()
                    .map(|doc| sidecar_path(&doc.path))
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                self.set_status(format!("批注与页边笺注已保存到 {path}"), cx);
            }
            Err(error) => self.set_status(format!("保存批注失败: {error}"), cx),
        }
        cx.notify();
    }

    // ---------- 全文检索会话 ----------

    /// 打开搜索浮层并把焦点交给输入框。
    pub(crate) fn open_search(&mut self, cx: &mut Context<Self>) {
        self.viewer.search.open = true;
        self.outline_open = false;
        let value = self.viewer.search.query.clone();
        self.search_input.update(cx, |input, cx| {
            input.set_value(value, cx);
            input.select_all(cx);
        });
        self.pending_focus = Some(FocusTarget::Search);
        cx.notify();
    }

    /// 关闭搜索：清空查询、把焦点还给页面，页面快捷键恢复。
    pub(crate) fn close_search(&mut self, cx: &mut Context<Self>) {
        self.viewer.search.close();
        self.search_input.update(cx, |input, cx| input.clear(cx));
        self.pending_focus = Some(FocusTarget::Root);
        cx.notify();
    }

    /// 查询串变化：逐页缓存全部作废，已渲染的页立刻重算，再请 worker 统计全文。
    pub(super) fn on_search_query_changed(&mut self, cx: &mut Context<Self>) {
        self.viewer.search.clear_page_matches();
        for page in self.viewer.cached_pages() {
            self.refresh_matches(page);
        }
        self.refresh_matches(self.viewer.current_page);
        self.request_doc_search(cx);
    }

    fn request_doc_search(&mut self, cx: &mut Context<Self>) {
        let query = self.viewer.search.query.clone();
        self.search_request_id = self.search_request_id.wrapping_add(1);

        if query.trim().is_empty() {
            self.viewer.search.clear_results();
            cx.notify();
            return;
        }

        self.viewer.search.begin_scan();
        let _ = self.engine.send(Command::Search {
            query,
            request: self.search_request_id,
        });
        cx.notify();
    }

    /// 收到全文搜索结果。`request` 或查询串过期时直接丢弃。
    pub(super) fn on_search_results(
        &mut self,
        request: u64,
        query: String,
        pages: Vec<PageHits>,
        total: usize,
        cx: &mut Context<Self>,
    ) {
        if request != self.search_request_id || query != self.viewer.search.query {
            return;
        }
        self.viewer.search.set_doc_hits(pages, total);
        self.viewer.search.rewind_to(self.viewer.current_page);
        log::debug!(
            "full-text search for {:?}: {} hits on {} pages",
            query,
            total,
            self.viewer.search.pages.len()
        );
        cx.notify();
    }

    /// 收到扫描进度；过期请求（用户已改查询）直接丢弃。
    pub(super) fn on_search_progress(&mut self, request: u64, scanned: usize, total: usize) {
        if request != self.search_request_id {
            return;
        }
        self.viewer.search.update_scan(scanned, total);
    }

    pub(super) fn next_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some((page, _local)) = self.viewer.search.next_doc(forward) {
            if page != self.viewer.current_page {
                self.set_page(page, cx);
            } else {
                self.recompute_matches(cx);
            }
            cx.notify();
            return;
        }

        if self.viewer.search.next(forward) {
            cx.notify();
            return;
        }

        if !self.viewer.search.has_query() {
            return;
        }
        self.set_status("没有匹配", cx);
        cx.notify();
    }

    // ---------- 词典资源与页边笺注交互 ----------

    /// 输入变化后防抖请求前缀建议；每次输入都会推进代次号，过期结果直接丢弃。
    pub(super) fn request_suggestions(&mut self, raw: String, cx: &mut Context<Self>) {
        let term = normalize_term(&raw);
        self.dictionary.suggest_request = self.dictionary.suggest_request.wrapping_add(1);
        let request = self.dictionary.suggest_request;
        self.dictionary.suggestions.clear();
        cx.notify();

        if term.is_empty() || !matches!(self.dictionary.load_state, DictLoadState::Ready { .. }) {
            return;
        }

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SUGGEST_DEBOUNCE).await;
            let _ = this.update(cx, |this, _cx| {
                if this.dictionary.suggest_request != request {
                    return;
                }
                let _ = this.dict.commands.send(DictCommand::Prefix {
                    request,
                    term,
                    limit: MAX_SUGGESTIONS * 3,
                });
            });
        })
        .detach();
    }

    /// 手动输入或以建议/历史词查词：这类查询没有正文选区，先清掉旧锚点。
    ///
    /// 锚点不清会让「钉住」把上一个词的高亮框画到新词身上（手动查 banana 却沿用
    /// 双击 apple 时的坐标）。这里把卡放回当前页顶部，并让后续钉住不带高亮。
    fn lookup_manually(&mut self, term: String, cx: &mut Context<Self>) {
        let page = self.viewer.current_page;
        self.dictionary
            .set_anchor(page, Vec::new(), MANUAL_ANCHOR_TOP);
        self.lookup_word(term, cx);
    }

    /// 在查词输入框按 Enter 或点「查」按钮提交查询。
    pub(crate) fn submit_dict_query(&mut self, cx: &mut Context<Self>) {
        let term = self.dict_input.read(cx).value().trim().to_string();
        if term.is_empty() {
            return;
        }
        self.pending_focus = Some(FocusTarget::Root);
        self.lookup_manually(term, cx);
    }

    /// 点击建议词或历史词：回填输入框并立即查词。
    pub(crate) fn query_dict_term(&mut self, term: String, cx: &mut Context<Self>) {
        self.dict_input
            .update(cx, |input, cx| input.set_value(term.clone(), cx));
        self.pending_focus = Some(FocusTarget::Root);
        self.lookup_manually(term, cx);
    }

    /// 点击释义内 `entry://` 链接：把当前词压入返回栈，保持当前页锚点不变并跳转查词。
    pub(crate) fn lookup_word_from_link(&mut self, term: String, cx: &mut Context<Self>) {
        let prev = self.dictionary.word.trim().to_string();
        let next = normalize_term(&term);
        if !prev.is_empty() && !prev.eq_ignore_ascii_case(&next) {
            self.dictionary.back_stack.push(prev);
        }
        self.dict_input
            .update(cx, |input, cx| input.set_value(next.clone(), cx));
        self.lookup_word(next, cx);
    }

    /// 从 `entry://` 跨词条跳转后点「‹ 返回」回到上一个词条。
    pub(crate) fn navigate_dict_back(&mut self, cx: &mut Context<Self>) {
        let Some(prev) = self.dictionary.back_stack.pop() else {
            return;
        };
        self.dict_input
            .update(cx, |input, cx| input.set_value(prev.clone(), cx));
        self.lookup_word(prev, cx);
    }

    /// 工具栏词典按钮或 `Ctrl+Shift+D`：在当前页开启或收起页边查词卡。
    ///
    /// 活动卡只是浮层，不参与笺注栏宽度计算，所以开关它不重排、不重新缩放。
    pub(crate) fn toggle_gloss_card(&mut self, cx: &mut Context<Self>) {
        if self.dictionary.open {
            self.close_dictionary(cx);
            return;
        }

        if !self.settings.current.has_enabled_dictionary() {
            self.settings_open = true;
            self.set_status("请先在设置中添加并启用 MDX 词典", cx);
            cx.notify();
            return;
        }

        let page = self.viewer.current_page;
        // 若还没有锚定在当前页，则把查词卡放在当前页靠上的舒适位置，并聚焦输入框。
        if self.dictionary.anchor_page != page || self.dictionary.word.is_empty() {
            self.dictionary
                .set_anchor(page, Vec::new(), MANUAL_ANCHOR_TOP);
        }
        self.dictionary.open = true;
        if self.refit() {
            self.ensure_visible_pages();
        }
        self.pending_focus = Some(FocusTarget::Dictionary);
        cx.notify();
    }

    /// 收起当前激活态页边查词卡（已钉住的紧凑笺注卡继续保留在页边）。
    pub(crate) fn close_dictionary(&mut self, cx: &mut Context<Self>) {
        if !self.dictionary.open {
            return;
        }
        self.dictionary.open = false;
        self.dictionary.suggestions.clear();
        self.dictionary.show_history = false;
        self.pending_focus = Some(FocusTarget::Root);
        cx.notify();
    }

    pub(super) fn lookup_word(&mut self, term: String, cx: &mut Context<Self>) {
        let term = normalize_term(&term);
        if term.is_empty() {
            return;
        }

        // 回车/点击确认后不再展示建议，并让尚未返回的联想结果失效。
        self.dictionary.suggest_request = self.dictionary.suggest_request.wrapping_add(1);
        self.dictionary.suggestions.clear();
        self.dictionary.show_history = false;

        if !self.settings.current.has_enabled_dictionary() {
            self.settings_open = true;
            self.set_status("请先在设置中添加并启用 MDX 词典", cx);
            cx.notify();
            return;
        }

        self.dict_history.record(&term);

        if self.dict_input.read(cx).value() != term {
            let value = term.clone();
            self.dict_input
                .update(cx, |input, cx| input.set_value(value, cx));
        }

        match self.dictionary.load_state.clone() {
            DictLoadState::Loading => {
                self.dictionary.pending_term = Some(term);
                self.dictionary.open = true;
                self.set_status("词典正在加载…", cx);
                cx.notify();
                return;
            }
            DictLoadState::Unconfigured => {
                self.settings_open = true;
                self.set_status("请先在设置中添加并启用 MDX 词典", cx);
                cx.notify();
                return;
            }
            DictLoadState::Failed(message) => {
                self.dictionary.error = Some(message.clone());
                self.dictionary.open = true;
                self.set_status(message, cx);
                cx.notify();
                return;
            }
            DictLoadState::Ready { .. } => {}
        }

        self.dictionary.request = self.dictionary.request.wrapping_add(1);
        let request = self.dictionary.request;
        self.dictionary.word = term.clone();
        self.dictionary.loading = true;
        self.dictionary.not_found = false;
        self.dictionary.error = None;
        self.dictionary.entries.clear();
        self.dictionary.pending_term = None;
        self.dictionary.open = true;
        if self.refit() {
            self.ensure_visible_pages();
        }

        if let Err(error) = self
            .dict
            .commands
            .send(DictCommand::Lookup { request, term })
        {
            self.dictionary.loading = false;
            self.dictionary.error = Some(format!("词典线程不可用: {error}"));
        }
        cx.notify();
    }

    pub(crate) fn choose_dictionary(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("添加 MDX 词典".into()),
        });

        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && !paths.is_empty()
            {
                let _ = this.update(cx, |this, cx| this.add_dictionary_paths(paths, cx));
            }
        })
        .detach();
    }

    fn add_dictionary_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let mut added = 0;
        let mut rejected = 0;
        for path in paths {
            let is_mdx = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("mdx"));
            if !is_mdx || !path.is_file() {
                rejected += 1;
                continue;
            }
            if self
                .settings
                .current
                .dictionaries
                .iter()
                .any(|dictionary| dictionary.path == path)
            {
                rejected += 1;
                continue;
            }
            self.settings
                .current
                .dictionaries
                .push(crate::settings::DictionarySetting {
                    path,
                    enabled: true,
                });
            added += 1;
        }

        if added == 0 {
            let message = if rejected > 0 {
                "请选择未添加过的 .mdx 词典文件"
            } else {
                "没有添加词典"
            };
            self.set_status(message, cx);
            cx.notify();
            return;
        }

        self.configure_dictionaries(cx);
        self.set_status(format!("已添加 {added} 本词典，正在加载…"), cx);
    }

    pub(crate) fn remove_dictionary(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.settings.current.dictionaries.len() {
            return;
        }
        self.settings.current.dictionaries.remove(index);
        self.configure_dictionaries(cx);
        self.set_status("已移除词典", cx);
    }

    pub(crate) fn move_dictionary(&mut self, index: usize, delta: isize, cx: &mut Context<Self>) {
        let len = self.settings.current.dictionaries.len();
        let Some(target) = index.checked_add_signed(delta) else {
            return;
        };
        if index >= len || target >= len {
            return;
        }
        self.settings.current.dictionaries.swap(index, target);
        self.configure_dictionaries(cx);
    }

    pub(crate) fn toggle_dictionary(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(dictionary) = self.settings.current.dictionaries.get_mut(index) else {
            return;
        };
        dictionary.enabled = !dictionary.enabled;
        self.configure_dictionaries(cx);
    }

    /// 把设置里的完整词典列表推给 worker；加载状态先回到「加载中」。
    fn configure_dictionaries(&mut self, cx: &mut Context<Self>) {
        self.settings.current.migrate();
        self.settings.save();

        self.dictionary.configure_request = self.dictionary.configure_request.wrapping_add(1);
        let request = self.dictionary.configure_request;
        self.dictionary.statuses = self
            .settings
            .current
            .dictionaries
            .iter()
            .map(|dictionary| crate::dictionary::DictStatus {
                path: dictionary.path.clone(),
                state: if dictionary.enabled {
                    DictLoadState::Loading
                } else {
                    DictLoadState::Unconfigured
                },
            })
            .collect();

        let enabled = self.settings.current.has_enabled_dictionary();
        self.dictionary.load_state = if enabled {
            DictLoadState::Loading
        } else {
            DictLoadState::Unconfigured
        };
        self.dictionary.pending_term = None;

        let dictionaries: Vec<_> = self
            .settings
            .current
            .dictionaries
            .iter()
            .map(|dictionary| DictionaryConfig {
                path: dictionary.path.clone(),
                enabled: dictionary.enabled,
            })
            .collect();
        let _ = self.dict.commands.send(DictCommand::Configure {
            request,
            dictionaries,
        });
        self.dictionary.clear_results();
        cx.notify();
    }

    pub(crate) fn clear_dict_history(&mut self, cx: &mut Context<Self>) {
        self.dict_history.clear();
        self.set_status("已清空查词历史", cx);
        cx.notify();
    }

    pub(crate) fn clear_dictionary(&mut self, cx: &mut Context<Self>) {
        self.settings.current.dictionaries.clear();
        self.settings.current.dictionary_path = None;
        self.settings.save();
        let _ = self.dict.commands.send(DictCommand::Unload);
        self.dictionary.statuses.clear();
        self.dictionary.clear_results();
        self.dictionary.load_state = DictLoadState::Unconfigured;
        self.set_status("已清除全部词典", cx);
        cx.notify();
    }
}
