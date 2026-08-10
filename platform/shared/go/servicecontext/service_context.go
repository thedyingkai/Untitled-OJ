// Package servicecontext implements the managed workload side of Service
// Contract v2. Applications address APIs by requirement name; the concrete
// provider, route and credential are materialized by the Orchestrator Agent.
package servicecontext

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
)

const (
	DefaultFile        = "/run/ojos/service/context.json"
	maxContextBytes    = 1 << 20
	maxCredentialBytes = 16 << 10
)

type ServiceContext struct {
	SchemaVersion  int                   `json:"schema_version"`
	Deployment     DeploymentIdentity    `json:"deployment"`
	Gateway        GatewayContext        `json:"gateway"`
	Bindings       map[string]APIBinding `json:"bindings"`
	CredentialFile string                `json:"credential_file"`
	Generation     uint64                `json:"generation"`
}

type DeploymentIdentity struct {
	ID      string `json:"id"`
	Service string `json:"service"`
	Node    string `json:"node"`
}

type GatewayContext struct {
	Origin string `json:"origin"`
	CAFile string `json:"ca_file,omitempty"`
}

type APIBinding struct {
	BindingID string `json:"binding_id"`
	APIID     string `json:"api_id"`
	BasePath  string `json:"base_path"`
	TimeoutMS uint64 `json:"timeout_ms"`
}

type RequestOptions struct {
	Headers       http.Header
	ContentLength int64
}

// LoadOptional returns nil when an unmanaged development process has no
// context. Managed workloads fail closed when the file is absent.
func LoadOptional() (*ServiceContext, error) {
	explicit := strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE"))
	path := explicit
	if path == "" {
		path = DefaultFile
	}
	_, err := os.Stat(path)
	if errors.Is(err, os.ErrNotExist) {
		if explicit != "" || envBool("OJOS_MANAGED_WORKLOAD") {
			return nil, fmt.Errorf("service context file is required but missing: %s", path)
		}
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("inspect service context: %w", err)
	}
	value, err := Load(path)
	if err != nil {
		return nil, err
	}
	return &value, nil
}

func Load(path string) (ServiceContext, error) {
	info, err := os.Stat(path)
	if err != nil {
		return ServiceContext{}, fmt.Errorf("inspect service context: %w", err)
	}
	if !info.Mode().IsRegular() || info.Size() <= 0 || info.Size() > maxContextBytes {
		return ServiceContext{}, errors.New("service context must be a bounded regular file")
	}
	file, err := os.Open(path)
	if err != nil {
		return ServiceContext{}, fmt.Errorf("open service context: %w", err)
	}
	defer file.Close()
	decoder := json.NewDecoder(io.LimitReader(file, maxContextBytes+1))
	decoder.DisallowUnknownFields()
	var value ServiceContext
	if err := decoder.Decode(&value); err != nil {
		return ServiceContext{}, fmt.Errorf("decode service context: %w", err)
	}
	if err := ensureJSONEOF(decoder); err != nil {
		return ServiceContext{}, err
	}
	if err := value.Validate(); err != nil {
		return ServiceContext{}, err
	}
	return value, nil
}

func (value ServiceContext) Validate() error {
	if value.SchemaVersion != 1 {
		return fmt.Errorf("unsupported service context schema version %d", value.SchemaVersion)
	}
	for label, field := range map[string]string{
		"deployment.id": value.Deployment.ID, "deployment.service": value.Deployment.Service,
		"deployment.node": value.Deployment.Node,
	} {
		if strings.TrimSpace(field) == "" {
			return fmt.Errorf("service context %s is required", label)
		}
	}
	origin, err := url.Parse(value.Gateway.Origin)
	if err != nil || origin.Host == "" || origin.User != nil || origin.Path != "" || origin.RawQuery != "" || origin.Fragment != "" {
		return errors.New("gateway origin must be an origin without path, query or fragment")
	}
	if origin.Scheme != "https" && !developmentHTTPAllowed(origin) {
		return errors.New("gateway origin must use https outside explicit development mode")
	}
	if !filepath.IsAbs(value.CredentialFile) {
		return errors.New("service context credential_file must be absolute")
	}
	if value.Gateway.CAFile != "" && !filepath.IsAbs(value.Gateway.CAFile) {
		return errors.New("service context gateway.ca_file must be absolute")
	}
	for name, binding := range value.Bindings {
		if strings.TrimSpace(name) == "" || strings.TrimSpace(binding.BindingID) == "" ||
			strings.TrimSpace(binding.APIID) == "" || binding.TimeoutMS == 0 || binding.TimeoutMS > 300_000 {
			return fmt.Errorf("service context binding %q is incomplete", name)
		}
		expected := "/internal/apis/" + binding.APIID
		if strings.TrimSuffix(binding.BasePath, "/") != expected {
			return fmt.Errorf("binding %s base_path must be %s, got %s", name, expected, binding.BasePath)
		}
	}
	return nil
}

func (value ServiceContext) RequireService(expected string) error {
	if value.Deployment.Service != expected {
		return fmt.Errorf("service context belongs to %s, expected %s", value.Deployment.Service, expected)
	}
	return nil
}

