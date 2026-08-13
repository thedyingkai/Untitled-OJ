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

COPY services/auth-service/go.mod services/auth-service/go.sum ./auth-service/
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/

WORKDIR /app/auth-service
RUN go mod download

WORKDIR /app

COPY services/auth-service ./auth-service
COPY platform/shared/go /platform/shared/go

WORKDIR /app/auth-service

RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/auth .

FROM alpine:3.22 AS runtime

RUN addgroup -S -g 65532 ojos && adduser -S -D -H -u 65532 -G ojos ojos

WORKDIR /app

COPY --from=build --chown=65532:65532 --chmod=0555 /out/auth ./auth
COPY --from=build --chown=65532:65532 --chmod=0444 /app/auth-service/etc ./etc

USER 65532:65532
EXPOSE 8081
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
  CMD ["/app/auth", "readycheck"]

ENTRYPOINT ["/app/auth"]
CMD ["-f", "etc/auth.yaml"]
