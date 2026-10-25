//! 查词用的 agent：组装模型 + 工具 + 系统提示，把流式事件归一化成 [`LookupEvent`]。
//!
//! 这一层是 `rig` 的最后一道边界：外面只看得到 [`LookupEvent`]，升级 rig 时改动
//! 被关在本文件里。事件通过 `mpsc` 发出去而不是回调，是为了让 GPUI 侧能把
//! 「跑网络」和「改界面」拆成两个任务——回调是同步的，没法在里面 `await` 更新。

use std::sync::Arc;

use futures::channel::mpsc;
use futures::StreamExt;
use rig::agent::MultiTurnStreamItem;
use rig::prelude::*;
use rig::streaming::{Item, StreamEvent as RigStreamEvent};

use crate::client::completion_model;
use crate::settings::AiSettings;
use crate::tools::{
    CurrentPageTextTool, DocumentInfoTool, LookupContext, PageBudget, ReadPageTool,
    SurroundingTextTool, TocNearestTool,
};

/// 一次查词最多允许的模型往返轮次。
///
/// 注意这是「模型调用次数」不是「工具调用次数」：一轮里可以并行发多个工具。
/// 取 100 等于「不设轮次限制」——由模型自己决定什么时候收手，而不是被框架
/// 硬截断在半路。真正的护栏换成另外两道：页预算（最多 2 页）和界面侧的
/// 90 秒总超时，所以 100 在这里只是「别拦我」，不会真的跑 100 轮。
pub const MAX_TURNS: usize = 100;

const PREAMBLE: &str = "\
你在帮助一位读者理解 PDF 里的中文词语。读者双击了一个词，你要解释它**在这段文本里**的意思。

规则：
- 先用工具了解语境：current_page_text 看当前页，surrounding_text 定位该词前后文，\
toc_nearest 判断章节语境；当前页信息不足时才用 read_page 读别的页（次数有限）。
- 需要多个信息时**在同一轮里一起调用**（例如同时要当前页与目录），不要一个一个来，\
否则会浪费轮次。
- 人名、典故、生僻的多字词是重点：说清它指谁/出自哪部书哪一篇、在这段话里承担什么角色。\
若无法从文本确认，明确说「文本中无法确认」，不要编造出处。
- 用简体中文回答，控制在 300 字以内；可用 **粗体** 和短列表，不要客套话。
- 不要在开头重复这个词，直接给出解释。";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupEvent {
    /// 模型输出的一段文本增量。
    Text(String),
    /// 模型决定调用工具（只在调用提交后上报一次）。
    ToolStarted { name: String, args: String },
    /// 整轮结束，`text` 是最终答案。
    Finished { text: String },
    Failed { kind: ErrorKind, message: String },
}

/// 失败分类：决定界面上给什么动作（去设置 / 重试 / 只显示原文）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// 认证失败（key 无效、无权限）——重试没有意义。
    Auth,
    /// 限流或额度问题。
    RateLimited,
    /// 网络不可达、超时。
    Network,
    /// 其他（模型返回异常、工具参数不合 schema 等）。
    Other,
}

impl ErrorKind {
    pub fn retryable(self) -> bool {
        !matches!(self, Self::Auth)
    }
}

/// 从错误文本里猜分类。rig 的错误是字符串化的供应商原文，只能这样粗分；
/// 猜错的代价只是「给了个重试按钮」，比按错误类型做精密映射划算得多。
pub fn classify_error(message: &str) -> ErrorKind {
    let lower = message.to_lowercase();
    if lower.contains("401") || lower.contains("403") || lower.contains("unauthorized")
        || lower.contains("invalid api key") || lower.contains("authentication")
    {
        return ErrorKind::Auth;
    }
    if lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("quota")
        || lower.contains("overloaded")
    {
        return ErrorKind::RateLimited;
    }
    if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("network")
        || lower.contains("connection")
        || lower.contains("dns")
        || lower.contains("unreachable")
    {
        return ErrorKind::Network;
    }
    ErrorKind::Other
}

/// 选区短于等于这个字数时按「查词」处理，更长就按「解释一段话」处理。
///
/// 6 是个安全阈值：最长的成语/术语 4–5 字，多留一位余量；再长就不该指望 mdx
/// 词典能命中，也不该让模型把整句当成一个词去解释。
pub const WORD_MAX_CHARS: usize = 6;

/// 选区文本送进提示词时的上限，防止用户拖选整页。
const PASSAGE_MAX_CHARS: usize = 1200;

/// 选区是「一个词」还是「一段话」。
pub fn is_word_query(text: &str) -> bool {
    text.chars().filter(|c| !c.is_whitespace()).count() <= WORD_MAX_CHARS
}

