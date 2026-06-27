FROM rust:1.89-bookworm AS builder

ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ARG http_proxy
ARG https_proxy
ARG no_proxy

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY crates/module-installer-core ./crates/module-installer-core
COPY services/module-installer ./services/module-installer
COPY tools/ojosctl ./tools/ojosctl

RUN cargo build --release -p module-installer

FROM rust:1.89-bookworm

WORKDIR /app
COPY --from=builder /app/target/release/module-installer /usr/local/bin/module-installer

EXPOSE 8090
CMD ["module-installer"]
