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

COPY services/storage-service/go.mod services/storage-service/go.sum ./storage-service/
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/

WORKDIR /app/storage-service
RUN go mod download

WORKDIR /app
COPY services/storage-service ./storage-service
COPY platform/shared/go /platform/shared/go

WORKDIR /app/storage-service
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/storage-service .

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=build /out/storage-service ./storage-service
COPY --from=build /app/storage-service/etc ./etc

RUN mkdir -p /data/ojos/storage

HEALTHCHECK --interval=2s --timeout=5s --start-period=5s --retries=30 CMD ["./storage-service", "healthcheck"]

CMD ["./storage-service", "-f", "etc/storageservice.yaml"]