func (value ServiceContext) Binding(name string) (APIBinding, error) {
	binding, ok := value.Bindings[name]
	if !ok {
		return APIBinding{}, fmt.Errorf("required API binding %q is missing", name)
	}
	return binding, nil
}

func (value ServiceContext) BindingURL(name, relativePath string) (string, error) {
	binding, err := value.Binding(name)
	if err != nil {
		return "", err
	}
	relative, err := parseRelativePath(relativePath)
	if err != nil {
		return "", err
	}
	result := strings.TrimSuffix(value.Gateway.Origin, "/") + strings.TrimSuffix(binding.BasePath, "/") + relative.EscapedPath()
	if relative.RawQuery != "" {
		result += "?" + relative.RawQuery
	}
	return result, nil
}

// Client returns a redirect-disabled client pinned to the Gateway CA. Proxy
// environment variables are deliberately ignored for managed service traffic.
func (value ServiceContext) Client() (*http.Client, error) {
	timeout := 35 * time.Second
	for _, binding := range value.Bindings {
		candidate := time.Duration(binding.TimeoutMS) * time.Millisecond
		if candidate > timeout {
			timeout = candidate
		}
	}
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.Proxy = nil
	if value.Gateway.CAFile != "" {
		roots, err := x509.SystemCertPool()
		if err != nil || roots == nil {
			roots = x509.NewCertPool()
		}
		pem, err := os.ReadFile(value.Gateway.CAFile)
		if err != nil {
			return nil, fmt.Errorf("read gateway CA: %w", err)
		}
		if !roots.AppendCertsFromPEM(pem) {
			return nil, errors.New("parse gateway CA: no certificate found")
		}
		transport.TLSClientConfig = &tls.Config{MinVersion: tls.VersionTLS12, RootCAs: roots}
	}
	return &http.Client{
		Transport: transport,
		Timeout:   timeout,
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}, nil
}

// NewRequest reloads the workload token on every call, so Agent rotations do
// not require a container restart.
func (value ServiceContext) NewRequest(ctx context.Context, bindingName, method, relativePath string, body io.Reader) (*http.Request, error) {
	return value.NewRequestWithOptions(ctx, bindingName, method, relativePath, body, RequestOptions{ContentLength: -1})
}

func (value ServiceContext) NewRequestWithOptions(ctx context.Context, bindingName, method, relativePath string, body io.Reader, options RequestOptions) (*http.Request, error) {
	if _, err := value.Binding(bindingName); err != nil {
		return nil, err
	}
	token, err := readCredential(value.CredentialFile)
	if err != nil {
		return nil, err
	}
	target, err := value.BindingURL(bindingName, relativePath)
	if err != nil {
		return nil, err
	}
	request, err := http.NewRequestWithContext(ctx, method, target, body)
	if err != nil {
		return nil, err
	}
	request.Header.Set("Authorization", "Bearer "+token)
	otel.GetTextMapPropagator().Inject(ctx, propagation.HeaderCarrier(request.Header))
	for name, values := range options.Headers {
		for _, headerValue := range values {
			request.Header.Add(name, headerValue)
		}
	}
	// The SDK-owned workload credential cannot be replaced by application
	// headers. Caller identity always comes from the Agent-materialized token.
	request.Header.Set("Authorization", "Bearer "+token)
	if options.ContentLength >= 0 {
		request.ContentLength = options.ContentLength
	}
	if isMutation(method) {
		if request.Header.Get("Idempotency-Key") == "" {
			request.Header.Set("Idempotency-Key", randomID())
		}
	}
	return request, nil
}

// Do enforces the selected binding's timeout and releases its deadline timer
// when the response body is closed. Callers should prefer Do over calling the
// underlying http.Client directly.
func (value ServiceContext) Do(ctx context.Context, client *http.Client, bindingName, method, relativePath string, body io.Reader) (*http.Response, error) {
	return value.DoWithOptions(ctx, client, bindingName, method, relativePath, body, RequestOptions{ContentLength: -1})
}

func (value ServiceContext) DoWithOptions(ctx context.Context, client *http.Client, bindingName, method, relativePath string, body io.Reader, options RequestOptions) (*http.Response, error) {
	binding, err := value.Binding(bindingName)
	if err != nil {
		return nil, err
	}
	requestCtx, cancel := context.WithTimeout(ctx, time.Duration(binding.TimeoutMS)*time.Millisecond)
	request, err := value.NewRequestWithOptions(requestCtx, bindingName, method, relativePath, body, options)
	if err != nil {
		cancel()
		return nil, err
	}
	response, err := client.Do(request)
	if err != nil {
		cancel()
		return nil, err
	}
	response.Body = &cancelOnClose{ReadCloser: response.Body, cancel: cancel}
	return response, nil
}

