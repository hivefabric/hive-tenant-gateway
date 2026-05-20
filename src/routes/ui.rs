//! Demo UI served by the tenant gateway at `GET /ui`.
//!
//! Two panels:
//!   * Submit form + result/trace — exercises the full
//!     orchestrator → workers → SLM flow via `/v1/mcp/tools/call`.
//!   * Stack overview — pulls aggregated network state from
//!     `/v1/_demo/overview` (nodes, recent tasks, capabilities, ledger
//!     balance + events). Auto-refreshes every 5 s.
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
  .panel.full { grid-column: 1 / -1; }
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
  .turn.kind-tool .head, .turn.kind-step .head { color: var(--accent); }
  .turn pre { margin: 4px 0 0; white-space: pre-wrap; word-break: break-word; }
  .err { color: var(--bad); }
  .meta { color: var(--muted); font-size: 11px; margin-top: 8px; }
  details { margin-top: 6px; }
  summary { cursor: pointer; color: var(--muted); }

  /* Overview tables */
  .overview { display: grid; grid-template-columns: repeat(4, 1fr); gap: 12px; margin-top: 8px; }
  @media (max-width: 1100px) { .overview { grid-template-columns: 1fr 1fr; } }
  @media (max-width: 600px)  { .overview { grid-template-columns: 1fr;     } }
  .stat { background: var(--code); border: 1px solid var(--border); border-radius: 4px; padding: 10px 12px; }
  .stat .label { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; }
  .stat .value { font-size: 22px; font-weight: 700; color: var(--accent); }
  table { width: 100%; border-collapse: collapse; font-size: 12px; margin-top: 12px; }
  th, td { text-align: left; padding: 6px 8px; border-bottom: 1px solid var(--border); }
  th { color: var(--muted); font-weight: 500; text-transform: uppercase; font-size: 10px; letter-spacing: 0.06em; }
  td.mono { color: var(--text); }
  td .ok  { color: var(--good); }
  td .bad { color: var(--bad); }
  .ov-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 18px; margin-top: 14px; }
  @media (max-width: 1100px) { .ov-grid { grid-template-columns: 1fr; } }
  .ov-grid h3 { margin: 0 0 8px; font-size: 11px; color: var(--muted);
                text-transform: uppercase; letter-spacing: 0.06em; }
  .truncate { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  /* Workflow chart */
  .workflow-tabs { display: flex; gap: 8px; margin-bottom: 12px; }
  .wf-tab { background: var(--code); color: var(--muted); border: 1px solid var(--border);
            border-radius: 4px; padding: 6px 12px; font: inherit; cursor: pointer;
            margin-top: 0; font-weight: 500; letter-spacing: 0; }
  .wf-tab:hover { color: var(--text); }
  .wf-tab.active { background: var(--accent); color: #1a1300; border-color: var(--accent); }
  .wf-pane { background: white; border: 1px solid var(--border); border-radius: 4px;
             min-height: 280px; padding: 12px; overflow-x: auto; }
  .wf-pane:empty::before { content: "(submit a task to populate)"; color: var(--muted);
                           display: block; padding: 100px 0; text-align: center; font-size: 12px; }
  .wf-legend { display: flex; gap: 10px; margin-top: 10px; flex-wrap: wrap; font-size: 11px; }
  .legend-pill { padding: 3px 9px; border-radius: 12px; font-weight: 600; letter-spacing: 0.04em; }
  .agent-queen  { background: #533200; color: var(--accent); }
  .agent-worker { background: #0d4429; color: var(--good); }
  .agent-leaf   { background: #21262d; color: var(--muted); border: 1px solid var(--border); }
</style>
<script src="https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.min.js"></script>
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
      The queen comb decomposes your prompt into sub-tasks, dispatches each as a new task
      to the worker comb, and aggregates the results. Default queen handler is
      <code>queen:expression</code> (deterministic, no LLM key needed).
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

  <section class="panel full">
    <h2>Workflow · who did what</h2>
    <div class="workflow-tabs">
      <button class="wf-tab active" data-wf="ast">decomposition tree</button>
      <button class="wf-tab" data-wf="seq">cross-comb sequence</button>
    </div>
    <div id="wf-ast" class="wf-pane"></div>
    <div id="wf-seq" class="wf-pane" style="display:none"></div>
    <div class="wf-legend">
      <span class="legend-pill agent-queen">queen comb</span>
      <span class="legend-pill agent-worker">worker comb</span>
      <span class="legend-pill agent-leaf">literal</span>
    </div>
  </section>

  <section class="panel full">
    <h2>Stack overview · auto-refresh 5 s</h2>
    <div class="overview">
      <div class="stat"><div class="label">combs registered</div><div class="value" id="ov-nodes">—</div></div>
      <div class="stat"><div class="label">capabilities</div><div class="value" id="ov-caps">—</div></div>
      <div class="stat"><div class="label">recent tasks</div><div class="value" id="ov-tasks">—</div></div>
      <div class="stat"><div class="label">ledger balance</div><div class="value" id="ov-ledger">—</div></div>
    </div>
    <div class="ov-grid">
      <div>
        <h3>Combs</h3>
        <table id="ov-nodes-tbl">
          <thead><tr><th>node_id</th><th>profiles</th><th>cpu%</th><th>mem%</th></tr></thead>
          <tbody></tbody>
        </table>
      </div>
      <div>
        <h3>Capabilities</h3>
        <table id="ov-caps-tbl">
          <thead><tr><th>urn</th><th>workload</th><th>p50 ms</th></tr></thead>
          <tbody></tbody>
        </table>
      </div>
      <div>
        <h3>Recent tasks</h3>
        <table id="ov-tasks-tbl">
          <thead><tr><th>task_id</th><th>status</th><th>node</th><th>ms</th></tr></thead>
          <tbody></tbody>
        </table>
      </div>
      <div>
        <h3>Ledger events</h3>
        <table id="ov-ledger-tbl">
          <thead><tr><th>kind</th><th>delta</th><th>correlation</th></tr></thead>
          <tbody></tbody>
        </table>
      </div>
    </div>
    <div class="meta" id="ov-meta"></div>
  </section>
</main>
<script>
const $ = (id) => document.getElementById(id);

// Mermaid renders inside a white container so its default theme works
// without us needing to vendor a dark theme. Disable security to allow
// dynamic graph injection.
mermaid.initialize({ startOnLoad: false, securityLevel: 'loose', theme: 'default' });

// ─── Workflow tab switcher ─────────────────────────────────────────────
document.querySelectorAll('.wf-tab').forEach(btn => {
  btn.addEventListener('click', () => {
    document.querySelectorAll('.wf-tab').forEach(b => b.classList.remove('active'));
    btn.classList.add('active');
    const which = btn.dataset.wf;
    document.getElementById('wf-ast').style.display = which === 'ast' ? '' : 'none';
    document.getElementById('wf-seq').style.display = which === 'seq' ? '' : 'none';
  });
});

// ─── Submit panel ──────────────────────────────────────────────────────
$('go').addEventListener('click', async () => {
  const prompt = $('prompt').value.trim();
  const key    = $('key').value.trim();
  const urn    = $('urn').value.trim();
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
    const inner = unwrap(body);
    const finalMsg = inner.final_message || inner.text || JSON.stringify(inner, null, 2);
    $('final').textContent = finalMsg;
    const trace = Array.isArray(inner.trace) ? inner.trace : [];
    $('trace').innerHTML = trace.map(renderTraceItem).join('') || '<div class="meta">(no trace)</div>';
    const parts = [];
    if (inner.iterations !== undefined) parts.push('iterations: ' + inner.iterations);
    if (inner.expression)               parts.push('expr: ' + inner.expression);
    if (inner.result !== undefined)     parts.push('result: ' + inner.result);
    if (inner.model)                    parts.push('model: ' + inner.model);
    parts.push('wall: ' + dt + ' ms');
    $('meta').textContent = parts.join(' · ');
    // Render the workflow diagrams from the queen's response.
    await renderWorkflow(inner);
  } catch (e) {
    $('final').innerHTML = '<span class="err">' + escapeHtml(e.message) + '</span>';
  } finally {
    $('go').disabled = false;
    refreshOverview(); // immediate refresh after a task
  }
});

// ─── Workflow chart ────────────────────────────────────────────────────
async function renderWorkflow(inner) {
  const wfAst = $('wf-ast');
  const wfSeq = $('wf-seq');
  const ast = inner && inner.ast ? inner.ast : null;
  const trace = Array.isArray(inner && inner.trace) ? inner.trace : [];
  const expr = inner && inner.expression ? inner.expression : '';
  const finalVal = inner && inner.result !== undefined ? inner.result : null;
  const queenUrn = $('urn').value.trim();

  // Annotate AST: walk post-order, attach the matching trace step's
  // `urn` and `value` to each Binary node. queen:expression evaluates
  // post-order so the trace order matches.
  let stepCursor = 0;
  function annotate(node) {
    if (!node) return;
    if (node.kind === 'binary') {
      annotate(node.left);
      annotate(node.right);
      const step = trace[stepCursor++];
      if (step) {
        node._urn = step.urn;
        node._value = step.value;
        node._step = step.step;
      }
    }
  }
  if (ast) annotate(ast);

  // Build a Mermaid `graph TD` from the annotated tree.
  if (ast) {
    let nodeId = 0;
    const lines = [];
    const classDefs = [];
    const opSym = { add: '+', sub: '-', mul: '×', div: '÷' };

    function visit(node) {
      const id = 'n' + (nodeId++);
      if (node.kind === 'int') {
        lines.push(`${id}[${node.value}]`);
        classDefs.push(`class ${id} leaf`);
      } else if (node.kind === 'binary') {
        const sym = opSym[node.op] || node.op;
        const agentName = shortenUrn(node._urn || '');
        const value = node._value !== undefined ? node._value : '?';
        const stepLabel = node._step ? `step ${node._step}` : '';
        const label = `${stepLabel}<br/><b>${sym}</b> → <b>${value}</b><br/><i>${agentName}</i>`;
        lines.push(`${id}["${label}"]`);
        classDefs.push(`class ${id} worker`);
        const leftId = visit(node.left);
        const rightId = visit(node.right);
        lines.push(`${id} --> ${leftId}`);
        lines.push(`${id} --> ${rightId}`);
      }
      return id;
    }

    const queenId = 'q0';
    const rootId = visit(ast);
    const queenLabel = `<b>queen</b><br/>${escapeHtml(expr)}<br/>→ <b>${finalVal}</b>`;
    lines.unshift(`${queenId}["${queenLabel}"] --> ${rootId}`);
    classDefs.unshift(`class ${queenId} queen`);

    const mermaidSrc = [
      'graph TD',
      'classDef queen fill:#fff3c4,stroke:#b88800,color:#1a1300,font-weight:bold',
      'classDef worker fill:#d1f0d3,stroke:#3fb950,color:#0d3318',
      'classDef leaf fill:#f2f2f2,stroke:#cfcfcf,color:#333',
      ...lines,
      ...classDefs,
    ].join('\n');

    wfAst.innerHTML = `<div class="mermaid">${mermaidSrc}</div>`;
    try { await mermaid.run({ nodes: wfAst.querySelectorAll('.mermaid') }); } catch (e) {
      wfAst.textContent = 'mermaid render failed: ' + e.message + '\n\n' + mermaidSrc;
    }
  } else {
    wfAst.innerHTML = '<div style="padding:40px;text-align:center;color:#888">'
      + 'No AST in response (set <code>include_ast = true</code> on the queen capability config).</div>';
  }

  // Sequence diagram: from user → tg → honeycomb → queen → workers.
  // We don't have wall-time-ordered records of every hop, but each trace
  // step is one full sub-task round trip, so we can render them in order.
  const seqLines = [
    'sequenceDiagram',
    '    autonumber',
    '    actor U as User',
    '    participant TG as Tenant Gateway',
    '    participant HC as Honeycomb',
    '    participant Q as Queen Comb',
    '    participant W as Worker Comb',
    '    participant O as Ollama',
    `    U->>+TG: prompt "${escapeHtml((expr || '?').slice(0, 40))}"`,
    `    TG->>+HC: TaskCreate(${shortenUrn(queenUrn)})`,
    '    HC->>+Q: ExecuteRequest',
  ];
  trace.forEach(step => {
    const sym = ({add: '+', sub: '-', mul: '*', div: '/'}[step.op] || step.op);
    const a = step.inputs && step.inputs.a;
    const b = step.inputs && step.inputs.b;
    seqLines.push(`    Q->>+HC: TaskCreate(${shortenUrn(step.urn)}, ${a} ${sym} ${b})`);
    seqLines.push('    HC->>+W: ExecuteRequest');
    seqLines.push(`    W->>+O: chat completion`);
    seqLines.push(`    O-->>-W: "${step.value}"`);
    seqLines.push(`    W-->>-HC: succeeded(${step.value})`);
    seqLines.push(`    HC-->>-Q: result(${step.value})`);
  });
  seqLines.push(`    Q-->>-HC: succeeded(${finalVal})`);
  seqLines.push(`    HC-->>-TG: result(${finalVal})`);
  seqLines.push(`    TG-->>-U: ${expr || ''} = ${finalVal}`);
  const seqSrc = seqLines.join('\n');
  wfSeq.innerHTML = `<div class="mermaid">${seqSrc}</div>`;
  try { await mermaid.run({ nodes: wfSeq.querySelectorAll('.mermaid') }); } catch (e) {
    wfSeq.textContent = 'mermaid render failed: ' + e.message + '\n\n' + seqSrc;
  }
}

function shortenUrn(urn) {
  if (!urn) return '';
  // oasf://demo/agents/sum/v1 → "sum_agent (demo)"
  const m = /^oasf:\/\/([^\/]+)\/([^\/]+)\/([^\/]+)\/v\d+$/.exec(urn);
  if (m) {
    const [, ns, dom, op] = m;
    if (dom === 'agents') return op + '_agent';
    if (dom === 'queen') return 'queen.' + op;
    return `${dom}.${op}`;
  }
  return urn;
}

function unwrap(body) {
  // tenant-gateway wraps run_subagent's result as `output`. Honeycomb's
  // TaskView wraps the agent output again as `output`. Peel both.
  const outer = body && body.output ? body.output : body;
  if (outer && outer.output && (outer.status || outer.task_id)) return outer.output;
  return outer || body;
}

function renderTraceItem(t) {
  // Two trace shapes:
  //   queen:expression — { step, op, urn, inputs, raw_result, value }
  //   queen:anthropic  — { iteration, kind: "tool_turn"|"final_turn", tools|text }
  if (t && t.kind === 'final_turn') {
    return '<div class="turn kind-final"><div class="head">iteration ' + t.iteration + ' · final</div>' +
           '<pre>' + escapeHtml(t.text || '') + '</pre></div>';
  }
  if (t && t.kind === 'tool_turn') {
    const tools = (t.tools || []).map(renderTool).join('');
    return '<div class="turn kind-tool"><div class="head">iteration ' + t.iteration +
           ' · tool_turn · ' + (t.tools || []).length + ' call(s)</div>' + tools + '</div>';
  }
  if (t && t.op !== undefined) {
    // queen:expression step
    const stepNo = t.step ?? '?';
    const args = JSON.stringify(t.inputs || {}, null, 2);
    const value = t.value !== undefined ? String(t.value) : '?';
    const drift = t.slm_drift
      ? '<div class="meta err">slm drift: said ' + escapeHtml(String(t.slm_drift.slm_said)) +
        ', actual ' + escapeHtml(String(t.slm_drift.actual)) + '</div>'
      : '';
    const raw = t.raw_result
      ? '<details><summary>raw sub-task result</summary><pre>' +
        escapeHtml(JSON.stringify(t.raw_result, null, 2)) + '</pre></details>'
      : '';
    return '<div class="turn kind-step"><div class="head">step ' + stepNo + ' · ' +
           escapeHtml(t.op) + ' · → ' + escapeHtml(value) + '</div>' +
           '<div>' + escapeHtml(t.urn || '') + '</div>' +
           '<pre>' + escapeHtml(args) + '</pre>' + drift + raw + '</div>';
  }
  return '<div class="turn"><pre>' + escapeHtml(JSON.stringify(t, null, 2)) + '</pre></div>';
}

function renderTool(tc) {
  const args = JSON.stringify(tc.input ?? tc.arguments ?? {}, null, 2);
  const result = tc.error
    ? '<span class="err">error: ' + escapeHtml(tc.error) + '</span>'
    : '<pre>' + escapeHtml(JSON.stringify(tc.result, null, 2)) + '</pre>';
  return '<details open><summary>' + escapeHtml(tc.name) + '(' + escapeHtml(args) + ')</summary>' + result + '</details>';
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, ch => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[ch]));
}

// ─── Overview panel ────────────────────────────────────────────────────
async function refreshOverview() {
  const key = $('key').value.trim();
  if (!key) return;
  try {
    const resp = await fetch('/v1/_demo/overview', {
      headers: { 'authorization': 'Bearer ' + key },
    });
    if (!resp.ok) { $('ov-meta').textContent = 'overview: ' + resp.status; return; }
    const data = await resp.json();
    renderOverview(data);
  } catch (e) {
    $('ov-meta').textContent = 'overview error: ' + e.message;
  }
}

function renderOverview(d) {
  const nodes = Array.isArray(d.nodes) ? d.nodes : [];
  const caps  = (d.capabilities && Array.isArray(d.capabilities.capabilities))
                  ? d.capabilities.capabilities
                  : [];
  const tasks = Array.isArray(d.tasks) ? d.tasks : [];

  $('ov-nodes').textContent = nodes.length;
  $('ov-caps').textContent  = caps.length;
  $('ov-tasks').textContent = tasks.length;
  $('ov-ledger').textContent =
    (d.ledger_balance !== undefined && d.ledger_balance !== null) ? d.ledger_balance : '—';

  $('ov-nodes-tbl').querySelector('tbody').innerHTML = nodes.map(n => {
    const id   = (n.node_id || '').slice(0, 8) + '…';
    const prof = (n.llm_profiles || []).join(', ');
    const cpu  = n.cpu_usage_percent  !== undefined && n.cpu_usage_percent  !== null
                 ? n.cpu_usage_percent.toFixed(0) + '%' : '—';
    const mem  = n.memory_usage_percent !== undefined && n.memory_usage_percent !== null
                 ? n.memory_usage_percent.toFixed(0) + '%' : '—';
    return '<tr><td class="mono truncate">' + escapeHtml(id) + '</td><td>' +
            escapeHtml(prof) + '</td><td>' + cpu + '</td><td>' + mem + '</td></tr>';
  }).join('') || '<tr><td colspan="4" class="meta">no combs</td></tr>';

  $('ov-caps-tbl').querySelector('tbody').innerHTML = caps.map(c =>
    '<tr><td class="mono truncate">' + escapeHtml(c.urn || '') +
    '</td><td>' + escapeHtml(c.workload || c.handler || '') +
    '</td><td>' + (c.latency_p50_ms ?? '—') + '</td></tr>'
  ).join('') || '<tr><td colspan="3" class="meta">no capabilities</td></tr>';

  $('ov-tasks-tbl').querySelector('tbody').innerHTML = tasks.slice(0, 10).map(t => {
    const tid = (t.task_id || '').slice(0, 8) + '…';
    const stat = t.status || '—';
    const cls = stat === 'succeeded' ? 'ok' : (stat === 'failed' || stat === 'timed_out' ? 'bad' : '');
    const node = (t.assigned_node_id || '').slice(0, 8) + (t.assigned_node_id ? '…' : '—');
    return '<tr><td class="mono truncate">' + escapeHtml(tid) + '</td><td><span class="' +
            cls + '">' + escapeHtml(stat) + '</span></td><td class="mono truncate">' +
            escapeHtml(node) + '</td><td>' + (t.execution_time_ms ?? '—') + '</td></tr>';
  }).join('') || '<tr><td colspan="4" class="meta">no tasks</td></tr>';

  const events = (d.ledger_events && Array.isArray(d.ledger_events))
                   ? d.ledger_events
                   : (d.ledger_events && Array.isArray(d.ledger_events.events))
                       ? d.ledger_events.events
                       : [];
  $('ov-ledger-tbl').querySelector('tbody').innerHTML = events.slice(0, 10).map(e =>
    '<tr><td>' + escapeHtml(e.kind || '') +
    '</td><td>' + (e.delta_credits ?? '—') +
    '</td><td class="mono truncate">' + escapeHtml(e.correlation || '') + '</td></tr>'
  ).join('') || '<tr><td colspan="3" class="meta">no events (ledger may be off, see configuration)</td></tr>';

  $('ov-meta').textContent = 'tenant_id: ' + (d.tenant_id || '—') + ' · refreshed ' + new Date().toISOString().slice(11, 19);
}

setInterval(refreshOverview, 5000);
refreshOverview();
</script>
</body>
</html>"##;
