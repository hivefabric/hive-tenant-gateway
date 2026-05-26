# CLAUDE.md — hive-tenant-gateway

## What this is

Multi-tenant BYO-LLM HTTP gateway for HiveFabric. Each customer brings their own frontier LLM API (Anthropic, OpenAI, or any OpenAI-compatible endpoint — Together, Groq, vLLM, Ollama) and authenticates as a tenant using a bearer token. The gateway dispatches work to the Comb network via three MCP-equivalent tools (`describe_cluster`, `run_subagent`, `estimate_cost`). Serves on port 8090 (`GATEWAY_BIND`, default `0.0.0.0:8090`). Called by the customer's agent or the `hive-gateway-tests` suite; calls Honeycomb internally.

## Key files

- `src/bin/tenant_gateway.rs` — binary entry point; reads env, builds `AppState`, runs DB migrations, starts Axum.
- `src/lib.rs` — `AppState`, `router()`, and re-exports. Frontier adapters are re-exported from `hive-sdk::frontier`.
- `src/vault.rs` — `KeyVault`: AES-256-GCM encryption for stored LLM API keys. Needs `TENANT_LLM_SECRET_KEY` (32-byte base64url). Without it, keys are stored with a `raw:` prefix and a startup warning.
- `src/tenant/` — `TenantStore` trait + `InMemoryTenantStore` (dev) + `PgTenantStore` (prod). Selection is runtime: `DATABASE_URL` set → Postgres; unset → in-memory with a dev seed tenant.
- `src/auth.rs` — Bearer token extraction and per-tenant scope checks (`tools:invoke`, `orchestrate`, `read:usage`). Argon2-hashed at rest; O(N) verify per request (tracked as Phase 2.3 perf item).
- `src/routes/orchestrate.rs` — `POST /v1/orchestrate`: gateway-managed tool loop using the `FrontierLlm` adapter. Accepts `messages` + `llm` provider config + `max_iterations`.
- `src/routes/mcp.rs` — `POST /v1/mcp/tools/list` and `POST /v1/mcp/tools/call`: customer-managed loop.
- `src/routes/signup.rs` — `POST /v1/signup`: self-service tenant provisioning (no admin key required).
- `src/routes/admin.rs` — `/admin/v1/*`: requires `x-admin-key: $HF_ADMIN_KEY`. Returns 503 if `HF_ADMIN_KEY` is unset.
- `src/ledger.rs` — `LedgerClient`: calls hive-ledger for debit/refund on each `run_subagent`. No-op when `LEDGER_URL` is unset.
- `migrations/` — SQL migrations applied automatically at startup.

## How to run

```bash
# Dev mode — in-memory store, seed tenant printed to stderr
HONEYCOMB_URL=http://localhost:8080 \
HONEYCOMB_API_KEY=dev-hive-key \
GATEWAY_BIND=0.0.0.0:8090 \
cargo run --bin tenant-gateway

# Production — Postgres + admin gate + vault
DATABASE_URL=postgres://hf:dev@localhost:5432/hf \
HF_ADMIN_KEY=$(openssl rand -base64 32) \
TENANT_LLM_SECRET_KEY=$(openssl rand -base64 32) \
HONEYCOMB_URL=http://localhost:8080 \
HONEYCOMB_API_KEY=dev-hive-key \
GATEWAY_BIND=0.0.0.0:8090 \
cargo run --bin tenant-gateway
```

## How to test

```bash
# Unit tests (no DB, no stack)
cargo test -p hive-tenant-gateway

# Integration tests — see hive-gateway-tests repo
```

## Architecture notes

- Self-service tenant flow: `POST /v1/signup` → `POST /v1/me/llm-providers` (register provider once) → `POST /v1/orchestrate` or `POST /v1/mcp/tools/call`.
- Frontier LLM adapters (`anthropic`, `openai`) were moved out of `src/frontier/` into `hive-sdk::frontier`. `src/lib.rs` re-exports them for back-compat. Do not re-add a local `src/frontier/` directory.
- `tenant_id` is always overridden from the bearer's tenant — the caller cannot spoof it. Honeycomb stamps it on every `TaskRecord`.
- `HF_ADMIN_KEY` unset → admin surface is disabled (all `/admin/v1/*` return 503). This is intentional for dev setups.
- Rate limiter (`src/rate_limit.rs`): per-tenant, default 300 RPM. Configured via `TENANT_RATE_LIMIT_RPM`.
- Vault: generate key with `openssl rand -base64 32`. Without it, LLM API keys are stored plaintext with a `raw:` prefix and a loud startup warning — acceptable in dev, never in prod.

### Key env vars

| Var | Default | Purpose |
|---|---|---|
| `HONEYCOMB_URL` | — | Required. Honeycomb base URL. |
| `HONEYCOMB_API_KEY` | — | Required. Honeycomb x-api-key. |
| `GATEWAY_BIND` | `0.0.0.0:8090` | Bind address. |
| `DATABASE_URL` | — | Postgres DSN. Unset = in-memory dev mode. |
| `HF_ADMIN_KEY` | — | Admin surface key. Unset = admin disabled. |
| `TENANT_LLM_SECRET_KEY` | — | 32-byte base64url key for vault. Unset = plaintext dev mode. |
| `LEDGER_URL` | — | hive-ledger URL. Unset = billing disabled. |
| `TENANT_RATE_LIMIT_RPM` | `300` | Per-tenant requests per minute. |

## What's not done

- Gemini and Bedrock frontier adapters are not implemented (Phase 2.4).
- Bearer-token O(1) lookup: today every resolve is an O(N) Argon2 verify — viable for the first hundred tenants, tracked as Phase 2.3 perf.
- Per-tenant stored LLM provider registry exists in DB (`tenant_llm_providers`) but the full self-service CRUD flow is partially implemented.
- Honey Ledger budget reservation/refund cycle is wired but the reserve-before-dispatch pattern is not enforced end-to-end.
