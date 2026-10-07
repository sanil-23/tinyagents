//! Turn and message lifecycle events (`TurnStarted`, `TurnCompleted`,
//! `MessageAppended`) and the message-carrying `QueuedMessageApplied`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::context::{RunConfig, RunContext};
use crate::events::AgentEvent;
use crate::ids::CallId;
use crate::run_queue::{QueueLane, RunQueue};
use crate::runtime::{AgentHarness, PayloadCapture, RunPolicy};
use crate::testkit::{EventRecorder, ScriptedModel};
use tinyinference_llm::message::{AssistantMessage, ContentBlock, Message};
use tinyinference_llm::model::{ChatModel, ModelRequest, ModelResponse};
use tinyinference_llm::tool::ToolCall;
use tinyinference_llm::usage::Usage;
use tinytools::{Tool, ToolResult};

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echo"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::success("echoed"))
    }
}

struct FailingModel;

#[async_trait]
impl ChatModel<()> for FailingModel {
    async fn invoke(&self, _: &(), _: ModelRequest) -> tinyinference_llm::Result<ModelResponse> {
        Err(tinyinference_llm::Error::Model("boom".into()))
    }
}

fn response(tool_calls: Vec<ToolCall>, text: &str) -> ModelResponse {
    ModelResponse {
        message: AssistantMessage {
            id: None,
            content: if text.is_empty() {
                Vec::new()
            } else {
                vec![ContentBlock::Text(text.to_string())]
            },
            tool_calls,
            usage: Some(Usage::new(1, 1)),
            origin: None,
        },
        usage: Some(Usage::new(1, 1)),
        finish_reason: Some("stop".to_string()),
        raw: None,
        resolved_model: None,
        continue_turn: None,
        served_from_cache: false,
        correlation: None,
        resolved_route: None,
    }
}

fn tool_turn(ids: &[&str]) -> ModelResponse {
    response(
        ids.iter()
            .map(|id| ToolCall::new(*id, "echo", json!({})))
            .collect(),
        "",
    )
}

fn harness(responses: Vec<ModelResponse>, capture: PayloadCapture) -> AgentHarness<()> {
    let mut harness: AgentHarness<()> = AgentHarness::new();
    harness.register_model("mock", Arc::new(ScriptedModel::new(responses)));
    harness.register_tool(Arc::new(EchoTool));
    harness.with_policy(RunPolicy {
        capture,
        ..RunPolicy::default()
    });
    harness
}

/// A compact rendering of the lifecycle events, in emission order.
fn lifecycle(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TurnStarted { turn } => Some(format!("turn.started:{turn}")),
            AgentEvent::TurnCompleted {
                turn,
                tool_result_count,
                tool_call_ids,
            } => Some(format!(
                "turn.completed:{turn}:{tool_result_count}:{}",
                tool_call_ids
                    .iter()
                    .map(CallId::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            )),
            AgentEvent::MessageAppended {
                role,
                index,
                call_id,
                ..
            } => Some(format!(
                "message:{index}:{role}{}",
                call_id
                    .as_ref()
                    .map(|id| format!(":{}", id.as_str()))
                    .unwrap_or_default()
            )),
            AgentEvent::ModelStarted { .. } => Some("model.started".to_string()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn turns_and_messages_are_announced_in_transcript_order() {
    let harness = harness(
        vec![tool_turn(&["a", "b"]), response(vec![], "done")],
        PayloadCapture::default(),
    );
    let recorder = EventRecorder::new();
    let ctx = RunContext::new(RunConfig::new("life"), ()).with_events(recorder.sink());

    let run = harness
        .invoke_in_context(&(), ctx, vec![Message::user("go")])
        .await
        .unwrap();

    assert_eq!(run.messages.len(), 5);
    assert_eq!(
        lifecycle(&recorder.events()),
        vec![
            "turn.started:1",
            "model.started",
            "message:1:assistant",
            "message:2:tool:a",
            "message:3:tool:b",
            "turn.completed:1:2:a,b",
            "turn.started:2",
            "model.started",
            "message:4:assistant",
            "turn.completed:2:0:",
        ]
    );
}

#[tokio::test]
async fn message_payloads_follow_the_capture_policy() {
    for (capture, expect_message, expect_tool) in [
        (PayloadCapture::default(), false, false),
        (PayloadCapture::all(), true, true),
    ] {
        let harness = harness(vec![tool_turn(&["a"]), response(vec![], "done")], capture);
        let recorder = EventRecorder::new();
        let ctx = RunContext::new(RunConfig::new("cap"), ()).with_events(recorder.sink());
        harness
            .invoke_in_context(&(), ctx, vec![Message::user("go")])
            .await
            .unwrap();
        for event in recorder.events() {
            if let AgentEvent::MessageAppended { role, message, .. } = event {
                let expected = if role == "tool" { expect_tool } else { expect_message };
                assert_eq!(message.is_some(), expected, "role {role}");
            }
        }
    }
}

#[tokio::test]
async fn a_failed_turn_is_still_closed() {
    let mut harness: AgentHarness<()> = AgentHarness::new();
    harness.register_model("mock", Arc::new(FailingModel));
    let recorder = EventRecorder::new();
    let ctx = RunContext::new(RunConfig::new("fail"), ()).with_events(recorder.sink());
    let partial = harness
        .invoke_in_context_collecting_partial(&(), ctx, vec![Message::user("go")])
        .await;
    assert!(partial.error.is_some());
    assert_eq!(
        lifecycle(&recorder.events()),
        vec!["turn.started:1", "model.started", "turn.completed:1:0:"]
    );
}

#[tokio::test]
async fn queued_message_applied_carries_the_applied_messages() {
    let harness = harness(
        vec![tool_turn(&["a"]), response(vec![], "done")],
        PayloadCapture::all(),
    );
    let queue = Arc::new(RunQueue::new());
    queue.push(QueueLane::Steer, Message::user("be brief")).await;
    let recorder = EventRecorder::new();
    let ctx = RunContext::new(RunConfig::new("queue"), ())
        .with_events(recorder.sink())
        .with_run_queue(Arc::clone(&queue));
    harness
        .invoke_in_context(&(), ctx, vec![Message::user("go")])
        .await
        .unwrap();

    let events = recorder.events();
    let applied = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::QueuedMessageApplied {
                count,
                first_index,
                messages,
                ..
            } => Some((*count, *first_index, messages.clone())),
            _ => None,
        })
        .expect("queue applied");
    assert_eq!(applied.0, 1);
    assert_eq!(applied.1, 3, "after user, assistant, tool");
    assert_eq!(applied.2.len(), 1);
    assert_eq!(applied.2[0], serde_json::to_value(Message::user("be brief")).unwrap());
    // The same message is also announced as an ordinary transcript append.
    assert!(lifecycle(&events).contains(&"message:3:user".to_string()));
}
