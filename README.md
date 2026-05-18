# hive-tenant-gateway

Multi-tenant **BYO-LLM HTTP gateway** for HiveFabric. Each customer brings their own frontier LLM via API (Anthropic / OpenAI / Gemini / Bedrock / self-hosted) and connects it to the Comb network through HTTP equivalents of the MCP tools.

The customer owns the orchestrator loop. We own the network and the SLM substrate.

## Status

Phase 2.2 — Postgres persistence + admin auth gate.

- Bearer-token auth, per-tenant API keys (Argon2-hashed at rest, never plaintext).
- Per-tenant scopes (`tools:invoke`, `orchestrate`, `read:usage`).
- **Tenant runs the loop:** `POST /v1/mcp/tools/list`, `POST /v1/mcp/tools/call`.
- **Gateway runs the loop:** `POST /v1/orchestrate` with the `FrontierLlm` adapter trait. Adapters today: Anthropic (Messages API) + OpenAI (Chat Completions, also covers any OpenAI-compatible provider — Together, Groq, vLLM, Ollama — via `base_url` override). Gemini / Bedrock land additively.
- **Admin auth gate:** `/admin/v1/*` requires `x-admin-key: $HF_ADMIN_KEY`, constant-time compared. If `HF_ADMIN_KEY` is unset, the admin surface is disabled (every endpoint returns 503).
- **Postgres-backed `TenantStore`** behind the same trait as the in-memory dev store. Selection is runtime: `DATABASE_URL` set → Postgres + migrations; unset → in-memory + dev seed tenant.
- `tenant_id` propagated through `TaskCreateRequest` and stamped on every Honeycomb `TaskRecord`. Spoof-prevention: gateway always overrides caller-supplied `tenant_id` with the bearer's tenant.

Not yet:
- Per-tenant LLM provider registry (today: customer sends key in each `/v1/orchestrate` body).
- KMS for tenant-side LLM API keys.
- Honey Ledger budget reservation/refund cycle.
- Gemini / Bedrock adapters.
- Bearer-token public/secret split for O(1) resolve (today: O(N) argon2 verify per resolve — viable for the first hundred tenants, tracked as Phase 2.3 perf).

See [`docs/02_architecture/18_tenant_gateway.md`](https://github.com/hivefabric/.github-private/blob/main/docs/private/docs/02_architecture/18_tenant_gateway.md) in the private docs.

## Run locally

### Dev mode (in-memory store)

```bash
# 1. Start Honeycomb + a Comb node (the SLM side):
cd ../honeycomb/docker
docker compose -f docker-compose.with-node.yml up -d

# 2. Start the tenant gateway in dev mode (in-memory + seed tenant):
cd ../../hive-tenant-gateway
HONEYCOMB_URL=http://localhost:8080 \
HONEYCOMB_API_KEY=dev-hive-key \
GATEWAY_BIND=0.0.0.0:8090 \
cargo run --bin tenant-gateway
```

The dev binary prints a seed tenant API key on stderr at boot. Admin endpoints
return 503 unless you also set `HF_ADMIN_KEY`.

### Production mode (Postgres + admin gate)

```bash
docker run -d --name hf-pg \
  -e POSTGRES_PASSWORD=dev -e POSTGRES_USER=hf -e POSTGRES_DB=hf \
  -p 5432:5432 postgres:16

DATABASE_URL=postgres://hf:dev@localhost:5432/hf \
HF_ADMIN_KEY=$(openssl rand -base64 32) \
HONEYCOMB_URL=http://localhost:8080 \
HONEYCOMB_API_KEY=dev-hive-key \
GATEWAY_BIND=0.0.0.0:8090 \
cargo run --bin tenant-gateway
```

Migrations run automatically. Provision a tenant:

```bash
curl -s -X POST \
  -H "x-admin-key: $HF_ADMIN_KEY" \
  -H "content-type: application/json" \
  -d '{"name":"acme"}' \
  http://localhost:8090/admin/v1/tenants | jq .
```

The dev binary prints a seed tenant API key on stderr at boot:

```
[tenant-gateway] dev seed tenant ready
[tenant-gateway]   tenant_id   = ...
[tenant-gateway]   API KEY (shown once) = hf_...
```

Use that key as `Authorization: Bearer ...`:

```bash
KEY="hf_..."

# Identity check
curl -s -H "Authorization: Bearer $KEY" http://localhost:8090/v1/whoami | jq .

# Tool catalog
curl -s -X POST -H "Authorization: Bearer $KEY" http://localhost:8090/v1/mcp/tools/list | jq .

# Run a generic-inference call
curl -s -X POST \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{
    "name": "run_subagent",
    "arguments": {
      "model_id": "qwen2.5:0.5b",
      "prompt": "Classify the sentiment of: '\''great game!'\''. Reply with one word: positive | negative | neutral."
    }
  }' \
  http://localhost:8090/v1/mcp/tools/call | jq .
```

## How a customer wires their queen agent

Two paths:

### Path A — customer runs the loop (`POST /v1/mcp/tools/call`)

The customer's orchestrator (their preferred LLM) sees HiveFabric as a tool provider. Three tools, registered into the LLM's function-calling schema:

```json
{
  "tools": [
    { "name": "describe_cluster", "input_schema": { ... } },
    { "name": "run_subagent",     "input_schema": { ... } },
    { "name": "estimate_cost",    "input_schema": { ... } }
  ]
}
```

Tool calls become POSTs to `/v1/mcp/tools/call`; tool results are fed back to the customer's LLM. The customer drives the loop end-to-end. We just dispatch.

### Path B — gateway runs the loop (`POST /v1/orchestrate`)

For customers who don't want to maintain an agent loop in their own code: send a chat completion + LLM provider config in one request. We run the tool loop internally and return the final assistant message + a trace.

**Anthropic (Claude):**

```bash
curl -s -X POST \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{
    "messages": [
      {"role": "user", "content": "Classify the sentiment of: '\''great game!'\''"}
    ],
    "llm": {
      "provider": "anthropic",
      "model": "claude-3-5-sonnet-latest",
      "api_key": "sk-ant-..."
    },
    "max_iterations": 10
  }' \
  http://localhost:8090/v1/orchestrate | jq .
```

**OpenAI (GPT):**

```bash
curl -s -X POST \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{
    "messages": [
      {"role": "user", "content": "Classify the sentiment of: '\''great game!'\''"}
    ],
    "llm": {
      "provider": "openai",
      "model": "gpt-4o",
      "api_key": "sk-..."
    },
    "max_iterations": 10
  }' \
  http://localhost:8090/v1/orchestrate | jq .
```

**OpenAI-compatible** (Together, Groq, vLLM, Ollama, etc.) — same `provider: "openai"`, point `base_url` elsewhere:

```bash
"llm": {
  "provider": "openai",
  "model": "llama3.1:70b",
  "api_key": "...",
  "base_url": "https://api.together.xyz"
}
```

Returns:

```json
{
  "final_message": "The sentiment is positive.",
  "stop_reason": "end_turn",
  "trace": [
    {"kind": "tool_turn", "iteration": 1, "tools": [
      {"tool_use_id": "...", "name": "run_subagent", "arguments": {...}, "result": {...}}
    ]},
    {"kind": "final_turn", "iteration": 2, "text": "The sentiment is positive."}
  ]
}
```

The customer's API key never touches our DB — it lives in the request body. Phase 2.2 adds a server-side `tenant_llm_providers` registry so customers can register a provider once and reference it by id.

Requires the `orchestrate` API key scope.

## Tests

```bash
cargo test -p hive-tenant-gateway
```
