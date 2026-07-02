FROM rust:1.89-bookworm

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

RUN cargo build --release -p ojos-orchestrator-daemon

CMD ["./target/release/ojos-orchestrator-daemon", "--repo-root", "/workspace", "--bind", "0.0.0.0:8090"]
