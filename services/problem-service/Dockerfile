FROM golang:1.26.4

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

RUN go build -o problem-service .

RUN mkdir -p /data/ojos/problems

CMD ["./problem-service", "-f", "etc/problemapi.yaml"]
