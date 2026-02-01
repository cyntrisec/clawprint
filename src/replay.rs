//! Replay functionality - deterministic reconstruction from ledger
//!
//! Replays a recorded run timeline without contacting gateway.
//! Produces rich transcripts with event breakdowns, agent run sections,
//! timestamps, and chat reconstruction from OUTPUT_CHUNK deltas.

use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};

use crate::storage::{RunStorage, verify_event_chain};
use crate::{Event, EventKind, RunId};

/// Info about a single agent conversation run within the recording
#[derive(Debug)]
pub struct AgentRunInfo {
    pub run_id: String,
    pub start_time: Option<chrono::DateTime<chrono::Utc>>,
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
    pub event_count: u64,
    pub tool_calls: Vec<ToolCallReplay>,
    pub chat_output: String,
}

/// Replay a recorded run
pub fn replay_run(run_id: &RunId, base_path: &Path, offline: bool) -> Result<ReplayResult> {
    info!("Replaying run: {} (offline={})", run_id.0, offline);

    let storage = RunStorage::open(run_id.clone(), base_path)?;
    let events = storage.load_events(None)?;

    if events.is_empty() {
        return Err(anyhow!("No events in run"));
    }

    // Verify chain integrity from already-loaded events (avoids double load)
    if !verify_event_chain(&events)? {
        warn!("Hash chain verification failed - replay may be tampered");
    }

    // Build event breakdown
    let mut event_breakdown: HashMap<String, u64> = HashMap::new();
    for event in &events {
        *event_breakdown
            .entry(format!("{:?}", event.kind))
            .or_insert(0) += 1;
    }

    // Identify agent runs and group events
    let mut agent_runs: HashMap<String, AgentRunInfo> = HashMap::new();
    let mut agent_run_order: Vec<String> = Vec::new();

    let mut result = ReplayResult {
        run_id: run_id.clone(),
        event_count: events.len() as u64,
        started_at: events.first().map(|e| e.ts),
        ended_at: events.last().map(|e| e.ts),
        event_breakdown,
        tool_calls: Vec::new(),
        outputs: Vec::new(),
        final_output: String::new(),
        agent_runs: Vec::new(),
    };

    for event in &events {
        // Extract agent runId from gateway event payload
        let agent_run_id = extract_agent_run_id(event);

        match event.kind {
            EventKind::AgentEvent => {
                if let Some(ref arid) = agent_run_id {
                    let info = agent_runs.entry(arid.clone()).or_insert_with(|| {
                        agent_run_order.push(arid.clone());
                        AgentRunInfo {
                            run_id: arid.clone(),
                            start_time: Some(event.ts),
                            end_time: None,
                            event_count: 0,
                            tool_calls: Vec::new(),
                            chat_output: String::new(),
                        }
                    });
                    info.end_time = Some(event.ts);
                    info.event_count += 1;

                    // Extract tool calls from agent events (only tool_use, not tool_result)
                    let is_tool_use = event
                        .payload
                        .pointer("/data/type")
                        .and_then(|v| v.as_str())
                        .map(|t| t == "tool_use")
                        .unwrap_or(false);
                    if is_tool_use
                        && let Some(tool) =
                            event.payload.pointer("/data/tool").and_then(|v| v.as_str())
                    {
                        let tc = ToolCallReplay {
                            tool: tool.to_string(),
                            args: event
                                .payload
                                .pointer("/data/args")
                                .cloned()
                                .unwrap_or_default(),
                            span_id: event.span_id.clone(),
                            timestamp: Some(event.ts),
                        };
                        info.tool_calls.push(tc.clone());
                        result.tool_calls.push(tc);
                    }
                }
            }
            EventKind::ToolCall => {
                let tool = event
                    .payload
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let tc = ToolCallReplay {
                    tool: tool.to_string(),
                    args: event.payload.get("args").cloned().unwrap_or_default(),
                    span_id: event.span_id.clone(),
                    timestamp: Some(event.ts),
                };
                result.tool_calls.push(tc);
            }
            EventKind::OutputChunk => {
                // Try to reconstruct chat from gateway "chat" events
                // The "state: final" chunks contain full text
                let content = extract_chat_content(event);
                if let Some(text) = content {
                    result.outputs.push(text.clone());
                    // Also attach to agent run if we can identify it
                    if let Some(ref arid) = agent_run_id
                        && let Some(info) = agent_runs.get_mut(arid)
                    {
                        info.chat_output = text;
                    }
                }
            }
            EventKind::ToolResult => {}
            EventKind::RunEnd => {}
            _ => {}
        }
    }

    // Reconstruct final output — prefer the last "final" state chunk
    if !result.outputs.is_empty() {
        result.final_output = result.outputs.last().cloned().unwrap_or_default();
    }

    // Collect agent runs in order
    for arid in &agent_run_order {
        if let Some(info) = agent_runs.remove(arid) {
            result.agent_runs.push(info);
        }
    }

    info!(
        "Replay complete: {} events, {} tool calls, {} agent runs",
        result.event_count,
        result.tool_calls.len(),
        result.agent_runs.len(),
    );

    Ok(result)
}

