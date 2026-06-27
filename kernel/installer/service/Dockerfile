FROM rust:1.89-bookworm AS builder

ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ARG http_proxy
ARG https_proxy
ARG no_proxy

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY kernel/installer/core ./kernel/installer/core
COPY kernel/installer/service ./kernel/installer/service
COPY kernel/installer/cli ./kernel/installer/cli

RUN cargo build --release -p module-installer

FROM debian:bookworm-slim

WORKDIR /app
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /app/target/release/module-installer /usr/local/bin/module-installer

USER 65532:65532
EXPOSE 8090
CMD ["module-installer"]
