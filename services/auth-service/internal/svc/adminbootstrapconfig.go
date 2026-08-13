package svc

import (
	"bytes"
	"crypto/sha256"
	"crypto/subtle"
	"fmt"
	"os"
	"regexp"
	"strings"

	"ojos-auth-service/internal/config"
)

var adminBootstrapSecretPattern = regexp.MustCompile(`^[A-Za-z0-9_-]+$`)

const (
	minAdminBootstrapSecretBytes = 32
	maxAdminBootstrapSecretBytes = 512
)

// resolveAdminBootstrapSecret returns an in-memory copy of the configured
// one-time bootstrap secret. An empty configuration disables the route. The
// caller must not retain the clear-text secret in the service context.
func resolveAdminBootstrapSecret(c config.AdminBootstrapConfig) ([]byte, bool, error) {
	inline := strings.TrimSpace(c.Secret)
	path := strings.TrimSpace(c.SecretFile)
	if inline != "" && path != "" {
		return nil, false, fmt.Errorf("configure only one of AdminBootstrap.Secret or AdminBootstrap.SecretFile")
	}
	if inline == "" && path == "" {
		return nil, false, nil
	}

	var secret []byte
	if path != "" {
		info, err := os.Stat(path)
		if err != nil {
			return nil, false, fmt.Errorf("stat AdminBootstrap.SecretFile: %w", err)
		}
		if !info.Mode().IsRegular() {
			return nil, false, fmt.Errorf("AdminBootstrap.SecretFile must be a regular file")
		}
		if info.Size() > maxAdminBootstrapSecretBytes+2 {
			return nil, false, fmt.Errorf("AdminBootstrap.SecretFile is too large")
		}
		secret, err = os.ReadFile(path)
		if err != nil {
			return nil, false, fmt.Errorf("read AdminBootstrap.SecretFile: %w", err)
		}
		secret = bytes.TrimSpace(secret)
	} else {
		secret = []byte(inline)
	}

	if len(secret) < minAdminBootstrapSecretBytes || len(secret) > maxAdminBootstrapSecretBytes {
		return nil, false, fmt.Errorf(
			"admin bootstrap secret must contain between %d and %d bytes",
			minAdminBootstrapSecretBytes,
			maxAdminBootstrapSecretBytes,
		)
	}
	if !adminBootstrapSecretPattern.Match(secret) {
		return nil, false, fmt.Errorf("admin bootstrap secret must use only URL-safe token characters")
	}
	return append([]byte(nil), secret...), true, nil
}

func validateAdminBootstrapSecretSeparation(secret []byte, protected map[string]string) error {
	bootstrapDigest := sha256.Sum256(secret)
	for name, value := range protected {
		value = strings.TrimSpace(value)
		if value == "" {
			continue
		}
		protectedDigest := sha256.Sum256([]byte(value))
		if subtle.ConstantTimeCompare(bootstrapDigest[:], protectedDigest[:]) == 1 {
			return fmt.Errorf("admin bootstrap secret must not reuse %s", name)
		}
	}
	return nil
}
