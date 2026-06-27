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

COPY auth/go.mod auth/go.sum ./auth/
COPY shared/go.mod shared/go.sum ./shared/

WORKDIR /app/auth
RUN go mod download

WORKDIR /app

COPY auth ./auth
COPY shared ./shared

WORKDIR /app/auth

RUN go build -o auth .

CMD ["./auth", "-f", "etc/auth.yaml"]
