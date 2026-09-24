// Bounded workload identity key loading and verifier construction.
package svc

import (
	"crypto/ed25519"
	"crypto/x509"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"ojos-shared/security/workload"
	"ojos-storage-service/internal/config"
)

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
