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

COPY services/gateway/go.mod services/gateway/go.sum ./gateway/
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/

WORKDIR /app/gateway
RUN go mod download

WORKDIR /app

COPY services/gateway ./gateway
COPY platform/shared/go /platform/shared/go

WORKDIR /app/gateway

RUN go build -o gateway .

CMD ["./gateway", "-f", "etc/gateway.yaml"]
