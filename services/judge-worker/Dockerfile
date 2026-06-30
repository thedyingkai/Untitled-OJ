FROM rust:1.89-bookworm

ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ARG http_proxy
ARG https_proxy
ARG no_proxy

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        git \
        autoconf \
        bison \
        flex \
        gcc \
        g++ \
        libprotobuf-dev \
        libnl-route-3-dev \
        libtool \
        make \
        pkg-config \
        protobuf-compiler \
        bash \
        coreutils \
        python3 \
        openjdk-17-jdk \
    && git clone --depth=1 https://github.com/google/nsjail.git /tmp/nsjail \
    && make -C /tmp/nsjail \
    && cp /tmp/nsjail/nsjail /usr/local/bin/nsjail \
    && mkdir -p /jail/root /var/lib/ojos-worker/work /var/lib/ojos-worker/cache \
    && groupadd -g 10001 sandbox \
    && useradd -u 10001 -g 10001 -M -s /usr/sbin/nologin sandbox \
    && rm -rf /tmp/nsjail \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY services/judge-worker ./judge-worker

WORKDIR /app/judge-worker

RUN cargo build --release

CMD ["./target/release/judge-worker"]
