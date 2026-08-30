# syntax=docker/dockerfile:1

# Three image targets, one Dockerfile:
#   combined (default) — the Rust control plane serves the built web client
#       from the same origin as /api and /health, so one container starts
#       frontend and backend together as one process. This is the default
#       target: a plain `docker build .` produces it.
#   server    — backend only: janus-server + janus-admin, no web client.
#       Reach it through a frontend proxy (the web image, or any reverse
#       proxy) that forwards /api and /health.
#   web       — frontend only: nginx serves the built web client and proxies
#       /api and /health (WebSocket upgrade included) to the backend at
#       $JANUS_API_TARGET (env var, default http://127.0.0.1:4317).
#
# The web and server images share the same builder stages, so buildx caches
# the Bun and Rust compiles once and reuses them across all three targets.

# ============================================
# Stage 1: Web client build
# ============================================
FROM oven/bun:1.3.14 AS web-builder

WORKDIR /app/apps/web

# Dependency manifests first so image rebuilds skip the install on source-only
# changes.
COPY apps/web/package.json apps/web/bun.lock ./
RUN bun install --frozen-lockfile

COPY apps/web/ ./

# src/generated/api.ts is committed, so the bundle builds without the Rust
# OpenAPI generation chain.
RUN bun run build

# ============================================
# Stage 2: Server build
# ============================================
FROM rust:1.97.0-bookworm AS server-builder

# ring (rustls) compiles C sources; pkg-config keeps any -sys crates able to
# probe the sysroot during the build.
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Every workspace member must be present for cargo to resolve the workspace.
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY apps/server/ apps/server/
COPY crates/ crates/
COPY tools/ tools/

RUN cargo build --release -p janus-server

# ============================================
# Stage 3: Runtime base — shared by the combined and server images
# ============================================
FROM debian:bookworm-slim AS runtime-base

# git backs the source-control adapter; aria2 downloads GitHub repos as
# tarballs (git clone is cut with fatal 128 on the deployment host); tini reaps
# the session and terminal processes Janus spawns.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git aria2 tini \
    && rm -rf /var/lib/apt/lists/*

# JANUS_DEV_AUTH must be off because the listener is not loopback. Real
# deployments additionally set JANUS_MODE=production and an https
# JANUS_PUBLIC_ORIGIN pointing at the public hostname.
#
# MongoDB is the sole durable store and the connection must be a replica set
# (multi-document transactions are required), so JANUS_MONGODB_URI here is only
# the built-in default; deployments override it with their own address.
ENV JANUS_BIND=0.0.0.0:4317 \
    JANUS_DEV_AUTH=false \
    JANUS_DATA_ROOT=/data \
    JANUS_MONGODB_URI=mongodb://localhost:27017/?replicaSet=rs0 \
    JANUS_MONGODB_DATABASE=janus

WORKDIR /app

# janus-admin issues the initialization and recovery tokens. It opens the data
# root exclusively, so it runs as a one-off container against the same volume
# while the server container is stopped.
COPY --from=server-builder /app/target/release/janus-server /usr/local/bin/janus-server
COPY --from=server-builder /app/target/release/janus-admin /usr/local/bin/janus-admin

RUN useradd --system --create-home --home-dir /home/janus --shell /usr/sbin/nologin janus \
    && mkdir -p /data \
    && chown -R janus:janus /app /data

USER janus

VOLUME ["/data"]

# ============================================
# Stage 4: Server-only image — backend without the web client
# ============================================
FROM runtime-base AS server

# JANUS_WEB_DIST is intentionally unset: this image serves no web client. Point
# a frontend (the web image, or any reverse proxy) at it.

EXPOSE 4317

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["janus-server"]

# ============================================
# Stage 5: Web image — nginx serving the built client, proxying /api + /health
# to the backend at $JANUS_API_TARGET
# ============================================
FROM nginx:1.31-alpine AS web

# gettext provides envsubst, used by nginx-entrypoint.sh to substitute
# JANUS_API_TARGET into the template without touching nginx's own $variables.
RUN apk add --no-cache gettext

COPY --from=web-builder /app/apps/web/dist /usr/share/nginx/html
COPY docker/nginx.conf.template /etc/nginx/conf.d/default.conf.template
COPY docker/nginx-entrypoint.sh /usr/local/bin/nginx-entrypoint.sh

ENV JANUS_API_TARGET=http://127.0.0.1:4317

RUN chmod +x /usr/local/bin/nginx-entrypoint.sh

EXPOSE 80

ENTRYPOINT ["/usr/local/bin/nginx-entrypoint.sh"]
CMD ["nginx", "-g", "daemon off;"]

# ============================================
# Stage 6: Combined image (default target) — web client + server, one process
# ============================================
FROM runtime-base AS combined

ENV JANUS_WEB_DIST=/app/web

COPY --chown=janus:janus --from=web-builder /app/apps/web/dist /app/web

EXPOSE 4317

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["janus-server"]
