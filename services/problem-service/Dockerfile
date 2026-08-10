FROM golang:1.26.5 AS build

ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ARG http_proxy
ARG https_proxy
ARG no_proxy
ARG GOPROXY=https://proxy.golang.org,direct

ENV GOPROXY=${GOPROXY}

WORKDIR /app

COPY services/problem-service/go.mod services/problem-service/go.sum ./problem-service/
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/

WORKDIR /app/problem-service
RUN go mod download

WORKDIR /app

COPY services/problem-service ./problem-service
COPY platform/shared/go /platform/shared/go

WORKDIR /app/problem-service

RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/problem-service .

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=build /out/problem-service ./problem-service
COPY --from=build /app/problem-service/etc ./etc

RUN mkdir -p /data/ojos/problems

HEALTHCHECK --interval=2s --timeout=5s --start-period=5s --retries=30 CMD ["./problem-service", "healthcheck"]

CMD ["./problem-service", "-f", "etc/problemapi.yaml"]
