# MCP Wars server: one long-running process. The universe lives in Postgres when
# DATABASE_URL is set (production on Render), otherwise in a redb file under /data.
# Build:  docker build -t mcpwars .
# Run:    docker run -p 8080:8080 -e DATABASE_URL=postgres://... -e AF_ADMIN_TOKEN=... mcpwars
#         (add -e AF_CREATE_UNIVERSE=1 for the first start on an empty database only)
#    or:  docker run -p 8080:8080 -v mcpwars-data:/data -e AF_ADMIN_TOKEN=... mcpwars

# The build image names the exact toolchain `rust-toolchain.toml` pins; change both together.
FROM rust:1.96.0-bookworm AS build
WORKDIR /src
COPY . .
# crates.io occasionally drops connections mid-download: retry, avoid HTTP/2 multiplexing (the
# usual trigger for "HTTP2 framing layer" resets), and fetch before compiling.
ENV CARGO_NET_RETRY=10 CARGO_HTTP_MULTIPLEXING=false CARGO_HTTP_TIMEOUT=120
RUN cargo fetch --locked || (sleep 15 && cargo fetch --locked) || (sleep 45 && cargo fetch --locked)
RUN cargo build --release --frozen -p af-mcp --bin af-server

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 tini util-linux \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 10001 --create-home mcpwars \
 && mkdir -p /data
COPY --from=build /src/target/release/af-server /usr/local/bin/af-server
COPY scripts/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
VOLUME /data
# The entrypoint binds 0.0.0.0:$PORT (default 8080) unless AF_BIND is set.
EXPOSE 8080
# tini forwards SIGTERM/SIGINT so the server writes its shutdown snapshot.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/docker-entrypoint.sh"]
# --adopt-universe is a no-op when the engine version matches, and lets engine
# upgrades ship as a plain deploy (it needs the snapshot a graceful stop or a
# handover writes). --db is ignored when DATABASE_URL is set.
CMD ["serve", "--db", "/data/universe.redb", "--adopt-universe"]
