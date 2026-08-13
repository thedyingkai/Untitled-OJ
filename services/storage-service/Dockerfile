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

FROM gcr.io/distroless/static-debian12:nonroot AS runtime

WORKDIR /app

COPY --from=build --chown=65532:65532 /out/storage-service ./storage-service
COPY --from=build --chown=65532:65532 /app/storage-service/etc ./etc

USER 65532:65532

HEALTHCHECK --interval=2s --timeout=5s --start-period=5s --retries=30 CMD ["./storage-service", "healthcheck"]

CMD ["./storage-service", "-f", "etc/storageservice.yaml"]
