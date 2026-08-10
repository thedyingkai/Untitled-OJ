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

COPY services/judge-api/go.mod services/judge-api/go.sum ./judge-api/
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/

WORKDIR /app/judge-api
RUN go mod download

WORKDIR /app

COPY services/judge-api ./judge-api
COPY platform/shared/go /platform/shared/go

WORKDIR /app/judge-api

RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/judge-api .

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=build /out/judge-api ./judge-api
COPY --from=build /app/judge-api/etc ./etc

HEALTHCHECK --interval=2s --timeout=5s --start-period=5s --retries=30 CMD ["./judge-api", "healthcheck"]

CMD ["./judge-api", "-f", "etc/judgeapi.yaml"]
