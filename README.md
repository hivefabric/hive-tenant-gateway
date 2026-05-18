# hive-tenant-gateway

Multi-tenant **BYO-LLM HTTP gateway** for HiveFabric. Each customer brings their own frontier LLM via API (Anthropic / OpenAI / Gemini / Bedrock / self-hosted) and connects it to the Comb network through HTTP equivalents of the MCP tools.

The customer owns the orchestrator loop. We own the network and the SLM substrate.

## Status

Phase 2.1 — orchestration endpoint live.

- Bearer-token auth, per-tenant API keys (Argon2-hashed at rest, never plaintext).
- Per-tenant scopes (`tools:invoke`, `orchestrate`, `read:usage`).
- **Tenant runs the loop:** `POST /v1/mcp/tools/list`, `POST /v1/mcp/tools/call`.
- **Gateway runs the loop:** `POST /v1/orchestrate` — customer sends `{messages, llm}`; we drive the multi-turn tool loop using their LLM via the `FrontierLlm` adapter trait. Anthropic Messages API adapter ships today; OpenAI / Gemini / Bedrock / OpenAI-compatible adapters land additively.
- Admin provisioning: `POST /admin/v1/tenants`, `POST /admin/v1/tenants/:id/api-keys`, `DELETE /admin/v1/api-keys/:id`.
- `tenant_id` propagated through `TaskCreateRequest` and stamped on every Honeycomb `TaskRecord`. Spoof-prevention: gateway always overrides caller-supplied `tenant_id` with the bearer's tenant.
- In-memory tenant store. Postgres swap is Phase 2.2.

Not yet:
- Postgres-backed tenant store + admin auth gate (today: open; **do not deploy without one**).
- Per-tenant LLM provider registry (today: customer sends key in each `/v1/orchestrate` body).
- KMS for tenant-side LLM API keys.
- Honey Ledger budget reservation/refund cycle.
- OpenAI / Gemini / Bedrock adapters.

See [`docs/02_architecture/18_tenant_gateway.md`](https://github.com/hivefabric/.github-private/blob/main/docs/private/docs/02_architecture/18_tenant_gateway.md) in the private docs.

## Run locally

```bash
# 1. Start Honeycomb + a Comb node (the SLM side):
cd ../honeycomb/docker
docker compose -f docker-compose.with-node.yml up -d

# 2. Start the tenant gateway:
cd ../../hive-tenant-gateway
HONEYCOMB_URL=http://localhost:8080 \
HONEYCOMB_API_KEY=dev-hive-key \
GATEWAY_BIND=0.0.0.0:8090 \
cargo run --bin tenant-gateway
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