/// Extract the agent runId from a gateway event payload
fn extract_agent_run_id(event: &Event) -> Option<String> {
    event
        .payload
        .pointer("/data/runId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract chat content from an OUTPUT_CHUNK event.
/// Prefers "state: final" chunks which contain the complete message text.
fn extract_chat_content(event: &Event) -> Option<String> {
    // Try gateway-wrapped format: data.content or data.text
    if let Some(data) = event.payload.get("data") {
        // Check if this is a "final" state chunk (contains full text)
        let is_final = data
            .get("state")
            .and_then(|v| v.as_str())
            .map(|s| s == "final")
            .unwrap_or(false);

        if is_final {
            // Final chunks often have the complete text in "text" or "content"
            if let Some(text) = data.get("text").and_then(|v| v.as_str()) {
                return Some(text.to_string());
            }
            if let Some(text) = data.get("content").and_then(|v| v.as_str()) {
                return Some(text.to_string());
            }
        }

        // Delta chunks — accumulate content
        if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
            return Some(delta.to_string());
        }
    }

    // Direct format
    event
        .payload
        .get("content")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Replay result containing reconstructed session
#[derive(Debug)]
pub struct ReplayResult {
    pub run_id: RunId,
    pub event_count: u64,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub event_breakdown: HashMap<String, u64>,
    pub tool_calls: Vec<ToolCallReplay>,
    pub outputs: Vec<String>,
    pub final_output: String,
    pub agent_runs: Vec<AgentRunInfo>,
}

/// Tool call replay entry
#[derive(Debug, Clone)]
pub struct ToolCallReplay {
    pub tool: String,
    pub args: serde_json::Value,
    pub span_id: Option<String>,
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// Generate a rich text transcript from replay
pub fn generate_transcript(result: &ReplayResult) -> String {
    let mut t = String::new();

    // Header
    t.push_str(&format!("# Clawprint Replay — {}\n\n", result.run_id.0));

    // Summary
    if let (Some(start), Some(end)) = (result.started_at, result.ended_at) {
        let dur = end.signed_duration_since(start);
        let mins = dur.num_minutes();
        let secs = dur.num_seconds() % 60;
        t.push_str(&format!("**Duration:** {}m {}s  \n", mins, secs));
        t.push_str(&format!(
            "**Period:** {} → {}  \n",
            start.format("%Y-%m-%d %H:%M:%S UTC"),
            end.format("%H:%M:%S UTC"),
        ));
    }

    t.push_str(&format!("**Events:** {}  \n", result.event_count));
    t.push_str(&format!("**Tool Calls:** {}  \n", result.tool_calls.len()));
    t.push_str(&format!(
        "**Agent Runs:** {}  \n\n",
        result.agent_runs.len()
    ));

    // Event breakdown
    t.push_str("## Event Breakdown\n\n");
    t.push_str("| Kind | Count | % |\n");
    t.push_str("|------|------:|--:|\n");

    let total = result.event_count as f64;
    let mut sorted: Vec<_> = result.event_breakdown.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));

    for (kind, count) in &sorted {
        let pct = (**count as f64 / total) * 100.0;
        t.push_str(&format!("| {} | {} | {:.1}% |\n", kind, count, pct));
    }
    t.push('\n');

    // Agent runs
    if !result.agent_runs.is_empty() {
        t.push_str("## Agent Runs\n\n");

        for (i, run) in result.agent_runs.iter().enumerate() {
            let id_short = if run.run_id.len() > 8 {
                &run.run_id[..8]
            } else {
                &run.run_id
            };

            t.push_str(&format!("### Run {} — `{}`\n\n", i + 1, id_short));

            if let (Some(start), Some(end)) = (run.start_time, run.end_time) {
                let dur = end.signed_duration_since(start);
                t.push_str(&format!(
                    "- **Time:** {} → {} ({}s)  \n",
                    start.format("%H:%M:%S"),
                    end.format("%H:%M:%S"),
                    dur.num_seconds(),
                ));
            }

            t.push_str(&format!("- **Events:** {}  \n", run.event_count));
            t.push_str(&format!("- **Tool Calls:** {}  \n", run.tool_calls.len()));

            // Tool calls within this agent run
            if !run.tool_calls.is_empty() {
                t.push('\n');
                for tc in &run.tool_calls {
                    let ts_str = tc
                        .timestamp
                        .map(|ts| ts.format("%H:%M:%S").to_string())
                        .unwrap_or_default();
                    t.push_str(&format!("  - `[{}]` **{}**", ts_str, tc.tool));
                    if let Some(ref span) = tc.span_id {
                        t.push_str(&format!(" ({})", span));
                    }
                    t.push('\n');
                }
            }

            // Chat output
            if !run.chat_output.is_empty() {
                t.push_str("\n**Assistant Output:**\n\n");
                // Truncate very long outputs
                if run.chat_output.len() > 2000 {
                    t.push_str(&run.chat_output[..2000]);
                    t.push_str("\n\n_(truncated)_\n");
                } else {
                    t.push_str(&run.chat_output);
                    t.push('\n');
                }
            }

            t.push('\n');
        }
    }

    // All tool calls (flat list)
    if !result.tool_calls.is_empty() {
        t.push_str("## All Tool Calls\n\n");

        for (i, call) in result.tool_calls.iter().enumerate() {
            let ts_str = call
                .timestamp
                .map(|ts| ts.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "??:??:??".to_string());

            t.push_str(&format!("### {} `[{}]` {}\n", i + 1, ts_str, call.tool));
            t.push_str(&format!(
                "```json\n{}\n```\n\n",
                serde_json::to_string_pretty(&call.args).unwrap_or_default()
            ));
        }
    }

    // Final output
    if !result.final_output.is_empty() {
        t.push_str("## Final Output\n\n");
        if result.final_output.len() > 5000 {
            t.push_str(&result.final_output[..5000]);
            t.push_str("\n\n_(truncated)_\n");
        } else {
            t.push_str(&result.final_output);
            t.push('\n');
        }
    }

    t
}

/// Compare two runs and show differences
pub fn diff_runs(run_a: &RunId, run_b: &RunId, base_path: &Path) -> Result<String> {
    let storage_a = RunStorage::open(run_a.clone(), base_path)?;
    let storage_b = RunStorage::open(run_b.clone(), base_path)?;

    let events_a = storage_a.load_events(None)?;
    let events_b = storage_b.load_events(None)?;

    let mut diff = format!("# Diff: {} vs {}\n\n", run_a.0, run_b.0);
    diff.push_str(&format!(
        "Events A: {} | Events B: {}\n\n",
        events_a.len(),
        events_b.len()
    ));

    // Event kind breakdown comparison
    let mut kinds_a: HashMap<String, u64> = HashMap::new();
    let mut kinds_b: HashMap<String, u64> = HashMap::new();
    for e in &events_a {
        *kinds_a.entry(format!("{:?}", e.kind)).or_insert(0) += 1;
    }
    for e in &events_b {
        *kinds_b.entry(format!("{:?}", e.kind)).or_insert(0) += 1;
    }

    diff.push_str("## Event Kind Comparison\n\n");
    diff.push_str("| Kind | Run A | Run B | Delta |\n");
    diff.push_str("|------|------:|------:|------:|\n");

    let mut all_kinds: Vec<String> = kinds_a.keys().chain(kinds_b.keys()).cloned().collect();
    all_kinds.sort();
    all_kinds.dedup();

    for kind in &all_kinds {
        let a = kinds_a.get(kind).copied().unwrap_or(0);
        let b = kinds_b.get(kind).copied().unwrap_or(0);
        let delta = b as i64 - a as i64;
        let delta_str = if delta > 0 {
            format!("+{}", delta)
        } else {
            format!("{}", delta)
        };
        diff.push_str(&format!("| {} | {} | {} | {} |\n", kind, a, b, delta_str));
    }
    diff.push('\n');

    // Structural comparison
    let min_len = events_a.len().min(events_b.len());
    let mut differences = 0;

    for i in 0..min_len {
        let a = &events_a[i];
        let b = &events_b[i];

        if a.kind != b.kind {
            differences += 1;
            diff.push_str(&format!(
                "Event {}: KIND DIFFERENT - {:?} vs {:?}\n",
                i, a.kind, b.kind
            ));
        }
    }

    if events_a.len() != events_b.len() {
        differences += 1;
        diff.push_str(&format!(
            "LENGTH DIFFERENT: {} vs {}\n",
            events_a.len(),
            events_b.len()
        ));
    }

    if differences == 0 {
        diff.push_str("Runs are identical in structure.\n");
    } else {
        diff.push_str(&format!("\nTotal differences: {}\n", differences));
    }

    Ok(diff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_replay_empty() {
        let temp_dir = TempDir::new().unwrap();
        let run_id = RunId::new();

        // Create empty run
        let _storage =
            crate::storage::RunStorage::new(run_id.clone(), temp_dir.path(), 10).unwrap();

        // Should fail with no events
        let result = replay_run(&run_id, temp_dir.path(), true);
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_transcript_basic() {
        let result = ReplayResult {
            run_id: RunId("test-run-123".to_string()),
            event_count: 42,
            started_at: None,
            ended_at: None,
            event_breakdown: {
                let mut m = HashMap::new();
                m.insert("AgentEvent".to_string(), 30);
                m.insert("OutputChunk".to_string(), 10);
                m.insert("RunStart".to_string(), 1);
                m.insert("RunEnd".to_string(), 1);
                m
            },
            tool_calls: vec![ToolCallReplay {
                tool: "read_file".to_string(),
                args: serde_json::json!({"path": "/tmp/test"}),
                span_id: Some("seq:5".to_string()),
                timestamp: None,
            }],
            outputs: vec!["Hello world".to_string()],
            final_output: "Hello world".to_string(),
            agent_runs: vec![],
        };

        let transcript = generate_transcript(&result);
        assert!(transcript.contains("test-run-123"));
        assert!(transcript.contains("Events:** 42"));
        assert!(transcript.contains("AgentEvent"));
        assert!(transcript.contains("read_file"));
        assert!(transcript.contains("Hello world"));
    }

    #[test]
    fn test_extract_chat_content_final() {
        let event = Event::new(
            RunId("test".to_string()),
            crate::EventId(1),
            EventKind::OutputChunk,
            serde_json::json!({
                "data": {
                    "state": "final",
                    "text": "Complete response text"
                }
            }),
            None,
        );

        let content = extract_chat_content(&event);
        assert_eq!(content, Some("Complete response text".to_string()));
    }

    #[test]
    fn test_extract_chat_content_delta() {
        let event = Event::new(
            RunId("test".to_string()),
            crate::EventId(1),
            EventKind::OutputChunk,
            serde_json::json!({
                "data": {
                    "delta": "partial chunk"
                }
            }),
            None,
        );

        let content = extract_chat_content(&event);
        assert_eq!(content, Some("partial chunk".to_string()));
    }
}
