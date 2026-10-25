//! 一次 AI 查词的运行态：状态机、LRU 缓存、任务与节流。
//!
//! 网络部分在 `ai-core::run_lookup` 里跑（`cx.background_spawn`），它把事件推进
//! 一个 `mpsc`；前台任务负责把事件落到状态上并按节流重绘。取消 = drop 掉这里的
//! `Task`（接收端随之丢弃，`run_lookup` 发现发不出去就提前退出），再用
//! `request_id` 把迟到的回包挡掉。

use std::time::{Duration, Instant};

use ai_core::{ErrorKind, LookupContext, LookupEvent};
use futures::FutureExt as _;
use futures::StreamExt;
use futures::future::Either;
use gpui::{Context, Task};

/// 缓存条目上限；古文阅读里同一个词反复出现的概率很高。
pub(crate) const AI_CACHE_LIMIT: usize = 50;
/// 流式刷新间隔与字符阈值，先满足哪个都刷一次。
const NOTIFY_INTERVAL: Duration = Duration::from_millis(16);
const NOTIFY_CHARS: usize = 20;
/// 单次查询的总时限。
///
/// 轮次上限已经放到 100（等于不设限），超时就变成唯一的时间闸门：给得宽松些，
/// 让「翻好几页追出处」这类长查询能跑完；真正的成本护栏是页预算。
const AI_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolLine {
    pub name: String,
    pub args: String,
}

