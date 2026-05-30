FROM rust:1.89-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        g++ \
        gcc \
        python3 \
        openjdk-17-jdk \
        golang-go \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY judge-worker ./judge-worker

WORKDIR /app/judge-worker

RUN cargo build --release

CMD ["./target/release/judge-worker"]