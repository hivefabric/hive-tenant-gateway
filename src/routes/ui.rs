//! Demo UI served by the tenant gateway at `GET /ui`.
//!
//! Bare HTML + vanilla JS — no build step, no framework. Posts a queen task
//! to `/v1/mcp/tools/call` and renders the response. Designed to give a
//! human-visible end-to-end demo of the orchestrator → workers flow without
//! requiring the operator to assemble curl commands by hand.
//!
//! When the gateway is started in dev mode (in-memory tenant store with a
//! seed tenant), the seed bearer is interpolated into the HTML so the form
//! works out of the box. In production mode (Postgres-backed store, no
//! seed) the page renders an empty key field and the operator must paste
//! a tenant API key manually.

use axum::{response::Html, routing::get, Router};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/ui", get(serve_ui))
}

async fn serve_ui(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Html<String> {
    let seed_key = state.dev_seed_key.clone().unwrap_or_default();
    let queen_urn = state
        .demo_queen_urn
        .clone()
        .unwrap_or_else(|| "oasf://demo/queen/decompose/v1".to_string());
    let html = TEMPLATE
        .replace("{{SEED_KEY}}", &seed_key)
        .replace("{{QUEEN_URN}}", &queen_urn);
    Html(html)
}

const TEMPLATE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>HiveFabric — Demo Console</title>
<meta name="viewport" content="width=device-width, initial-scale=1" />
<style>
  :root {
    --bg: #0d1117;
    --panel: #161b22;
    --border: #30363d;
    --text: #e6edf3;
    --muted: #8b949e;
    --accent: #f5b800;
    --accent-dim: #b88800;
    --good: #3fb950;
    --bad: #f85149;
    --code: #21262d;
  }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--text);
         font: 14px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
  header { padding: 18px 28px; border-bottom: 1px solid var(--border);
           display: flex; justify-content: space-between; align-items: center; }
  header h1 { margin: 0; font-size: 16px; font-weight: 700;
              letter-spacing: 0.04em; text-transform: uppercase; color: var(--accent); }
  header .sub { color: var(--muted); font-size: 12px; }
  main { display: grid; grid-template-columns: 1fr 1fr; gap: 18px; padding: 24px 28px; max-width: 1280px; }
  @media (max-width: 900px) { main { grid-template-columns: 1fr; } }
  .panel { background: var(--panel); border: 1px solid var(--border); border-radius: 6px; padding: 18px; }
  .panel h2 { margin: 0 0 12px; font-size: 13px; font-weight: 700;
              text-transform: uppercase; letter-spacing: 0.06em; color: var(--accent); }
  label { display: block; margin: 12px 0 4px; color: var(--muted); font-size: 12px; }
  textarea, input { width: 100%; background: var(--code); color: var(--text);
                    border: 1px solid var(--border); border-radius: 4px;
                    padding: 8px 10px; font: inherit; }
  textarea { min-height: 80px; resize: vertical; }
  button { margin-top: 14px; background: var(--accent); color: #1a1300;
           border: 0; border-radius: 4px; padding: 10px 18px; font-weight: 700;
           letter-spacing: 0.04em; cursor: pointer; }
  button:hover { background: var(--accent-dim); }
  button[disabled] { opacity: 0.5; cursor: progress; }
  .result-final { background: var(--code); border: 1px solid var(--border);
                  border-radius: 4px; padding: 14px; white-space: pre-wrap;
                  min-height: 60px; }
  .trace { display: flex; flex-direction: column; gap: 10px; max-height: 60vh; overflow: auto; }
  .turn { background: var(--code); border: 1px solid var(--border);
          border-radius: 4px; padding: 10px 12px; font-size: 12px; }
  .turn .head { color: var(--muted); margin-bottom: 6px; }
  .turn.kind-final .head { color: var(--good); }
  .turn.kind-tool .head { color: var(--accent); }
  .turn pre { margin: 4px 0 0; white-space: pre-wrap; word-break: break-word; }
  .err { color: var(--bad); }
  .meta { color: var(--muted); font-size: 11px; margin-top: 8px; }
  details { margin-top: 6px; }
  summary { cursor: pointer; color: var(--muted); }
