FROM rust:1.89-slim-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked -j 2

FROM debian:bookworm-slim

RUN mkdir -p /data && chown 10001:10001 /data
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /app/target/release/mcp-db /usr/local/bin/mcp-db

USER 10001:10001
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8787
EXPOSE 8788
ENV MCPDB_ADDR=0.0.0.0:8787
ENV MCPDB_ADMIN_ADDR=0.0.0.0:8788
ENV MCPDB_STATE_PATH=/data/state.db
ENV MCPDB_HEALTHCHECK_ADDR=127.0.0.1:8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/mcp-db", "--healthcheck"]
ENTRYPOINT ["/usr/local/bin/mcp-db"]
