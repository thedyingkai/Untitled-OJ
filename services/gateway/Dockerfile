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

COPY gateway/go.mod gateway/go.sum ./gateway/
COPY shared/go.mod shared/go.sum ./shared/

WORKDIR /app/gateway
RUN go mod download

WORKDIR /app

COPY gateway ./gateway
COPY shared ./shared

WORKDIR /app/gateway

RUN go build -o gateway .

CMD ["./gateway", "-f", "etc/gateway.yaml"]