</style>
</head>
<body>
<header>
  <h1>HiveFabric · Demo Console</h1>
  <span class="sub">queen ⇄ workers ⇄ SLM</span>
</header>
<main>
  <section class="panel">
    <h2>Submit task</h2>
    <label for="prompt">prompt</label>
    <textarea id="prompt">process (2+2)*2</textarea>
    <label for="key">tenant API key</label>
    <input id="key" placeholder="hf_..." value="{{SEED_KEY}}" />
    <label for="urn">queen capability URN</label>
    <input id="urn" value="{{QUEEN_URN}}" />
    <button id="go">▶ run</button>
    <div class="meta">
      The queen comb decomposes your prompt into sub-tasks via Anthropic, dispatches each as a new
      task to the worker comb, and aggregates the results.
    </div>
  </section>
  <section class="panel">
    <h2>Result</h2>
    <label>final_message</label>
    <div id="final" class="result-final">—</div>
    <label>trace</label>
    <div id="trace" class="trace"></div>
    <div class="meta" id="meta"></div>
  </section>
</main>
<script>
const $ = (id) => document.getElementById(id);
$('go').addEventListener('click', async () => {
  const prompt = $('prompt').value.trim();
  const key = $('key').value.trim();
  const urn = $('urn').value.trim();
  if (!prompt || !key || !urn) { alert('prompt, key, and URN are required'); return; }
  $('go').disabled = true;
  $('final').innerHTML = '⏳ running …';
  $('trace').innerHTML = '';
  $('meta').textContent = '';
  const t0 = performance.now();
  try {
    const resp = await fetch('/v1/mcp/tools/call', {
      method: 'POST',
      headers: { 'authorization': 'Bearer ' + key, 'content-type': 'application/json' },
      body: JSON.stringify({
        name: 'run_subagent',
        arguments: { capability_urn: urn, prompt: prompt, timeout_seconds: 180 },
      }),
    });
    const dt = (performance.now() - t0).toFixed(0);
    const body = await resp.json();
    if (!resp.ok) {
      $('final').innerHTML = '<span class="err">error ' + resp.status + '</span>\n' + JSON.stringify(body, null, 2);
      return;
    }
    const inner = body.output && body.output.output ? body.output.output : (body.output || body);
    const finalMsg = inner.final_message || inner.text || JSON.stringify(inner, null, 2);
    $('final').textContent = finalMsg;
    const trace = Array.isArray(inner.trace) ? inner.trace : [];
    $('trace').innerHTML = trace.map(renderTurn).join('');
    $('meta').textContent = 'iterations: ' + (inner.iterations ?? '—') +
                            ' · model: ' + (inner.model ?? '—') +
                            ' · wall: ' + dt + ' ms';
  } catch (e) {
    $('final').innerHTML = '<span class="err">' + e.message + '</span>';
  } finally {
    $('go').disabled = false;
  }
});
function renderTurn(t) {
  const kind = t.kind || 'unknown';
  if (kind === 'final_turn') {
    return '<div class="turn kind-final"><div class="head">iteration ' + t.iteration + ' · final_turn</div>' +
           '<pre>' + escapeHtml(t.text || '') + '</pre></div>';
  }
  const tools = (t.tools || []).map(renderTool).join('');
  return '<div class="turn kind-tool"><div class="head">iteration ' + t.iteration +
         ' · tool_turn · ' + (t.tools || []).length + ' call(s)</div>' + tools + '</div>';
}
function renderTool(tc) {
  const args = JSON.stringify(tc.input, null, 2);
  const result = tc.error
    ? '<span class="err">error: ' + escapeHtml(tc.error) + '</span>'
    : '<pre>' + escapeHtml(JSON.stringify(tc.result, null, 2)) + '</pre>';
  return '<details open><summary>' + escapeHtml(tc.name) + '(' + escapeHtml(args) + ')</summary>' + result + '</details>';
}
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, ch => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[ch]));
}
</script>
</body>
</html>"##;
