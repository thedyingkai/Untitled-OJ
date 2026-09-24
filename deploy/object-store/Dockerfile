FROM golang:1.26.6 AS build
WORKDIR /app/storage-service
COPY services/storage-service/go.mod services/storage-service/go.sum ./
COPY platform/shared/go/go.mod platform/shared/go/go.sum /platform/shared/go/
RUN go mod download
COPY services/storage-service/ ./
COPY platform/shared/go/ /platform/shared/go/
RUN CGO_ENABLED=0 go build -trimpath -o /out/ojos-object-store ./cmd/ojos-object-store

# SeaweedFS 4.47, upstream multi-platform manifest resolved 2026-09-24.
FROM chrislusf/seaweedfs:4.47@sha256:ce9e796f1fe6f06968f4c04bdaf8f678dad9c8acdfef3d244133d71bfa6bf882
COPY --from=build /out/ojos-object-store /usr/bin/ojos-object-store
USER seaweed
ENV GODEBUG=fips140=on
ENTRYPOINT ["ojos-object-store", "serve"]
CMD ["server", "-dir=/data", "-ip=127.0.0.1", "-ip.bind=127.0.0.1", "-master.volumeSizeLimitMB=1024", "-volume.max=0", "-volume.preStopSeconds=1", "-filer", "-filer.exposeDirectoryData=false", "-s3", "-s3.ip.bind=127.0.0.1", "-s3.port=8334", "-s3.config=/tmp/ojos-s3.json", "-s3.iam=false", "-s3.port.iceberg=0", "-s3.port.lance=0", "-s3.autoCreateBucket=false", "-s3.allowDeleteBucketNotEmpty=false"]
