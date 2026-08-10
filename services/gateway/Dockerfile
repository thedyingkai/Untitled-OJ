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

COPY services/gateway/go.mod services/gateway/go.sum ./gateway/
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/

WORKDIR /app/gateway
RUN go mod download

WORKDIR /app

COPY services/gateway ./gateway
COPY platform/shared/go /platform/shared/go

WORKDIR /app/gateway

RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/gateway .

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=build /out/gateway ./gateway
COPY --from=build /app/gateway/etc ./etc

CMD ["./gateway", "-f", "etc/gateway.yaml"]
