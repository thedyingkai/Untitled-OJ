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

COPY services/judge-api/go.mod services/judge-api/go.sum ./judge-api/
COPY services/problem-service/gen/go/events ./problem-service/gen/go/events
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/

WORKDIR /app/judge-api
RUN go mod download

WORKDIR /app

COPY services/judge-api ./judge-api
COPY platform/shared/go /platform/shared/go

WORKDIR /app/judge-api

RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -buildvcs=false -ldflags="-s -w" -o /out/judge-api .

FROM alpine:3.22 AS runtime

RUN apk add --no-cache ca-certificates \
    && addgroup -S -g 65532 ojos \
    && adduser -S -D -H -u 65532 -G ojos ojos

WORKDIR /app

COPY --from=build --chown=65532:65532 --chmod=0555 /out/judge-api ./judge-api
COPY --from=build --chown=65532:65532 --chmod=0444 /app/judge-api/etc/judgeapi.yaml ./etc/judgeapi.yaml

USER 65532:65532
EXPOSE 8082
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 CMD ["/app/judge-api", "healthcheck"]

ENTRYPOINT ["/app/judge-api"]
CMD ["-f", "etc/judgeapi.yaml"]
