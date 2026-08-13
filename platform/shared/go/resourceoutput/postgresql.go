// Package resourceoutput reads Agent-materialized resource outputs without
// exposing credentials to the control plane or application configuration.
package resourceoutput

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	"strings"
)

const maxPostgreSQLOutputBytes = 64 << 10

type postgreSQLOutput struct {
	DSN string `json:"dsn"`
}

// ReadPostgreSQLDSN accepts the v1 JSON resource output and the legacy raw-DSN
// representation. Errors deliberately omit the file contents and parsed URL so
// credentials cannot reach logs through wrapped startup failures.
func ReadPostgreSQLDSN(path string) (string, error) {
	path = strings.TrimSpace(path)
	if path == "" {
		return "", errors.New("PostgreSQL resource output path is required")
	}
	file, err := os.Open(path)
	if err != nil {
		return "", fmt.Errorf("open PostgreSQL resource output: %w", err)
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil || !info.Mode().IsRegular() || info.Size() < 1 || info.Size() > maxPostgreSQLOutputBytes {
		return "", errors.New("PostgreSQL resource output must be a bounded regular file")
	}
	contents, err := io.ReadAll(io.LimitReader(file, maxPostgreSQLOutputBytes+1))
	if err != nil {
		return "", errors.New("read PostgreSQL resource output")
	}
	trimmed := strings.TrimSpace(string(contents))
	var dsn string
	if strings.HasPrefix(trimmed, "{") {
		decoder := json.NewDecoder(strings.NewReader(trimmed))
		decoder.DisallowUnknownFields()
		var output postgreSQLOutput
		if err := decoder.Decode(&output); err != nil {
			return "", errors.New("decode PostgreSQL resource output")
		}
		var trailer any
		if err := decoder.Decode(&trailer); !errors.Is(err, io.EOF) {
			return "", errors.New("PostgreSQL resource output contains trailing data")
		}
		dsn = strings.TrimSpace(output.DSN)
	} else {
		dsn = trimmed
	}
	parsed, err := url.Parse(dsn)
	if err != nil || (parsed.Scheme != "postgres" && parsed.Scheme != "postgresql") ||
		parsed.Host == "" || parsed.User == nil || parsed.User.Username() == "" || parsed.Fragment != "" {
		return "", errors.New("PostgreSQL resource output contains an invalid DSN")
	}
	if _, passwordSet := parsed.User.Password(); !passwordSet {
		return "", errors.New("PostgreSQL resource output DSN must contain a credential")
	}
	return parsed.String(), nil
}