/// 给模型的用户消息。上下文由模型自己用工具取，这里只给选区和页位置。
///
/// 词与段落的问法必须分开：对整句问「这个词是什么意思」，模型会把整句当成一个
/// 生造词去硬解释；对单个词问「这段话什么意思」，它又会开始泛泛而谈。
pub fn build_question(text: &str, page: usize) -> String {
    let trimmed = text.trim();
    if is_word_query(trimmed) {
        format!(
            "读者在第 {} 页选中了「{trimmed}」，请解释它在这段文本里的含义（人名/典故请说明出处与身份）。",
            page + 1
        )
    } else {
        let body: String = trimmed.chars().take(PASSAGE_MAX_CHARS).collect();
        let truncated = body.chars().count() < trimmed.chars().count();
        format!(
            "读者在第 {} 页选中了下面这段文字。请解释它在这段文本里的意思；涉及典故、人名、制度名或器物时说明出处与背景，最后用一句话概括这段话在说什么。\n\n{}{}",
            page + 1,
            body,
            if truncated { "\n…（选区过长，已截断）" } else { "" }
        )
    }
}

/// 跑一次查词，事件通过 `emit` 发出；接收端被丢弃（用户关掉卡片/换了词）时提前退出。
pub async fn run_lookup(
    settings: &AiSettings,
    context: Arc<dyn LookupContext>,
    question: String,
    emit: mpsc::UnboundedSender<LookupEvent>,
) {
    let model = match completion_model(settings) {
        Ok(model) => model,
        Err(error) => {
            let _ = emit.unbounded_send(LookupEvent::Failed {
                kind: ErrorKind::Other,
                message: error.to_string(),
            });
            return;
        }
    };

    let budget = Arc::new(PageBudget::new(settings.lookup.max_pages.max(1)));
    let agent = AgentBuilder::new(model)
        .preamble(PREAMBLE)
        .tool(DocumentInfoTool::new(context.clone()))
        .tool(CurrentPageTextTool::new(context.clone()))
        .tool(SurroundingTextTool::new(context.clone()))
        .tool(TocNearestTool::new(context.clone()))
        .tool(ReadPageTool::new(context, budget))
        .default_max_turns(MAX_TURNS)
        .build();

    let mut stream = agent.prompt(question).max_turns(MAX_TURNS).stream();
    let mut text = String::new();

    while let Some(item) = stream.next().await {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(RigStreamEvent::Text {
                text: delta,
                ..
            }))) => {
                if delta.is_empty() {
                    continue;
                }
                text.push_str(&delta);
                if emit.unbounded_send(LookupEvent::Text(delta)).is_err() {
                    return;
                }
            }
            Ok(MultiTurnStreamItem::ToolCall { tool_call }) => {
                let args = serde_json::to_string(&tool_call.function.arguments)
                    .unwrap_or_else(|_| String::new());
                if emit
                    .unbounded_send(LookupEvent::ToolStarted {
                        name: tool_call.function.name.to_string(),
                        args,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Ok(MultiTurnStreamItem::FinalResponse(response)) => {
                let final_text = response.output().to_string();
                if final_text.trim().is_empty() {
                    // 有供应商在最后一轮只给工具调用、不给文本，退回已累积的内容。
                    let _ = emit.unbounded_send(LookupEvent::Finished { text });
                } else {
                    let _ = emit.unbounded_send(LookupEvent::Finished {
                        text: final_text,
                    });
                }
                return;
            }
            Ok(_) => {}
            Err(error) => {
                let message = error.to_string();
                let _ = emit.unbounded_send(LookupEvent::Failed {
                    kind: classify_error(&message),
                    message,
                });
                return;
            }
        }
    }

    // 流意外结束（没有 FinalResponse）：把已拿到的文本当作结果，总比空白好。
    let _ = emit.unbounded_send(LookupEvent::Finished { text });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_classification_covers_the_common_cases() {
        assert_eq!(
            classify_error("HTTP 401 Unauthorized: invalid api key"),
            ErrorKind::Auth
        );
        assert_eq!(classify_error("429 Too Many Requests"), ErrorKind::RateLimited);
        assert_eq!(
            classify_error("error sending request: connection refused"),
            ErrorKind::Network
        );
        assert_eq!(classify_error("tool schema mismatch"), ErrorKind::Other);
        assert!(!ErrorKind::Auth.retryable());
        assert!(ErrorKind::Network.retryable());
    }

    #[test]
    fn question_mentions_the_term_and_a_one_based_page() {
        let question = build_question("子产", 4);
        assert!(question.contains("子产"));
        assert!(question.contains("第 5 页"), "页码要给用户看到的那个：{question}");
        assert!(is_word_query("子产"));
        assert!(is_word_query("东施效颦"));
        assert!(!is_word_query("郑人游于乡校，以论执政"));
    }

    #[test]
    fn passage_questions_embed_the_text_instead_of_quoting_it_as_a_word() {
        let text = "郑人游于乡校，以论执政。";
        let question = build_question(text, 0);
        assert!(question.contains(text), "{question}");
        assert!(!question.contains(&format!("「{text}」")), "段落不该被引号当成词：{question}");
        assert!(question.contains("第 1 页"));
    }

    #[test]
    fn overlong_selections_are_truncated_with_a_notice() {
        let text = "字".repeat(PASSAGE_MAX_CHARS + 50);
        let question = build_question(&text, 0);
        assert!(question.contains("已截断"), "超长选区要提示截断");
        assert!(question.chars().count() < text.chars().count() + 200);
    }
}
