FROM golang:1.26.6 AS build
WORKDIR /src
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/
COPY services/contest-service/go.mod services/contest-service/go.sum ./
COPY services/contest-service/gen/go ./gen/go
RUN go mod download
COPY platform/shared/go /platform/shared/go
COPY services/contest-service ./
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -buildvcs=false -ldflags="-s -w" -o /out/contest-service .

FROM alpine:3.22
RUN addgroup -S -g 65532 ojos && adduser -S -D -H -u 65532 -G ojos ojos
WORKDIR /app
COPY --from=build --chown=65532:65532 /out/contest-service /app/contest-service
USER 65532:65532
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
  CMD ["/app/contest-service", "readycheck"]
ENTRYPOINT ["/app/contest-service"]
