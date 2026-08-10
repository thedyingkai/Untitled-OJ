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

COPY services/user-service/go.mod services/user-service/go.sum ./user-service/
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/

WORKDIR /app/user-service
RUN go mod download

WORKDIR /app
COPY services/user-service ./user-service
COPY platform/shared/go /platform/shared/go

WORKDIR /app/user-service
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/user-service .

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=build /out/user-service ./user-service
COPY --from=build /app/user-service/etc ./etc

RUN mkdir -p /data/ojos/users

CMD ["./user-service", "-f", "etc/userservice.yaml"]
