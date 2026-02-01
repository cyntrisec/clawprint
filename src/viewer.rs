//! Web viewer — cybersecurity dashboard for Clawprint recordings
//!
//! Serves an interactive dark-themed web interface for browsing events.

use anyhow::Result;
use axum::{
    Router,
    extract::{Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{Html, IntoResponse, Json},
    routing::get,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use crate::RunId;
use crate::storage::{RunStorage, list_runs_with_stats};

#[derive(Clone)]
struct ViewerState {
    base_path: PathBuf,
}

pub async fn bearer_auth(
    State(expected): State<Arc<String>>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());
    match auth_header {
        Some(val) if val == format!("Bearer {}", *expected) => next.run(req).await.into_response(),
        _ => (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    }
}

pub async fn start_viewer(
    base_path: PathBuf,
    host: [u8; 4],
    port: u16,
    token: Option<String>,
) -> Result<()> {
    let state = ViewerState { base_path };

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/view/{run_id}", get(view_run_handler))
        .route("/api/runs", get(list_runs_handler))
        .route("/api/runs/{run_id}", get(get_run_handler))
        .route("/api/runs/{run_id}/events", get(get_events_handler))
        .route("/api/runs/{run_id}/stats", get(get_run_stats_handler))
        .with_state(state);

    let app = if let Some(tok) = token {
        app.layer(middleware::from_fn_with_state(Arc::new(tok), bearer_auth))
    } else {
        app
    };

    let addr = SocketAddr::from((host, port));
    info!("Viewer starting on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn format_duration_html(
    start: chrono::DateTime<chrono::Utc>,
    end: Option<chrono::DateTime<chrono::Utc>>,
) -> String {
    match end {
        Some(e) => {
            let secs = e.signed_duration_since(start).num_seconds();
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            if h > 0 {
                format!("{}h {}m {}s", h, m, s)
            } else if m > 0 {
                format!("{}m {}s", m, s)
            } else {
                format!("{}s", s)
            }
        }
        None => "recording...".to_string(),
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

// ---------------------------------------------------------------------------
// Page handlers
// ---------------------------------------------------------------------------

async fn index_handler(State(state): State<ViewerState>) -> impl IntoResponse {
    let runs = match list_runs_with_stats(&state.base_path) {
        Ok(r) => r,
        Err(_) => return Html("<h1>Error loading runs</h1>".to_string()),
    };

    let total_runs = runs.len();
    let total_events: u64 = runs.iter().map(|(_, m, _)| m.event_count).sum();
    let total_size: u64 = runs.iter().map(|(_, _, s)| s).sum();

    let runs_html: String = runs
        .iter()
        .map(|(run_id, meta, size)| {
            let id_esc = escape_html(&run_id.0);
            let id_short = if run_id.0.len() >= 8 {
                escape_html(&run_id.0[..8])
            } else {
                id_esc.clone()
            };
            let dur = format_duration_html(meta.started_at, meta.ended_at);
            let status = if meta.ended_at.is_some() {
                r#"<span class="badge complete">Complete</span>"#
            } else {
                r#"<span class="badge progress">Recording</span>"#
            };
            format!(
                r#"<a href="/view/{id}" class="run-card">
                <div class="run-card-header">
                    <code class="run-id">{short}</code>
                    {status}
                    <span class="lock" title="Hash-chain verified">&#x1f512;</span>
                </div>
                <div class="run-meta">
                    <span><b>Started</b> {started}</span>
                    <span><b>Duration</b> {dur}</span>
                    <span><b>Events</b> {events}</span>
                    <span><b>Size</b> {size}</span>
                </div>
            </a>"#,
                id = id_esc,
                short = id_short,
                status = status,
                started = meta.started_at.format("%Y-%m-%d %H:%M:%S"),
                dur = dur,
                events = meta.event_count,
                size = format_bytes(*size),
            )
        })
        .collect();

    Html(
        DASHBOARD_HTML
            .replace("{{TOTAL_RUNS}}", &total_runs.to_string())
            .replace("{{TOTAL_EVENTS}}", &total_events.to_string())
            .replace("{{TOTAL_SIZE}}", &format_bytes(total_size))
            .replace("{{RUNS}}", &runs_html),
    )
}

async fn view_run_handler(Path(run_id): Path<String>) -> impl IntoResponse {
    Html(RUN_DETAIL_HTML.replace("{{RUN_ID}}", &escape_html(&run_id)))
}

// ---------------------------------------------------------------------------
// API handlers
// ---------------------------------------------------------------------------

async fn list_runs_handler(State(state): State<ViewerState>) -> impl IntoResponse {
    match list_runs_with_stats(&state.base_path) {
        Ok(runs) => {
            let j: Vec<_> = runs
                .into_iter()
                .map(|(id, meta, size)| {
                    serde_json::json!({
                        "run_id": id.0,
                        "started_at": meta.started_at,
                        "ended_at": meta.ended_at,
                        "event_count": meta.event_count,
                        "size": size,
                    })
                })
                .collect();
            Json(j).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_run_handler(
    State(state): State<ViewerState>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    let run_id = RunId(run_id);
    match RunStorage::open(run_id.clone(), &state.base_path) {
        Ok(storage) => {
            let valid = storage.verify_chain().unwrap_or(false);
            Json(serde_json::json!({
                "run_id": run_id.0,
                "event_count": storage.event_count(),
                "root_hash": storage.root_hash(),
                "chain_valid": valid,
            }))
            .into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

async fn get_events_handler(
    State(state): State<ViewerState>,
    Path(run_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let run_id = RunId(run_id);

    let page: usize = params.get("page").and_then(|s| s.parse().ok()).unwrap_or(1);
    let per_page: usize = params
        .get("per_page")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let offset = (page.saturating_sub(1)) * per_page;

    let kind_strs: Option<Vec<String>> = params
        .get("kind")
        .map(|k| k.split(',').map(|s| s.to_string()).collect());
    let kind_refs: Option<Vec<&str>> = kind_strs
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect());

    let search = params.get("search").map(|s| s.as_str());

    match RunStorage::open(run_id, &state.base_path) {
        Ok(storage) => {
            match storage.load_events_filtered(kind_refs.as_deref(), search, offset, per_page) {
                Ok((events, total)) => {
                    let total_pages = total.div_ceil(per_page as u64);
                    Json(serde_json::json!({
                        "events": events,
                        "total": total,
                        "page": page,
                        "per_page": per_page,
                        "total_pages": total_pages,
                    }))
                    .into_response()
                }
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            }
        }
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

async fn get_run_stats_handler(
    State(state): State<ViewerState>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    let run_id = RunId(run_id);
    match RunStorage::open(run_id, &state.base_path) {
        Ok(storage) => {
            let breakdown = storage.event_count_by_kind().unwrap_or_default();
            let timeline = storage.events_timeline().unwrap_or_default();
            let agent_runs = storage.agent_run_ids().unwrap_or_default();
            Json(serde_json::json!({
                "event_breakdown": breakdown,
                "timeline": timeline,
                "agent_run_count": agent_runs.len(),
            }))
            .into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// HTML Templates
// ---------------------------------------------------------------------------

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Clawprint Dashboard</title>
<style>
:root{--bg:#0a0e27;--bg2:#151932;--card:#1a1f3a;--accent:#00d4aa;--red:#ff4757;--orange:#ffa502;--blue:#3498db;--purple:#9b59b6;--text:#e1e8ed;--dim:#8895a7;--border:#2d3561}
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;background:var(--bg);color:var(--text);padding:24px;line-height:1.6}
.wrap{max-width:1400px;margin:0 auto}
header{margin-bottom:32px;padding-bottom:20px;border-bottom:2px solid var(--border)}
h1{font-size:1.8rem;color:var(--accent)}
header p{color:var(--dim);margin-top:4px}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:16px;margin-bottom:32px}
.card{background:var(--card);border:1px solid var(--border);border-radius:8px;padding:20px;border-left:4px solid var(--accent)}
.card-label{font-size:.8rem;color:var(--dim);text-transform:uppercase;letter-spacing:.5px;margin-bottom:6px}
.card-value{font-size:2rem;font-weight:700;color:var(--accent)}
h2{margin-bottom:12px}
.run-card{display:block;background:var(--card);border:1px solid var(--border);border-radius:8px;padding:18px;margin-bottom:12px;text-decoration:none;color:inherit;transition:border-color .2s,transform .15s}
.run-card:hover{border-color:var(--accent);transform:translateY(-2px)}
.run-card-header{display:flex;align-items:center;gap:10px;margin-bottom:12px}
.run-id{color:var(--accent);font-size:1.05rem}
.badge{padding:3px 10px;border-radius:10px;font-size:.7rem;font-weight:600;text-transform:uppercase;letter-spacing:.4px}
.badge.complete{background:rgba(0,212,170,.15);color:var(--accent);border:1px solid var(--accent)}
.badge.progress{background:rgba(255,165,2,.15);color:var(--orange);border:1px solid var(--orange)}
.lock{font-size:1rem;margin-left:auto}
.run-meta{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:10px;font-size:.85rem;color:var(--dim)}
.run-meta b{color:var(--text);margin-right:4px}
</style>
</head>
<body>
<div class="wrap">
<header>
<h1>Clawprint Dashboard</h1>
<p>Tamper-evident audit logs for OpenClaw agent activity</p>
</header>
<div class="cards">
<div class="card"><div class="card-label">Total Runs</div><div class="card-value">{{TOTAL_RUNS}}</div></div>
<div class="card"><div class="card-label">Total Events</div><div class="card-value">{{TOTAL_EVENTS}}</div></div>
<div class="card"><div class="card-label">Storage</div><div class="card-value">{{TOTAL_SIZE}}</div></div>
</div>
<h2>Recorded Runs</h2>
{{RUNS}}
</div>
</body>
</html>"##;

const RUN_DETAIL_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Run {{RUN_ID}} — Clawprint</title>
<style>
:root{--bg:#0a0e27;--bg2:#151932;--card:#1a1f3a;--accent:#00d4aa;--red:#ff4757;--orange:#ffa502;--blue:#3498db;--purple:#9b59b6;--text:#e1e8ed;--dim:#8895a7;--border:#2d3561}
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;background:var(--bg);color:var(--text);padding:24px;line-height:1.6}
.wrap{max-width:1400px;margin:0 auto}
a.back{color:var(--dim);text-decoration:none;display:inline-block;margin-bottom:16px}
a.back:hover{color:var(--accent)}
.header{background:var(--card);border:1px solid var(--border);border-radius:8px;padding:20px;margin-bottom:20px}
.header h1{font-size:1.5rem;color:var(--accent);margin-bottom:12px}
.stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(160px,1fr));gap:12px}
.stat{padding:10px;background:var(--bg2);border-radius:6px}
.stat-label{font-size:.75rem;color:var(--dim);text-transform:uppercase}
.stat-value{font-size:1.3rem;color:var(--accent);font-weight:600}
.section{background:var(--card);border:1px solid var(--border);border-radius:8px;padding:20px;margin-bottom:20px}
.section h2{font-size:1.1rem;margin-bottom:14px}
.bar-row{display:flex;align-items:center;gap:8px;margin-bottom:6px;font-size:.85rem}
.bar-label{width:120px;color:var(--dim)}
.bar-track{flex:1;background:var(--bg2);border-radius:3px;height:22px;position:relative}
.bar-fill{height:100%;border-radius:3px;background:linear-gradient(90deg,var(--accent),var(--blue));transition:width .3s}
.bar-num{position:absolute;right:6px;top:50%;transform:translateY(-50%);font-size:.75rem;color:#fff;font-weight:600}
.filters{display:flex;gap:8px;flex-wrap:wrap;align-items:center;margin-bottom:16px}
.fbtn{padding:6px 14px;border:1px solid var(--border);background:var(--bg2);color:var(--dim);border-radius:6px;cursor:pointer;font-size:.8rem;transition:all .15s}
.fbtn:hover{border-color:var(--accent);color:var(--text)}
.fbtn.on{background:var(--accent);color:var(--bg);border-color:var(--accent)}
.search{flex:1;min-width:180px;padding:6px 12px;background:var(--bg2);border:1px solid var(--border);border-radius:6px;color:var(--text);font-size:.85rem}
.search:focus{outline:none;border-color:var(--accent)}
.events{display:flex;flex-direction:column;gap:8px}
.ev{background:var(--card);border:1px solid var(--border);border-radius:8px;padding:14px;border-left:4px solid var(--border)}
.ev.RUN_START{border-left-color:var(--accent)}.ev.RUN_END{border-left-color:var(--red)}
.ev.TOOL_CALL{border-left-color:var(--orange)}.ev.TOOL_RESULT{border-left-color:var(--blue)}
.ev.OUTPUT_CHUNK{border-left-color:var(--blue)}.ev.AGENT_EVENT{border-left-color:var(--purple)}
.ev.TICK{border-left-color:var(--dim)}.ev.PRESENCE{border-left-color:var(--dim)}
.ev-head{display:flex;justify-content:space-between;align-items:center;margin-bottom:6px}
.ev-kind{font-weight:600;font-size:.85rem;font-family:'Courier New',monospace}
.ev-kind.RUN_START{color:var(--accent)}.ev-kind.RUN_END{color:var(--red)}
.ev-kind.TOOL_CALL{color:var(--orange)}.ev-kind.OUTPUT_CHUNK{color:var(--blue)}
.ev-kind.AGENT_EVENT{color:var(--purple)}.ev-kind.CUSTOM{color:var(--dim)}
.ev-ts{font-size:.8rem;color:var(--dim)}
.ev-hash{font-size:.7rem;color:var(--dim);font-family:monospace;margin-bottom:6px}
.ev-payload{background:var(--bg2);border-radius:4px;padding:10px;max-height:0;overflow:hidden;transition:max-height .3s ease}
.ev-payload.open{max-height:600px;overflow-y:auto}
.ev-payload pre{margin:0;font-size:.78rem;white-space:pre-wrap;word-break:break-word;color:var(--text)}
.toggle{background:none;border:none;color:var(--accent);cursor:pointer;font-size:.8rem;padding:0;margin-bottom:4px}
.toggle:hover{text-decoration:underline}
.pager{display:flex;gap:8px;justify-content:center;margin-top:20px}
.pbtn{padding:6px 14px;background:var(--card);border:1px solid var(--border);border-radius:6px;color:var(--text);cursor:pointer;transition:all .15s}
.pbtn:hover:not(:disabled){border-color:var(--accent)}
.pbtn.cur{background:var(--accent);color:var(--bg);border-color:var(--accent)}
.pbtn:disabled{opacity:.4;cursor:not-allowed}
.loading{text-align:center;padding:40px;color:var(--dim)}
</style>
</head>
<body>
<div class="wrap">
<a href="/" class="back">&larr; Dashboard</a>
<div class="header">
<h1>Run <span id="rid">{{RUN_ID}}</span></h1>
<div class="stats">
<div class="stat"><div class="stat-label">Events</div><div class="stat-value" id="s-events">-</div></div>
<div class="stat"><div class="stat-label">Duration</div><div class="stat-value" id="s-dur">-</div></div>
<div class="stat"><div class="stat-label">Agent Runs</div><div class="stat-value" id="s-agents">-</div></div>
<div class="stat"><div class="stat-label">Integrity</div><div class="stat-value" id="s-integrity">-</div></div>
</div>
</div>

<div class="section">
<h2>Event Breakdown</h2>
<div id="chart"></div>
</div>

<div class="section">
<h2>Events</h2>
<div class="filters">
<span style="color:var(--dim);font-size:.85rem">Filter:</span>
<button class="fbtn" data-k="AGENT_EVENT">AGENT_EVENT</button>
<button class="fbtn" data-k="OUTPUT_CHUNK">OUTPUT_CHUNK</button>
<button class="fbtn" data-k="TOOL_CALL">TOOL_CALL</button>
<button class="fbtn" data-k="RUN_START">RUN_START</button>
<button class="fbtn" data-k="RUN_END">RUN_END</button>
<button class="fbtn" data-k="TICK">TICK</button>
<button class="fbtn" data-k="CUSTOM">CUSTOM</button>
<input class="search" id="q" placeholder="Search payload...">
</div>
<div class="events" id="evlist"><div class="loading">Loading...</div></div>
<div class="pager" id="pager"></div>
</div>
</div>

<script>
const R='{{RUN_ID}}';let page=1,filters=new Set(),search='',pages=1;

async function init(){
 const[run,stats]=await Promise.all([
  fetch('/api/runs/'+R).then(r=>r.json()),
  fetch('/api/runs/'+R+'/stats').then(r=>r.json())
 ]);
 document.getElementById('rid').textContent=R.substring(0,8);
 document.getElementById('s-events').textContent=run.event_count;
 document.getElementById('s-agents').textContent=stats.agent_run_count;
 const si=document.getElementById('s-integrity');
 si.textContent=run.chain_valid?'\u2713 Valid':'\u2717 Invalid';
 si.style.color=run.chain_valid?'var(--accent)':'var(--red)';
 renderChart(stats.event_breakdown);
 loadEvents();
}

function renderChart(b){
 const el=document.getElementById('chart');
 const total=Object.values(b).reduce((s,v)=>s+v,0);
 const sorted=Object.entries(b).sort((a,c)=>c[1]-a[1]);
 el.innerHTML=sorted.map(([k,v])=>{
  const p=(v/total*100).toFixed(1);
  return '<div class="bar-row"><div class="bar-label">'+esc(k)+'</div><div class="bar-track"><div class="bar-fill" style="width:'+p+'%"></div><div class="bar-num">'+v+'</div></div></div>';
 }).join('');
}

async function loadEvents(){
 const p=new URLSearchParams({page,per_page:50});
 if(filters.size)p.set('kind',[...filters].join(','));
 if(search)p.set('search',search);
 const d=await fetch('/api/runs/'+R+'/events?'+p).then(r=>r.json());
 pages=d.total_pages;
 renderEvents(d.events);
 renderPager();
}

function renderEvents(evs){
 const el=document.getElementById('evlist');
 if(!evs.length){el.innerHTML='<div class="loading">No events found</div>';return;}
 el.innerHTML=evs.map(e=>{
  const collapse=e.kind==='TICK'||e.kind==='PRESENCE';
  const ts=new Date(e.ts).toLocaleString();
  const hash=e.hash_self?e.hash_self.substring(0,16):'';
  const payload=JSON.stringify(e.payload,null,2);
  return '<div class="ev '+esc(e.kind)+'">'
   +'<div class="ev-head"><span class="ev-kind '+esc(e.kind)+'">'+esc(e.kind)+'</span><span class="ev-ts">'+esc(ts)+'</span></div>'
   +'<div class="ev-hash">'+esc(hash)+'</div>'
   +'<button class="toggle" onclick="tog(this)">'+(collapse?'Show':'Hide')+' payload</button>'
   +'<div class="ev-payload'+(collapse?'':' open')+'"><pre>'+esc(payload)+'</pre></div>'
   +'</div>';
 }).join('');
}

function tog(btn){
 const p=btn.nextElementSibling;
 p.classList.toggle('open');
 btn.textContent=p.classList.contains('open')?'Hide payload':'Show payload';
}

function renderPager(){
 const el=document.getElementById('pager');
 let h='<button class="pbtn" '+(page<=1?'disabled':'')+' onclick="go('+(page-1)+')">Prev</button>';
 const start=Math.max(1,page-2),end=Math.min(pages,start+4);
 for(let i=start;i<=end;i++)h+='<button class="pbtn'+(i===page?' cur':'')+'" onclick="go('+i+')">'+i+'</button>';
 if(end<pages)h+='<span style="color:var(--dim);padding:6px">...'+pages+'</span>';
 h+='<button class="pbtn" '+(page>=pages?'disabled':'')+' onclick="go('+(page+1)+')">Next</button>';
 el.innerHTML=h;
}

function go(p){if(p>=1&&p<=pages){page=p;loadEvents();}}
function esc(s){const d=document.createElement('div');d.textContent=s;return d.innerHTML;}

document.querySelectorAll('.fbtn[data-k]').forEach(b=>{
 b.addEventListener('click',()=>{
  const k=b.dataset.k;
  if(filters.has(k)){filters.delete(k);b.classList.remove('on');}
  else{filters.add(k);b.classList.add('on');}
  page=1;loadEvents();
 });
});

let st;document.getElementById('q').addEventListener('input',e=>{
 clearTimeout(st);st=setTimeout(()=>{search=e.target.value.trim();page=1;loadEvents();},300);
});

init();
</script>
</body>
</html>"##;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_html() {
        assert_eq!(
            escape_html("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html(r#"x="y""#), "x=&quot;y&quot;");
        assert_eq!(escape_html("it's"), "it&#x27;s");
        assert_eq!(escape_html("safe text"), "safe text");
    }

    #[test]
    fn test_format_duration_html() {
        use chrono::{TimeZone, Utc};
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 1, 1, 1, 30, 45).unwrap();
        assert_eq!(format_duration_html(start, Some(end)), "1h 30m 45s");
        assert_eq!(format_duration_html(start, None), "recording...");
    }
}
