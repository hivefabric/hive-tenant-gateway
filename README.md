# hive-tenant-gateway

Multi-tenant **BYO-LLM HTTP gateway** for HiveFabric. Each customer brings their own frontier LLM via API (Anthropic / OpenAI / Gemini / Bedrock / self-hosted) and connects it to the Comb network through HTTP equivalents of the MCP tools.

The customer owns the orchestrator loop. We own the network and the SLM substrate.

## Status

Phase 2.0 — initial scaffold.

- Bearer-token auth, per-tenant API keys (Argon2-hashed at rest, never plaintext).
- Per-tenant scopes (`tools:invoke`, `orchestrate`, `read:usage`).
- HTTP tool surface: `POST /v1/mcp/tools/list`, `POST /v1/mcp/tools/call`.
- Admin provisioning: `POST /admin/v1/tenants`, `POST /admin/v1/tenants/:id/api-keys`, `DELETE /admin/v1/api-keys/:id`.
- In-memory tenant store. Postgres swap is Phase 2.1.

Not yet:
- `POST /v1/orchestrate` (we run the loop with the tenant's chosen LLM).
- Admin auth gate (today: open; **do not deploy without one**).
- Per-tenant `tenant_id` propagation through Honeycomb's `TaskCreateRequest`.
- Honey Ledger budget reservation/refund cycle.
- Postgres-backed tenant store.
- KMS for tenant-side LLM API keys.

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

## Tests

```bash
cargo test -p hive-tenant-gateway
```