func (value ServiceContext) DownloadTo(ctx context.Context, client *http.Client, bindingName, relativePath, expectedSHA256 string, expectedSize uint64, target string) error {
	expected, err := normalizedSHA256(expectedSHA256)
	if err != nil || expectedSize == 0 {
		return errors.New("download artifact identity is invalid")
	}
	response, err := value.Do(ctx, client, bindingName, http.MethodGet, relativePath, nil)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return fmt.Errorf("bound download returned %s", response.Status)
	}
	if response.ContentLength >= 0 && uint64(response.ContentLength) != expectedSize {
		return errors.New("bound download size does not match resource reference")
	}
	parent := filepath.Dir(target)
	if err := os.MkdirAll(parent, 0o750); err != nil {
		return err
	}
	temporary, err := os.CreateTemp(parent, ".ojos-download-*.tmp")
	if err != nil {
		return err
	}
	temporaryPath := temporary.Name()
	defer func() { _ = os.Remove(temporaryPath) }()
	if err := temporary.Chmod(0o600); err != nil {
		_ = temporary.Close()
		return err
	}
	hasher := sha256.New()
	written, copyErr := io.Copy(io.MultiWriter(temporary, hasher), io.LimitReader(response.Body, int64(expectedSize)+1))
	if copyErr == nil {
		copyErr = temporary.Sync()
	}
	closeErr := temporary.Close()
	if copyErr != nil {
		return copyErr
	}
	if closeErr != nil {
		return closeErr
	}
	if written != int64(expectedSize) {
		return errors.New("bound download size does not match resource reference")
	}
	if hex.EncodeToString(hasher.Sum(nil)) != expected {
		return errors.New("bound download SHA-256 does not match resource reference")
	}
	if _, err := os.Stat(target); err == nil {
		return fmt.Errorf("download target already exists: %s", target)
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if err := os.Rename(temporaryPath, target); err != nil {
		return err
	}
	return nil
}

type cancelOnClose struct {
	io.ReadCloser
	cancel context.CancelFunc
}

func (body *cancelOnClose) Close() error {
	err := body.ReadCloser.Close()
	body.cancel()
	return err
}

func readCredential(path string) (string, error) {
	info, err := os.Stat(path)
	if err != nil {
		return "", fmt.Errorf("inspect workload credential: %w", err)
	}
	if !info.Mode().IsRegular() || info.Size() <= 0 || info.Size() > maxCredentialBytes {
		return "", errors.New("workload credential file is invalid")
	}
	bytes, err := os.ReadFile(path)
	if err != nil {
		return "", fmt.Errorf("read workload credential: %w", err)
	}
	token := strings.TrimSpace(string(bytes))
	if token == "" || strings.IndexFunc(token, func(r rune) bool { return r == ' ' || r == '\t' || r == '\r' || r == '\n' }) >= 0 {
		return "", errors.New("workload credential is invalid")
	}
	return token, nil
}

func parseRelativePath(value string) (*url.URL, error) {
	trimmed := strings.TrimSpace(value)
	if trimmed == "" {
		return &url.URL{}, nil
	}
	parsed, err := url.ParseRequestURI(trimmed)
	if err != nil || parsed.IsAbs() || parsed.Host != "" || strings.HasPrefix(trimmed, "//") || parsed.Fragment != "" {
		return nil, errors.New("binding path must be relative to the selected API")
	}
	for _, segment := range strings.Split(parsed.Path, "/") {
		if segment == ".." || segment == "." {
			return nil, errors.New("binding path must not contain dot segments")
		}
	}
	if !strings.HasPrefix(parsed.Path, "/") {
		parsed.Path = "/" + parsed.Path
	}
	return parsed, nil
}

func normalizedSHA256(value string) (string, error) {
	value = strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(value), "sha256:"))
	if len(value) != 64 || strings.ToLower(value) != value {
		return "", errors.New("SHA-256 must be 64 lowercase hexadecimal characters")
	}
	if _, err := hex.DecodeString(value); err != nil {
		return "", err
	}
	return value, nil
}

func isMutation(method string) bool {
	switch strings.ToUpper(method) {
	case http.MethodPost, http.MethodPut, http.MethodPatch, http.MethodDelete:
		return true
	default:
		return false
	}
}

func randomID() string {
	var value [16]byte
	if _, err := rand.Read(value[:]); err != nil {
		return fmt.Sprintf("fallback-%d", time.Now().UnixNano())
	}
	value[6] = (value[6] & 0x0f) | 0x40
	value[8] = (value[8] & 0x3f) | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", value[0:4], value[4:6], value[6:8], value[8:10], value[10:16])
}

func ensureJSONEOF(decoder *json.Decoder) error {
	var extra any
	if err := decoder.Decode(&extra); !errors.Is(err, io.EOF) {
		if err == nil {
			return errors.New("service context contains multiple JSON values")
		}
		return fmt.Errorf("decode service context trailer: %w", err)
	}
	return nil
}

func envBool(name string) bool {
	value := strings.TrimSpace(os.Getenv(name))
	return value == "1" || strings.EqualFold(value, "true")
}

func developmentHTTPAllowed(origin *url.URL) bool {
	if origin.Scheme != "http" {
		return false
	}
	host := origin.Hostname()
	return envBool("OJOS_ALLOW_HTTP_SERVICE_CONTEXT") || host == "127.0.0.1" || host == "localhost" || host == "::1"
}
