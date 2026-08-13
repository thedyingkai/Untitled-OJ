// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"crypto/ed25519"
	"crypto/x509"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	sharedlogger "ojos-shared/logger"
	"ojos-shared/security/workload"
	"ojos-shared/tracing"
	"ojos-storage-service/internal/config"
	"ojos-storage-service/internal/store"

	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config              config.Config
	ObjectStore         store.ObjectStorage
	Logger              *zap.Logger
	Tracer              *sdktrace.TracerProvider
	WorkloadVerifier    *workload.Verifier
	WorkloadAuthEnabled bool
}

func NewServiceContext(c config.Config) *ServiceContext {
	result, err := BuildServiceContext(c)
	if err != nil {
		panic(err)
	}
	return result
}

func BuildServiceContext(c config.Config) (*ServiceContext, error) {
	ctx := context.Background()
	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		return nil, err
	}
	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		return nil, err
	}
	objectStore, err := store.NewObjectStorage(store.Options{
		Backend: c.Storage.Backend,
		Root:    c.Storage.Root,
		Buckets: c.Storage.Buckets,
		MinIO: store.MinIOOptions{
			Endpoint:  c.Storage.MinIO.Endpoint,
			AccessKey: c.Storage.MinIO.AccessKey,
			SecretKey: c.Storage.MinIO.SecretKey,
			UseSSL:    c.Storage.MinIO.UseSSL,
		},
	})
	if err != nil {
		return nil, err
	}
	workloadVerifier, err := workloadVerifier(c.WorkloadIdentity)
	if err != nil {
		return nil, err
	}
	workloadAuthEnabled := config.ProductionEnvironment()
	if workloadAuthEnabled && workloadVerifier == nil {
		return nil, errors.New("production storage requires a workload identity verifier")
	}
	if config.ManagedEnvironment() && strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE")) == "" {
		return nil, errors.New("managed storage requires an Agent-materialized service context path")
	}
	return &ServiceContext{
		Config:              c,
		ObjectStore:         objectStore,
		Logger:              zlog,
		Tracer:              tp,
		WorkloadVerifier:    workloadVerifier,
		WorkloadAuthEnabled: workloadAuthEnabled,
	}, nil
}

func workloadVerifier(c config.WorkloadIdentityConfig) (*workload.Verifier, error) {
	text := strings.TrimSpace(c.PublicKeyPEM)
	if path := strings.TrimSpace(c.PublicKeyFile); path != "" {
		if text != "" {
			return nil, errors.New("workload verifier must use exactly one public key source")
		}
		loaded, err := readWorkloadPublicKey(path)
		if err != nil {
			return nil, err
		}
		text = loaded
	}
	if text == "" {
		return nil, nil
	}
	block, rest := pem.Decode([]byte(text))
	if block == nil || block.Type != "PUBLIC KEY" || len(strings.TrimSpace(string(rest))) != 0 {
		return nil, errors.New("workload public key is not PEM")
	}
	parsed, err := x509.ParsePKIXPublicKey(block.Bytes)
	if err != nil {
		return nil, errors.New("parse workload public key")
	}
	key, ok := parsed.(ed25519.PublicKey)
	if !ok {
		return nil, errors.New("workload public key is not Ed25519")
	}
	return workload.NewVerifier(key, c.KeyID, c.Issuer, c.Audience)
}

const maximumWorkloadPublicKeyBytes int64 = 16 * 1024

func readWorkloadPublicKey(path string) (string, error) {
	if !filepath.IsAbs(path) || filepath.Clean(path) != path {
		return "", errors.New("workload public key file must be an absolute canonical path")
	}
	file, err := os.Open(path)
	if err != nil {
		return "", fmt.Errorf("open workload public key: %w", err)
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return "", fmt.Errorf("stat workload public key: %w", err)
	}
	if !info.Mode().IsRegular() || info.Size() <= 0 || info.Size() > maximumWorkloadPublicKeyBytes {
		return "", errors.New("workload public key must be a non-empty regular file no larger than 16 KiB")
	}
	bytes, err := io.ReadAll(io.LimitReader(file, maximumWorkloadPublicKeyBytes+1))
	if err != nil || int64(len(bytes)) > maximumWorkloadPublicKeyBytes {
		return "", errors.New("read bounded workload public key")
	}
	return string(bytes), nil
}

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.ObjectStore == nil {
		return errors.New("object storage backend is unavailable")
	}
	return s.ObjectStore.Ready(ctx)
}

func (s *ServiceContext) Close(ctx context.Context) {
	if s == nil {
		return
	}
	if s.Tracer != nil {
		_ = s.Tracer.Shutdown(ctx)
	}
	if s.Logger != nil {
		_ = s.Logger.Sync()
	}
}
