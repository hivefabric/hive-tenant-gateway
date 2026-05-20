# syntax=docker/dockerfile:1.7
#
# Tenant Gateway — multi-tenant BYO-LLM HTTP entry point.
# Build context = ~/agent-cloud/hivefabric/ (workspace root).

FROM rust:1.89-bookworm AS builder
WORKDIR /workspace
COPY hive-sdk ./hive-sdk
COPY hive-mcp-gateway ./hive-mcp-gateway
COPY hive-tenant-gateway ./hive-tenant-gateway
WORKDIR /workspace/hive-tenant-gateway
RUN cargo build --release --locked --bin tenant-gateway

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -u 10005 -m -d /home/tenant-gateway tenant-gateway

COPY --from=builder /workspace/hive-tenant-gateway/target/release/tenant-gateway /usr/local/bin/tenant-gateway

EXPOSE 8090
USER tenant-gateway
WORKDIR /home/tenant-gateway

ENTRYPOINT ["/usr/local/bin/tenant-gateway"]
