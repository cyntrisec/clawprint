//! Simple HTTP viewer for recorded runs
//!
//! Serves a minimal web interface for browsing events.

use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, get_service},
    Router,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use tower_http::services::ServeDir;
use tracing::info;

use crate::storage::{list_runs, RunStorage};
use crate::{Event, RunId, RunMeta};

/// Viewer state
#[derive(Clone)]
struct ViewerState {
    base_path: PathBuf,
}

/// Start the viewer server
pub async fn start_viewer(base_path: PathBuf, port: u16) -> Result<()> {
    let state = ViewerState { base_path };

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/runs", get(list_runs_handler))
        .route("/api/runs/:run_id", get(get_run_handler))
        .route("/api/runs/:run_id/events", get(get_events_handler))
        .route("/view/:run_id", get(view_run_handler))
        .route("/export/:run_id", get(export_run_handler))
        .nest_service("/static", get_service(ServeDir::new("./static")))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    info!("Viewer starting on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Index page - list all runs
async fn index_handler(State(state): State<ViewerState>) -> impl IntoResponse {
    let runs = match list_runs(&state.base_path) {
        Ok(runs) => runs,
        Err(_) => return Html(ERROR_HTML.to_string()),
    };

    let runs_html: String = runs
        .into_iter()
        .map(|(run_id, meta)| {
            let duration = meta
                .ended_at
                .map(|end| {
                    let dur = end.signed_duration_since(meta.started_at);
                    format!("{}s", dur.num_seconds())
                })
                .unwrap_or_else(|| "in progress".to_string());

            let id_escaped = escape_html(&run_id.0);
            let id_short = if run_id.0.len() >= 8 {
                escape_html(&run_id.0[..8])
            } else {
                id_escaped.clone()
            };
            format!(
                r#"<div class="run-card">
                    <h3><a href="/view/{}">{}</a></h3>
                    <div class="meta">
                        <span>Started: {}</span>
                        <span>Duration: {}</span>
                        <span>Events: {}</span>
                    </div>
                </div>"#,
                id_escaped,
                id_short,
                meta.started_at.format("%Y-%m-%d %H:%M:%S"),
                duration,
                meta.event_count
            )
        })
        .collect();

    Html(format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Clawprint Viewer</title>
    <style>
        body {{ font-family: system-ui, sans-serif; max-width: 1200px; margin: 0 auto; padding: 20px; background: #1a1a2e; color: #eee; }}
        h1 {{ color: #00d4aa; }}
        .run-card {{ background: #16213e; padding: 15px; margin: 10px 0; border-radius: 8px; border-left: 4px solid #00d4aa; }}
        .run-card a {{ color: #00d4aa; text-decoration: none; }}
        .run-card a:hover {{ text-decoration: underline; }}
        .meta {{ display: flex; gap: 20px; margin-top: 10px; font-size: 0.9em; color: #888; }}
    </style>
</head>
<body>
    <h1>🔴 Clawprint Viewer</h1>
    <p>Recorded runs:</p>
    {}
</body>
</html>"#,
        runs_html
    ))
}

/// API: List all runs
async fn list_runs_handler(State(state): State<ViewerState>) -> impl IntoResponse {
    match list_runs(&state.base_path) {
        Ok(runs) => {
            let runs_json: Vec<_> = runs
                .into_iter()
                .map(|(id, meta)| {
                    serde_json::json!({
                        "run_id": id.0,
                        "started_at": meta.started_at,
                        "ended_at": meta.ended_at,
                        "event_count": meta.event_count,
                        "gateway_url": meta.gateway_url,
                    })
                })
                .collect();
            Json(runs_json).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// API: Get run metadata
async fn get_run_handler(
    State(state): State<ViewerState>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    let run_id = RunId(run_id);
    
    match RunStorage::open(run_id.clone(), &state.base_path) {
        Ok(storage) => {
            match storage.verify_chain() {
                Ok(valid) => {
                    let meta_path = state.base_path.join("runs").join(&run_id.0).join("meta.json");
                    let meta: Option<RunMeta> = if meta_path.exists() {
                        std::fs::read_to_string(&meta_path)
                            .ok()
                            .and_then(|s| serde_json::from_str(&s).ok())
                    } else {
                        None
                    };

                    Json(serde_json::json!({
                        "run_id": run_id.0,
                        "event_count": storage.event_count(),
                        "root_hash": storage.root_hash(),
                        "chain_valid": valid,
                        "meta": meta,
                    }))
                    .into_response()
                }
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            }
        }
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

/// API: Get run events
async fn get_events_handler(
    State(state): State<ViewerState>,
    Path(run_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let run_id = RunId(run_id);
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1000);

    match RunStorage::open(run_id.clone(), &state.base_path) {
        Ok(storage) => match storage.load_events(Some(limit)) {
            Ok(events) => Json(events).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

/// View a specific run
async fn view_run_handler(
    State(state): State<ViewerState>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    let run_id_clone = run_id.clone();
    
    let events_html = match RunStorage::open(RunId(run_id.clone()), &state.base_path) {
        Ok(storage) => match storage.load_events(Some(100)) {
            Ok(events) => events
                .into_iter()
                .map(|e| format_event_card(&e))
                .collect::<String>(),
            Err(_) => "<p>Error loading events</p>".to_string(),
        },
        Err(_) => "<p>Run not found</p>".to_string(),
    };

    Html(format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Run {} - Clawprint</title>
    <style>
        body {{ font-family: system-ui, sans-serif; max-width: 1400px; margin: 0 auto; padding: 20px; background: #1a1a2e; color: #eee; }}
        h1 {{ color: #00d4aa; }}
        .back {{ color: #888; text-decoration: none; }}
        .event {{ background: #16213e; padding: 12px; margin: 8px 0; border-radius: 6px; border-left: 3px solid #0f3460; }}
        .event.RUN_START {{ border-left-color: #00d4aa; }}
        .event.RUN_END {{ border-left-color: #e94560; }}
        .event.TOOL_CALL {{ border-left-color: #f39c12; }}
        .event.TOOL_RESULT {{ border-left-color: #3498db; }}
        .meta {{ font-size: 0.85em; color: #888; margin-bottom: 5px; }}
        .kind {{ font-weight: bold; color: #00d4aa; }}
        pre {{ background: #0f3460; padding: 10px; border-radius: 4px; overflow-x: auto; font-size: 0.85em; }}
        .hash {{ font-family: monospace; font-size: 0.8em; color: #666; }}
    </style>
</head>
<body>
    <a href="/" class="back">← Back to runs</a>
    <h1>🔴 Run {}</h1>
    <p><a href="/export/{}">Export HTML Report</a></p>
    <div class="events">
        {}
    </div>
</body>
</html>"#,
        run_id_clone, run_id_clone, run_id, events_html
    ))
}

/// Export run as static HTML report
async fn export_run_handler(
    State(state): State<ViewerState>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    let run_id = RunId(run_id);
    
    match RunStorage::open(run_id.clone(), &state.base_path) {
        Ok(storage) => match storage.load_events(None) {
            Ok(events) => {
                let events_html = events
                    .into_iter()
                    .map(|e| format_event_card(&e))
                    .collect::<String>();

                let report = format!(
                    r#"<!DOCTYPE html>
<html>
<head>
    <title>Clawprint Report - {}</title>
    <style>
        body {{ font-family: system-ui, sans-serif; max-width: 1400px; margin: 0 auto; padding: 20px; background: #fff; color: #333; }}
        h1 {{ color: #1a1a2e; border-bottom: 2px solid #00d4aa; padding-bottom: 10px; }}
        .event {{ background: #f8f9fa; padding: 12px; margin: 8px 0; border-radius: 6px; border-left: 3px solid #ccc; }}
        .meta {{ font-size: 0.85em; color: #666; margin-bottom: 5px; }}
        .kind {{ font-weight: bold; color: #00d4aa; }}
        pre {{ background: #f4f4f4; padding: 10px; border-radius: 4px; overflow-x: auto; font-size: 0.85em; }}
        .hash {{ font-family: monospace; font-size: 0.8em; color: #999; }}
        .header {{ margin-bottom: 20px; padding: 15px; background: #1a1a2e; color: #fff; border-radius: 8px; }}
    </style>
</head>
<body>
    <div class="header">
        <h1>🔴 Clawprint Report</h1>
        <p>Run ID: {}</p>
        <p>Generated: {}</p>
    </div>
    <div class="events">
        {}
    </div>
</body>
</html>"#,
                    run_id.0, run_id.0, chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"), events_html
                );

                ([("content-type", "text/html")], report).into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn format_event_card(event: &Event) -> String {
    let kind_class = format!("{:?}", event.kind);
    let payload_preview = serde_json::to_string_pretty(&event.payload)
        .unwrap_or_default()
        .lines()
        .take(20)
        .collect::<Vec<_>>()
        .join("\n");

    let hash_prefix = if event.hash_self.len() >= 16 {
        &event.hash_self[..16]
    } else {
        &event.hash_self
    };

    format!(
        r#"<div class="event {}">
            <div class="meta">
                <span class="kind">{}</span> |
                <span>{}</span> |
                <span class="hash">{}</span>
            </div>
            <pre>{}</pre>
        </div>"#,
        escape_html(&kind_class),
        escape_html(&kind_class),
        event.ts.format("%H:%M:%S%.3f"),
        escape_html(hash_prefix),
        escape_html(&payload_preview)
    )
}

const ERROR_HTML: &str = r#"<!DOCTYPE html>
<html><body><h1>Error loading runs</h1></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_html() {
        assert_eq!(escape_html("<script>alert(1)</script>"),
                   "&lt;script&gt;alert(1)&lt;/script&gt;");
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html(r#"x="y""#), "x=&quot;y&quot;");
        assert_eq!(escape_html("it's"), "it&#x27;s");
        assert_eq!(escape_html("safe text"), "safe text");
    }

    #[test]
    fn test_format_event_card_escapes_payload() {
        let event = crate::Event::new(
            crate::RunId("test-run".to_string()),
            crate::EventId(1),
            crate::EventKind::ToolCall,
            serde_json::json!({"xss": "<img src=x onerror=alert(1)>"}),
            None,
        );
        let html = format_event_card(&event);
        assert!(!html.contains("<img"), "XSS payload must be escaped");
        assert!(html.contains("&lt;img"), "XSS payload must be escaped to entities");
    }
}
