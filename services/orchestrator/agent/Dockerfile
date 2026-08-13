FROM rust:1.89-bookworm AS build

ARG GITHUB_SHA=development
ARG OJOS_BUILD_COMMIT=${GITHUB_SHA}

WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY services/orchestrator ./services/orchestrator
COPY manager ./manager
COPY platform ./platform

RUN OJOS_BUILD_COMMIT="$OJOS_BUILD_COMMIT" \
    cargo build --locked --release -p orchestrator-agent --bin ojos-orchestrator-agent

FROM debian:bookworm-slim AS runtime

ARG GITHUB_SHA=development
ARG OJOS_BUILD_COMMIT=${GITHUB_SHA}
LABEL org.opencontainers.image.revision="$OJOS_BUILD_COMMIT"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 65532 ojos-agent \
    && useradd -u 65532 -g 65532 -M -s /usr/sbin/nologin ojos-agent \
    && install -d -o 65532 -g 65532 -m 0750 /var/lib/ojos-agent

COPY --from=build /workspace/target/release/ojos-orchestrator-agent /usr/local/bin/ojos-orchestrator-agent

# Production adds only the isolated Docker socket GID as a supplemental group.
# The primary identity matches signed standard-v3 workloads so the Agent can
# atomically rotate 0600 bind-mounted context/resource files without CHOWN.
USER 65532:65532
WORKDIR /var/lib/ojos-agent
ENTRYPOINT ["/usr/local/bin/ojos-orchestrator-agent"]
