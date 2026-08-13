FROM golang:1.26.5 AS build
WORKDIR /src
COPY platform/shared/go /platform/shared/go
COPY services/auth-service/go.mod services/auth-service/go.sum ./
COPY services/auth-service/migrations/cmd/ojos-migrate ./migrations/cmd/ojos-migrate
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -buildvcs=false -ldflags="-s -w" -o /out/ojos-migrate ./migrations/cmd/ojos-migrate

FROM alpine:3.22
RUN addgroup -S -g 65532 ojos && adduser -S -D -H -u 65532 -G ojos ojos
COPY --from=build --chmod=0555 /out/ojos-migrate /ojos-migrate
COPY --chown=65532:65532 --chmod=0444 services/auth-service/migrations/*.sql /migrations/
USER 65532:65532
ENTRYPOINT ["/ojos-migrate"]
CMD ["apply", "/migrations"]
