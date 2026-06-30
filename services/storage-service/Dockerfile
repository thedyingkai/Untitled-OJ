FROM golang:1.26.3

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

WORKDIR /app/storage-service
RUN go mod download

WORKDIR /app
COPY services/storage-service ./storage-service

WORKDIR /app/storage-service
RUN go build -o storage-service .
RUN mkdir -p /data/ojos/storage

CMD ["./storage-service", "-f", "etc/storageservice.yaml"]
