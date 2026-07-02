FROM rust:1.89-bookworm

ARG NSJAIL_COMMIT=d6454b4640b6d8699b532a8afa37e4d67e477078
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
    && git init /tmp/nsjail \
    && git -C /tmp/nsjail remote add origin https://github.com/google/nsjail.git \
    && git -C /tmp/nsjail fetch --depth=1 origin "$NSJAIL_COMMIT" \
    && git -C /tmp/nsjail checkout --detach FETCH_HEAD \
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

RUN test "$(cat config/runtime-lock.yaml | sed -n 's/^nsjail_commit: //p')" = "$NSJAIL_COMMIT" \
    && g++ -dumpversion | grep -Eq '^12($|\.)' \
    && gcc -dumpversion | grep -Eq '^12($|\.)' \
    && python3 -c 'import sys; assert sys.version_info[:2] == (3, 11), sys.version' \
    && java -version 2>&1 | grep -q 'version "17\.' \
    && nsjail --help >/tmp/nsjail-help.txt \
    && mkdir -p /opt/ojos \
    && gxx_version="$(g++ -dumpfullversion)" \
    && gcc_version="$(gcc -dumpfullversion)" \
    && python_version="$(python3 -c 'import sys; print(".".join(map(str, sys.version_info[:3])))')" \
    && java_version="$(java -version 2>&1 | head -n 1)" \
    && { \
        echo "image=rust:1.89-bookworm"; \
        echo "nsjail_commit=$NSJAIL_COMMIT"; \
        echo "g++=$gxx_version"; \
        echo "gcc=$gcc_version"; \
        echo "python=$python_version"; \
        echo "java=$java_version"; \
      } > /opt/ojos/runtime-versions.txt

RUN mkdir -p /opt/ojos/default-components \
    && g++ default-components/default_runner.cpp -O2 -pipe -std=c++17 -o /opt/ojos/default-components/default-runner \
    && g++ default-components/default_validator.cpp -O2 -pipe -std=c++17 -o /opt/ojos/default-components/default-validator \
    && g++ default-components/default_checker.cpp -O2 -pipe -std=c++17 -o /opt/ojos/default-components/default-checker \
    && g++ default-components/default_scorer.cpp -O2 -pipe -std=c++17 -o /opt/ojos/default-components/default-scorer

RUN cargo build --release

CMD ["./target/release/judge-worker"]
