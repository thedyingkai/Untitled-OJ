FROM rust:1.89-bookworm AS build

ARG GITHUB_SHA=development
ARG OJOS_BUILD_COMMIT=${GITHUB_SHA}

WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY services/orchestrator ./services/orchestrator
COPY manager ./manager

RUN OJOS_BUILD_COMMIT="$OJOS_BUILD_COMMIT" \
    cargo build --locked --release -p orchestrator-agent --bin ojos-orchestrator-agent

FROM debian:bookworm-slim AS runtime

ARG GITHUB_SHA=development
ARG OJOS_BUILD_COMMIT=${GITHUB_SHA}
LABEL org.opencontainers.image.revision="$OJOS_BUILD_COMMIT"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 10004 ojos-agent \
    && useradd -u 10004 -g 10004 -M -s /usr/sbin/nologin ojos-agent \
    && install -d -o 10004 -g 10004 -m 0750 /var/lib/ojos-agent

COPY --from=build /workspace/target/release/ojos-orchestrator-agent /usr/local/bin/ojos-orchestrator-agent

# Production hosts normally add the Docker socket group at runtime. The
# capacity compose does that per isolated Engine; the image itself stays
# non-root and has no Docker CLI or shell-based execution path.
USER 10004:10004
WORKDIR /var/lib/ojos-agent
ENTRYPOINT ["/usr/local/bin/ojos-orchestrator-agent"]
