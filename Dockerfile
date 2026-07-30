# ---- Build stage ----
FROM rust:1.97-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    python3 \
    python3-dev \
    pkg-config \
    ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache deps separately from sources.
# Workspace members: root crate (.), sdk/rust, sdk/python
# Stub all bin/test/bench paths Cargo.toml references so manifest parses.
COPY Cargo.toml Cargo.lock build.rs ./
COPY proto ./proto
COPY sdk ./sdk
RUN mkdir -p src/bin/conproxy sdk/rust/src sdk/python/src \
  && echo 'fn main() { println!("placeholder"); }' > src/bin/conproxy/main.rs \
  && echo 'pub fn _x() {}' > src/lib.rs \
  && echo 'fn main() {}' > src/bin/test_runner.rs \
  && echo 'fn main() {}' > src/bin/generate_embeddings.rs \
  && echo 'fn main() {}' > src/bin/perf_summarize.rs \
  && echo 'fn main() {}' > src/bin/hitrate_bench.rs \
  && echo 'fn main() {}' > src/bin/console_snap.rs \
  && echo 'fn main() {}' > src/bin/corpus_seed.rs \
 && echo 'pub fn _x() {}' > sdk/rust/src/lib.rs \
 && echo 'pub fn _x() {}' > sdk/python/src/lib.rs \
  && mkdir -p tests/e2e_eval tests/e2e_proxy tests/e2e_uat tests/e2e_sdk_python tests/e2e_load benches \
  && touch tests/e2e_eval/main.rs tests/e2e_proxy/main.rs tests/e2e_uat/main.rs \
         tests/e2e_sdk_python/main.rs tests/e2e_load/main.rs \
         tests/integration_qdrant.rs tests/integration_elasticsearch.rs \
         tests/integration_pgvector.rs tests/integration_meilisearch.rs \
         tests/integration_opensearch.rs tests/integration_cascade.rs \
         tests/integration_peer.rs tests/integration_circuit.rs \
         tests/integration_batch.rs tests/integration_metrics.rs \
         tests/integration_context_config.rs tests/integration_singleflight.rs \
         tests/integration_milvus.rs tests/integration_pinecone.rs \
         benches/core_ops.rs benches/cascade_scope.rs \
 && cargo build --release --bin conproxy --features release \
 && rm -rf src sdk/rust/src sdk/python/src

COPY src ./src
COPY sdk/rust/src ./sdk/rust/src
COPY sdk/python/src ./sdk/python/src
COPY ui ./ui
RUN touch src/bin/conproxy/main.rs src/lib.rs && cargo build --release --bin conproxy --features release

# ---- Runtime stage ----
# Uses rustls (no libssl needed). Just needs glibc + ca-certificates for
# TLS root validation via webpki-roots.
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN groupadd --system --gid 10001 conproxy \
 && useradd --system --uid 10001 --gid conproxy --home-dir /var/lib/conproxy --create-home conproxy

COPY --from=builder /build/target/release/conproxy /usr/local/bin/conproxy
RUN chown conproxy:conproxy /usr/local/bin/conproxy

USER conproxy
WORKDIR /var/lib/conproxy

# Default gRPC port (HTTP REST = gRPC + 1).
EXPOSE 9999 10000

# Default listen on 0.0.0.0 so the proxy is reachable from outside the
# container. Use CONPROXY_CONFIG to point at a config file.
ENTRYPOINT ["conproxy"]
CMD ["start", "--listen", "0.0.0.0:9999"]
