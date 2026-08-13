FROM golang:1.26.6 AS build

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

RUN CGO_ENABLED=0 go build -buildvcs=false -trimpath -ldflags="-s -w" -o /out/gateway .

FROM alpine:3.23 AS runtime

RUN apk add --no-cache ca-certificates \
    && addgroup -S -g 10001 ojos \
    && adduser -S -D -H -u 10001 -G ojos ojos

WORKDIR /app

COPY --from=build --chown=10001:10001 /out/gateway ./gateway
COPY --from=build --chown=10001:10001 /app/gateway/etc ./etc

USER 10001:10001

HEALTHCHECK --interval=10s --timeout=4s --start-period=15s --retries=3 CMD ["/app/gateway", "healthcheck"]

ENTRYPOINT ["/app/gateway"]
CMD ["-f", "etc/gateway.yaml"]
