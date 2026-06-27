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

COPY problem-api/go.mod problem-api/go.sum ./problem-api/
COPY shared/go.mod shared/go.sum ./shared/

WORKDIR /app/problem-api
RUN go mod download

WORKDIR /app

COPY problem-api ./problem-api
COPY shared ./shared

WORKDIR /app/problem-api

RUN go build -o problem-api .

RUN mkdir -p /data/ojos/problems

CMD ["./problem-api", "-f", "etc/problemapi.yaml"]
