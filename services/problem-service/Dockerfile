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

RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -buildvcs=false -ldflags="-s -w" -o /out/problem-service .

FROM alpine:3.22 AS runtime

RUN addgroup -S -g 65532 ojos && adduser -S -D -H -u 65532 -G ojos ojos

WORKDIR /app

COPY --from=build --chown=65532:65532 --chmod=0555 /out/problem-service ./problem-service
COPY --from=build --chown=65532:65532 --chmod=0444 /app/problem-service/etc ./etc

RUN mkdir -p /data/ojos/problems && chown -R 65532:65532 /data/ojos

USER 65532:65532
EXPOSE 8083
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 CMD ["/app/problem-service", "healthcheck"]

ENTRYPOINT ["/app/problem-service"]
CMD ["-f", "etc/problemapi.yaml"]
