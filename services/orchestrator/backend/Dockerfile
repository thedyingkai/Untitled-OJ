# 编排器 daemon 的三阶段构建：
#   web     —— 构建 Vue3 + Vite 的 Web UI（daemon 在 `/` 静态托管的主入口）
#   build   —— 构建 Rust daemon 二进制，并挑出 daemon 在 --repo-root 下要读的运行时数据
#   runtime —— 不含任何工具链的最小运行镜像，非 root 用户 + /health 健康检查
#
# 运行镜像内的布局：
#   /usr/local/bin/ojos-orchestrator-daemon   二进制
#   /workspace                                --repo-root（platform/ services/ sets/ store/，可写）
#   /app/manager/web/dist                     --web-root（Web UI 构建产物）

# ---------------------------------------------------------------- stage: web
FROM node:24.11-bookworm-slim AS web

ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ARG http_proxy
ARG https_proxy
ARG no_proxy

WORKDIR /web

# 根 .dockerignore 已排除 **/node_modules 与 **/dist，这里只会带进源码和 npm 元数据。
COPY manager/web ./

# package-lock.json 已纳入仓库；缺失或与 package.json 不一致时直接终止构建。
RUN npm ci --no-audit --no-fund \
    && npm run typecheck \
    && npm run build \
    && test -f dist/index.html

# -------------------------------------------------------------- stage: build
FROM rust:1.89-bookworm AS build

ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ARG http_proxy
ARG https_proxy
ARG no_proxy

WORKDIR /workspace

COPY Cargo.toml Cargo.lock ./
COPY services/orchestrator ./services/orchestrator
COPY services/auth-service ./services/auth-service
COPY services/gateway ./services/gateway
COPY services/judge-api ./services/judge-api
COPY services/judge-worker ./services/judge-worker
COPY services/problem-service ./services/problem-service
COPY services/storage-service ./services/storage-service
COPY services/user-service ./services/user-service
COPY manager ./manager
COPY platform ./platform
COPY sets ./sets
COPY store ./store

RUN cargo build --release -p ojos-orchestrator-daemon

# daemon 在 --repo-root 下只读这几类文件：platform/schemas/orchestrator（schema）、
# services/*/{service,release}.yaml（服务契约）、sets/（服务集）、store/index.json（商店索引，
# OJOS_STORE_INDEX_URL 的默认值）。把它们挑到 /runtime，运行镜像就不必带上整棵源码树。
RUN set -eu; \
    mkdir -p /runtime/platform/schemas /runtime/services /runtime/sets /runtime/store; \
    cp -R platform/schemas/orchestrator /runtime/platform/schemas/orchestrator; \
    if [ -d sets ]; then cp -R sets/. /runtime/sets/; fi; \
    if [ -f store/index.json ]; then cp store/index.json /runtime/store/index.json; fi; \
    for svc_dir in services/*/; do \
        name="$(basename "$svc_dir")"; \
        if [ -f "$svc_dir/service.yaml" ] || [ -f "$svc_dir/release.yaml" ]; then \
            mkdir -p "/runtime/services/$name"; \
            if [ -f "$svc_dir/service.yaml" ]; then cp "$svc_dir/service.yaml" "/runtime/services/$name/"; fi; \
            if [ -f "$svc_dir/release.yaml" ]; then cp "$svc_dir/release.yaml" "/runtime/services/$name/"; fi; \
        fi; \
    done

# ------------------------------------------------------------ stage: runtime
FROM debian:bookworm-slim AS runtime

# ca-certificates：daemon 用 ureq + rustls 访问 GitHub / release 包源，需要系统根证书。
# curl：HEALTHCHECK 与 compose healthcheck 使用（slim 镜像默认既无 curl 也无 wget）。
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 10003 ojos \
    && useradd -u 10003 -g 10003 -M -s /usr/sbin/nologin ojos \
    && install -d -o 10003 -g 10003 -m 0755 /workspace

COPY --from=build /workspace/target/release/ojos-orchestrator-daemon /usr/local/bin/ojos-orchestrator-daemon

# --repo-root 必须可写：UI 布局落在 .ojos/ui-layout.json，release 包缓存落在
# .orchestrator-release-cache/（compose 把 ORCHESTRATOR_RELEASE_PACKAGE_ROOT 也指向这里）。
COPY --from=build --chown=10003:10003 /runtime/ /workspace/

# Web UI 产物；daemon 的 --web-root 默认是 <repo_root>/manager/web/dist，这里把它与 repo
# 数据分开放到 /app 下，因此下面的 CMD 显式传 --web-root。
COPY --from=web --chown=10003:10003 /web/dist/ /app/manager/web/dist/

WORKDIR /workspace
USER 10003:10003

EXPOSE 8090

# GET /health 是唯一免 ORCHESTRATOR_INTERNAL_TOKEN 的路由，健康检查因此无需带令牌。
HEALTHCHECK --interval=10s --timeout=3s --start-period=20s --retries=6 \
    CMD curl -fsS http://127.0.0.1:8090/health || exit 1

CMD ["ojos-orchestrator-daemon", \
     "--repo-root", "/workspace", \
     "--bind", "0.0.0.0:8090", \
     "--web-root", "/app/manager/web/dist"]
