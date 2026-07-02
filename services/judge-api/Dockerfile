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

COPY services/judge-api/go.mod services/judge-api/go.sum ./judge-api/
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/

WORKDIR /app/judge-api
RUN go mod download

WORKDIR /app

COPY services/judge-api ./judge-api
COPY platform/shared/go /platform/shared/go

WORKDIR /app/judge-api

RUN go build -o judge-api .

CMD ["./judge-api", "-f", "etc/judgeapi.yaml"]