impl ToolLine {
    /// 卡片里只显示「名字 + 参数」，不显示工具返回（整页文本会毁掉排版）。
    pub fn label(&self) -> String {
        if self.args.is_empty() || self.args == "null" || self.args == "{}" {
            self.name.clone()
        } else {
            format!("{}({})", self.name, self.args)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AiPhase {
    /// 卡片不显示。
    Idle,
    /// 已发出、还没有任何 token。
    Thinking,
    Streaming {
        text: String,
        tool: Option<ToolLine>,
    },
    Done {
        text: String,
        model: String,
    },
    Failed {
        kind: ErrorKind,
        message: String,
    },
}

impl AiPhase {
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
}

pub(crate) struct AiQueryState {
    pub phase: AiPhase,
    /// 当前（或上一次）查询的选区标签，重试与钉住时用。
    pub term: String,
    /// 这次查询对应的页与选区矩形；钉住长句时用它画高亮。
    pub page: usize,
    pub rects: Vec<pdf_core::RectPt>,
    request_id: u64,
    task: Option<Task<()>>,
    /// 最近使用在前；key 里带文档与模型，换书换模型自然失效。
    cache: Vec<(String, String)>,
    last_flush: Instant,
    pending_chars: usize,
}

impl Default for AiQueryState {
    fn default() -> Self {
        Self {
            phase: AiPhase::Idle,
            term: String::new(),
            page: 0,
            rects: Vec::new(),
            request_id: 0,
            task: None,
            cache: Vec::new(),
            last_flush: Instant::now(),
            pending_chars: 0,
        }
    }
}

impl AiQueryState {
    fn cache_get(&mut self, key: &str) -> Option<String> {
        let index = self.cache.iter().position(|(k, _)| k == key)?;
        let entry = self.cache.remove(index);
        let value = entry.1.clone();
        self.cache.insert(0, entry);
        Some(value)
    }

    fn cache_put(&mut self, key: String, value: String) {
        self.cache.retain(|(k, _)| k != &key);
        self.cache.insert(0, (key, value));
        self.cache.truncate(AI_CACHE_LIMIT);
    }

    /// 是否该重绘：时间够了或字符积到阈值。
    fn should_flush(&mut self) -> bool {
        if self.pending_chars >= NOTIFY_CHARS || self.last_flush.elapsed() >= NOTIFY_INTERVAL {
            self.pending_chars = 0;
            self.last_flush = Instant::now();
            true
        } else {
            false
        }
    }
}

impl crate::app::AppState {
    pub(crate) fn ai_card_visible(&self) -> bool {
        !self.ai.phase.is_idle()
    }

    /// 卡片标题用的短标签：折叠空白，超长截断（长句子在 340px 里放不下）。
    fn selection_label(text: &str) -> String {
        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let label: String = collapsed.chars().take(40).collect();
        if label.chars().count() < collapsed.chars().count() {
            format!("{label}…")
        } else {
            label
        }
    }

    /// 缓存 key：同一次会话里换书 / 换供应商 / 换模型都不该命中。
    fn ai_cache_key(&self, term: &str) -> String {
        let path = self
            .viewer
            .doc
            .as_ref()
            .map(|doc| doc.path.display().to_string())
            .unwrap_or_default();
        let provider = self
            .settings
            .current
            .ai
            .active_provider()
            .map(|entry| entry.id.clone())
            .unwrap_or_default();
        let model = self.settings.current.ai.lookup.model.trim();
        format!("{path}|{term}|{provider}|{model}")
    }

    /// 发起一次 AI 查词（词）。等价于 [`Self::start_ai_lookup_text`]。
    pub(crate) fn start_ai_lookup(&mut self, term: String, cx: &mut Context<Self>) {
        self.start_ai_lookup_text(term, cx);
    }

    /// 发起一次 AI 查询：`text` 可能是词，也可能是一整段选中的文字。
    ///
    /// 词与段落的差别只体现在提示词与标题上，链路其余部分完全一致。
    pub(crate) fn start_ai_lookup_text(&mut self, text: String, cx: &mut Context<Self>) {
        let settings = self.settings.current.ai.clone();
        let label = Self::selection_label(&text);
        if let Err(error) = settings.validate() {
            self.ai.term = label;
            self.ai.phase = AiPhase::Failed {
                kind: ErrorKind::Other,
                message: error.to_string(),
            };
            cx.notify();
            return;
        }
        let Some(snapshot) = self.lookup_snapshot() else {
            return;
        };

        // 先记下「这段话在纸上的哪儿」：笺注栏靠 `ai.page` 决定在哪一页出现，
        // 缓存命中会提前 return，所以必须在查缓存之前就写好。
        if let Some(capsule) = &self.selection_capsule {
            self.ai.page = capsule.page;
            self.ai.rects = capsule.rects.clone();
        } else if let Some(selection_page) = self.viewer.selection.page {
            let chars = self.viewer.chars_of(selection_page);
            self.ai.page = selection_page;
            self.ai.rects = self.viewer.selection.rects(chars);
        } else {
            self.ai.page = snapshot.current_page();
            self.ai.rects.clear();
        }

        let model = settings.lookup.model.trim().to_string();
        let key = format!(
            "{}#{}",
            self.ai_cache_key(&label),
            text.chars().count()
        );
        self.ai.request_id = self.ai.request_id.wrapping_add(1);
        let rid = self.ai.request_id;
        self.ai.term = label;

        // 上一次还在流式输出：直接丢掉任务，新的这次接管。
        self.ai.task = None;

        if let Some(text) = self.ai.cache_get(&key) {
            self.ai.phase = AiPhase::Done { text, model };
            cx.notify();
            return;
        }

        self.ai.phase = AiPhase::Thinking;
        self.ai.last_flush = Instant::now();
        self.ai.pending_chars = 0;
        // 立刻重绘：笺注栏要因为「有 AI 卡了」而出现，不能等第一个 token。
        cx.notify();

        let page = snapshot.current_page();
        let question = ai_core::build_question(&text, page);
        self.ai.task = Some(cx.spawn(async move |this, cx| {
            let (events, mut stream) = futures::channel::mpsc::unbounded();
            let runner = cx.background_executor().spawn(async move {
                ai_core::run_lookup(&settings, snapshot, question, events).await;
            });

            let timeout = cx.background_executor().timer(AI_TIMEOUT).fuse();
            futures::pin_mut!(timeout);
            let mut timed_out = false;

            loop {
                let next = stream.next();
                futures::pin_mut!(next);
                match futures::future::select(next, &mut timeout).await {
                    Either::Left((Some(event), _)) => {
                        if this
                            .update(cx, |this, cx| this.on_ai_event(rid, event, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Either::Left((None, _)) => break,
                    Either::Right((_, _)) => {
                        timed_out = true;
                        let _ = this.update(cx, |this, cx| {
                            this.on_ai_event(
                                rid,
                                LookupEvent::Failed {
                                    kind: ErrorKind::Network,
                                    message: format!("AI 响应超时（{} 秒）", AI_TIMEOUT.as_secs()),
                                },
                                cx,
                            );
                        });
                        break;
                    }
                }
            }

            if !timed_out {
                // 等 runner 收尾；它发现接收端没了会自己提前返回。
                runner.await;
            }
        }));
    }

    pub(crate) fn retry_ai_lookup(&mut self, cx: &mut Context<Self>) {
        let term = self.ai.term.clone();
        if term.is_empty() {
            return;
        }
        self.start_ai_lookup(term, cx);
    }

    pub(crate) fn close_ai_card(&mut self, cx: &mut Context<Self>) {
        self.ai.task = None;
        self.ai.phase = AiPhase::Idle;
        self.ai.term.clear();
        cx.notify();
    }

    /// 把一条流事件落到状态上；`request_id` 不匹配说明是上一轮的迟到回包。
    pub(crate) fn on_ai_event(
        &mut self,
        request: u64,
        event: LookupEvent,
        cx: &mut Context<Self>,
    ) {
        if request != self.ai.request_id {
            return;
        }
        match event {
            LookupEvent::Text(delta) => {
                let tool = match &self.ai.phase {
                    AiPhase::Streaming { tool, .. } => tool.clone(),
                    _ => None,
                };
                let mut text = match &self.ai.phase {
                    AiPhase::Streaming { text, .. } => text.clone(),
                    _ => String::new(),
                };
                text.push_str(&delta);
                self.ai.phase = AiPhase::Streaming { text, tool };
                self.ai.pending_chars += delta.chars().count();
                if self.ai.should_flush() {
                    cx.notify();
                }
            }
            LookupEvent::ToolStarted { name, args } => {
                let text = match &self.ai.phase {
                    AiPhase::Streaming { text, .. } => text.clone(),
                    _ => String::new(),
                };
                self.ai.phase = AiPhase::Streaming {
                    text,
                    tool: Some(ToolLine { name, args }),
                };
                cx.notify();
            }
            LookupEvent::Finished { text } => {
                let key = self.ai_cache_key(&self.ai.term.clone());
                let model = self
                    .settings
                    .current
                    .ai
                    .lookup
                    .model
                    .trim()
                    .to_string();
                self.ai.cache_put(key, text.clone());
                self.ai.phase = AiPhase::Done { text, model };
                cx.notify();
            }
            LookupEvent::Failed { kind, message } => {
                self.ai.phase = AiPhase::Failed { kind, message };
                cx.notify();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_is_lru_bounded_and_refreshes_on_hit() {
        let mut state = AiQueryState::default();
        for index in 0..(AI_CACHE_LIMIT + 5) {
            state.cache_put(format!("k{index}"), format!("v{index}"));
        }
        assert_eq!(state.cache.len(), AI_CACHE_LIMIT);
        assert!(state.cache_get("k0").is_none(), "最早的不该还在");

        let newest = format!("k{}", AI_CACHE_LIMIT + 4);
        assert_eq!(state.cache_get(&newest).as_deref(), Some(format!("v{}", AI_CACHE_LIMIT + 4).as_str()));
        assert_eq!(state.cache[0].0, newest, "命中后应提到最前");
    }

    #[test]
    fn tool_label_hides_empty_arguments() {
        let line = ToolLine {
            name: "current_page_text".to_string(),
            args: "{}".to_string(),
        };
        assert_eq!(line.label(), "current_page_text");

        let line = ToolLine {
            name: "read_page".to_string(),
            args: "{\"page\":12}".to_string(),
        };
        assert_eq!(line.label(), "read_page({\"page\":12})");
    }

    #[test]
    fn flush_happens_when_either_threshold_is_reached() {
        let mut state = AiQueryState::default();
        state.pending_chars = NOTIFY_CHARS;
        assert!(state.should_flush());
        assert_eq!(state.pending_chars, 0, "刷新后计数归零");

        let mut state = AiQueryState::default();
        state.pending_chars = 1;
        state.last_flush = Instant::now() - NOTIFY_INTERVAL - Duration::from_millis(1);
        assert!(state.should_flush(), "时间到也要刷");

        let mut state = AiQueryState::default();
        state.pending_chars = 1;
        assert!(!state.should_flush(), "两个阈值都没到就不刷");
    }
}
