FROM golang:1.26.5 AS build
WORKDIR /src
COPY platform/shared/go /platform/shared/go
COPY services/contest-service/go.mod services/contest-service/go.sum ./
COPY services/contest-service/gen/go ./gen/go
COPY services/contest-service/internal/config ./internal/config
COPY services/contest-service/migrations/cmd/ojos-migrate ./migrations/cmd/ojos-migrate
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -buildvcs=false -ldflags="-s -w" -o /out/ojos-migrate ./migrations/cmd/ojos-migrate

FROM alpine:3.22
RUN addgroup -S -g 65532 ojos && adduser -S -D -H -u 65532 -G ojos ojos
COPY --from=build --chmod=0555 /out/ojos-migrate /ojos-migrate
COPY --chown=65532:65532 --chmod=0444 services/contest-service/migrations/*.sql /migrations/
USER 65532:65532
ENTRYPOINT ["/ojos-migrate"]
CMD ["apply", "/migrations"]
