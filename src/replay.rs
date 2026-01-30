//! Replay functionality - deterministic reconstruction from ledger
//!
//! Replays a recorded run timeline without contacting gateway.

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use tracing::{info, warn};

use crate::storage::RunStorage;
use crate::{Event, EventKind, RunId};

/// Replay a recorded run
pub fn replay_run(run_id: &RunId, base_path: &Path, offline: bool) -> Result<ReplayResult> {
    info!("Replaying run: {} (offline={})", run_id.0, offline);

    let storage = RunStorage::open(run_id.clone(), base_path)?;
    let events = storage.load_events(None)?;

    if events.is_empty() {
        return Err(anyhow!("No events in run"));
    }

    // Verify chain integrity first
    if !storage.verify_chain()? {
        warn!("Hash chain verification failed - replay may be tampered");
    }

    let mut result = ReplayResult {
        run_id: run_id.clone(),
        event_count: events.len() as u64,
        tool_calls: Vec::new(),
        outputs: Vec::new(),
        final_output: String::new(),
    };

    // Replay events in order
    for event in &events {
        match event.kind {
            EventKind::ToolCall => {
                if let Some(tool) = event.payload.get("tool").and_then(|v| v.as_str()) {
                    result.tool_calls.push(ToolCallReplay {
                        tool: tool.to_string(),
                        args: event.payload.get("args").cloned().unwrap_or_default(),
                        span_id: event.span_id.clone(),
                    });
                }
            }
            EventKind::ToolResult => {
                // Match with previous tool call by span_id if available
            }
            EventKind::OutputChunk => {
                if let Some(content) = event.payload.get("content").and_then(|v| v.as_str()) {
                    result.outputs.push(content.to_string());
                }
            }
            EventKind::RunEnd => {
                // Finalize
            }
            _ => {}
        }
    }

    // Reconstruct final output from chunks
    result.final_output = result.outputs.join("");

    info!(
        "Replay complete: {} events, {} tool calls",
        result.event_count,
        result.tool_calls.len()
    );

    Ok(result)
}

/// Replay result containing reconstructed session
#[derive(Debug)]
pub struct ReplayResult {
    pub run_id: RunId,
    pub event_count: u64,
    pub tool_calls: Vec<ToolCallReplay>,
    pub outputs: Vec<String>,
    pub final_output: String,
}

/// Tool call replay entry
#[derive(Debug)]
pub struct ToolCallReplay {
    pub tool: String,
    pub args: serde_json::Value,
    pub span_id: Option<String>,
}

/// Generate a text transcript from replay
pub fn generate_transcript(result: &ReplayResult) -> String {
    let mut transcript = format!("# Clawprint Replay - {}\n\n", result.run_id.0);
    transcript.push_str(&format!("Events: {}\n", result.event_count));
    transcript.push_str(&format!("Tool Calls: {}\n\n", result.tool_calls.len()));

    for (i, call) in result.tool_calls.iter().enumerate() {
        transcript.push_str(&format!("## Tool Call {}: {}\n", i + 1, call.tool));
        transcript.push_str(&format!(
            "```json\n{}\n```\n\n",
            serde_json::to_string_pretty(&call.args).unwrap_or_default()
        ));
    }

    if !result.final_output.is_empty() {
        transcript.push_str("## Final Output\n\n");
        transcript.push_str(&result.final_output);
        transcript.push('\n');
    }

    transcript
}

/// Compare two runs and show differences
pub fn diff_runs(run_a: &RunId, run_b: &RunId, base_path: &Path) -> Result<String> {
    let storage_a = RunStorage::open(run_a.clone(), base_path)?;
    let storage_b = RunStorage::open(run_b.clone(), base_path)?;

    let events_a = storage_a.load_events(None)?;
    let events_b = storage_b.load_events(None)?;

    let mut diff = format!("# Diff: {} vs {}\n\n", run_a.0, run_b.0);
    diff.push_str(&format!("Events A: {} | Events B: {}\n\n", events_a.len(), events_b.len()));

    // Simple diff: compare event kinds and tool calls
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
        let storage = crate::storage::RunStorage::new(
            run_id.clone(),
            temp_dir.path(),
            10
        ).unwrap();
        
        // Should fail with no events
        let result = replay_run(&run_id, temp_dir.path(), true);
        assert!(result.is_err());
    }
}
