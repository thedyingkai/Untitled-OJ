use crate::{ApiOperationV3, EventContractV1, ServiceContractV3, contract_bytes};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

pub const CODEGEN_REPORT_SCHEMA_VERSION: &str = "ojos.dev/codegen-report/v1";
pub const CODEGEN_REPORT_FILE: &str = ".ojos-codegen.json";

#[derive(Debug, Error)]
pub enum CodegenError {
    #[error("generated identifier collision for {language}: {identifier}")]
    IdentifierCollision {
        language: &'static str,
        identifier: String,
    },
    #[error("generated path is not a safe relative path: {0}")]
    UnsafePath(PathBuf),
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("generated output is not sealed: {0}")]
    Drift(String),
    #[error("serialize code generation report: {0}")]
    Serialize(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CodegenError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerationReport {
    pub schema_version: String,
    pub service_id: String,
    pub service_version: String,
    pub files: Vec<GeneratedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneratedFile {
    pub path: String,
    pub digest: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationReport {
    pub schema_version: String,
    pub service_id: String,
    pub service_version: String,
    pub files: Vec<GeneratedFile>,
}

/// Renders all compiler-owned files without touching the filesystem.
///
/// Paths are relative to the service's `gen/` directory. A `BTreeMap` and
/// sorted contract inputs make the result byte-for-byte deterministic.
pub fn render(contract: &ServiceContractV3) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    validate_event_payload_schemas(contract)?;
    validate_identifiers(contract)?;
    let mut files = BTreeMap::new();

    files.insert(
        PathBuf::from("service.contract.json"),
        contract_bytes(contract).map_err(|error| CodegenError::Drift(error.to_string()))?,
    );

    files.insert(
        PathBuf::from("go/go.mod"),
        render_go_mod(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("go/client.go"),
        render_go_client(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("go/client_test.go"),
        render_go_client_test(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("go/events.go"),
        render_go_events(contract)?.into_bytes(),
    );
    files.insert(
        PathBuf::from("go/events_test.go"),
        render_go_events_test(contract)?.into_bytes(),
    );

    files.insert(
        PathBuf::from("rust/Cargo.toml"),
        render_rust_manifest(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("rust/src/lib.rs"),
        render_rust_lib(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("rust/src/client.rs"),
        render_rust_client(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("rust/src/events.rs"),
        render_rust_events(contract)?.into_bytes(),
    );

    files.insert(
        PathBuf::from("ts/package.json"),
        render_ts_package(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("ts/tsconfig.json"),
        render_ts_config().into_bytes(),
    );
    files.insert(
        PathBuf::from("ts/src/index.ts"),
        b"export * from './client.js';\nexport * from './events.js';\n".to_vec(),
    );
    files.insert(
        PathBuf::from("ts/src/client.ts"),
        render_ts_client(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("ts/src/client.test.ts"),
        render_ts_client_test(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("ts/src/events.ts"),
        render_ts_events(contract)?.into_bytes(),
    );
    files.insert(
        PathBuf::from("ts/src/events.test.ts"),
        render_ts_events_test(contract)?.into_bytes(),
    );

    files.insert(
        PathBuf::from("gozero/service.api"),
        render_gozero_api(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("gozero/server-adapter.json"),
        render_server_adapter(contract)?,
    );
    Ok(files)
}

/// Writes generated files below `output_root` and returns the stable report.
///
/// Files listed by the previous report but no longer generated are removed;
/// unrelated files are never deleted.
pub fn generate_to(contract: &ServiceContractV3, output_root: &Path) -> Result<GenerationReport> {
    let rendered = render(contract)?;
    fs::create_dir_all(output_root).map_err(|source| CodegenError::Write {
        path: output_root.to_path_buf(),
        source,
    })?;

    let report_path = output_root.join(CODEGEN_REPORT_FILE);
    remove_stale_files(output_root, &report_path, rendered.keys())?;

    for (relative, bytes) in &rendered {
        ensure_safe_relative(relative)?;
        let destination = output_root.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|source| CodegenError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&destination, bytes).map_err(|source| CodegenError::Write {
            path: destination,
            source,
        })?;
    }

    let report = report_for(contract, &rendered);
    let mut report_bytes = serde_json::to_vec_pretty(&report)?;
    report_bytes.push(b'\n');
    fs::write(&report_path, report_bytes).map_err(|source| CodegenError::Write {
        path: report_path,
        source,
    })?;
    Ok(report)
}

/// Verifies compiler-owned output without mutating the filesystem.
///
/// CI uses this after compilation so a missing file, a hand edit, a stale
/// compiler-owned file, or a forged generation report fails before build or
/// publication. Unrelated files below `gen/` remain developer-owned.
pub fn verify_generated(
    contract: &ServiceContractV3,
    output_root: &Path,
) -> Result<VerificationReport> {
    let rendered = render(contract)?;
    let expected = report_for(contract, &rendered);
    let report_path = output_root.join(CODEGEN_REPORT_FILE);
    let report_bytes = fs::read(&report_path).map_err(|source| CodegenError::Read {
        path: report_path.clone(),
        source,
    })?;
    let recorded: GenerationReport = serde_json::from_slice(&report_bytes)?;
    if recorded != expected {
        return Err(CodegenError::Drift(format!(
            "{} does not match compiler output",
            report_path.display()
        )));
    }

    for (relative, expected_bytes) in &rendered {
        ensure_safe_relative(relative)?;
        let destination = output_root.join(relative);
        let actual = fs::read(&destination).map_err(|source| CodegenError::Read {
            path: destination.clone(),
            source,
        })?;
        if &actual != expected_bytes {
            return Err(CodegenError::Drift(format!(
                "{} differs from deterministic output",
                destination.display()
            )));
        }
    }

    Ok(VerificationReport {
        schema_version: "ojos.dev/codegen-verification/v1".to_string(),
        service_id: expected.service_id,
        service_version: expected.service_version,
        files: expected.files,
    })
}

pub fn report_for(
    contract: &ServiceContractV3,
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> GenerationReport {
    GenerationReport {
        schema_version: CODEGEN_REPORT_SCHEMA_VERSION.to_string(),
        service_id: contract.service_id.clone(),
        service_version: contract.service_version.to_string(),
        files: files
            .iter()
            .map(|(path, bytes)| GeneratedFile {
                path: slash_path(path),
                digest: digest(bytes),
                size: bytes.len() as u64,
            })
            .collect(),
    }
}

fn remove_stale_files<'a>(
    output_root: &Path,
    report_path: &Path,
    current: impl Iterator<Item = &'a PathBuf>,
) -> Result<()> {
    let current = current
        .map(|path| slash_path(path))
        .collect::<BTreeSet<_>>();
    let bytes = match fs::read(report_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(CodegenError::Read {
                path: report_path.to_path_buf(),
                source,
            });
        }
    };
    let previous: GenerationReport = serde_json::from_slice(&bytes)?;
    for file in previous.files {
        if current.contains(&file.path) {
            continue;
        }
        let relative = PathBuf::from(&file.path);
        ensure_safe_relative(&relative)?;
        let destination = output_root.join(relative);
        match fs::remove_file(&destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CodegenError::Write {
                    path: destination,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn ensure_safe_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(CodegenError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn validate_identifiers(contract: &ServiceContractV3) -> Result<()> {
    let mut go = BTreeSet::new();
    let mut rust = BTreeSet::new();
    let mut ts = BTreeSet::new();
    for operation in &contract.operations {
        insert_identifier(&mut go, go_exported(&operation.operation_id), "Go")?;
        insert_identifier(&mut rust, rust_const(&operation.operation_id), "Rust")?;
        insert_identifier(
            &mut ts,
            ts_identifier(&operation.operation_id),
            "TypeScript",
        )?;
    }
    for event in all_events(contract) {
        let identity = format!("{}V{}", event.event_type, event.version);
        insert_identifier(&mut go, go_exported(&identity), "Go event")?;
        insert_identifier(&mut rust, rust_const(&identity), "Rust event")?;
        insert_identifier(&mut ts, ts_type(&identity), "TypeScript event")?;
    }
    Ok(())
}

fn validate_event_payload_schemas(contract: &ServiceContractV3) -> Result<()> {
    for event in all_events(contract) {
        let canonical = serde_json_canonicalizer::to_vec(&event.payload_schema)
            .map_err(CodegenError::Serialize)?;
        if digest(&canonical) != event.schema.digest {
            return Err(CodegenError::Drift(format!(
                "event {} v{} payload schema does not match {}",
                event.event_type, event.version, event.schema.digest
            )));
        }
    }
    Ok(())
}

fn insert_identifier(
    seen: &mut BTreeSet<String>,
    identifier: String,
    language: &'static str,
) -> Result<()> {
    if !seen.insert(identifier.clone()) {
        return Err(CodegenError::IdentifierCollision {
            language,
            identifier,
        });
    }
    Ok(())
}

fn render_go_mod(contract: &ServiceContractV3) -> String {
    format!(
        "module ojos.local/gen/{}\n\ngo 1.23\n",
        go_package(&contract.service_id)
    )
}

fn render_go_client(contract: &ServiceContractV3) -> String {
    let package = go_package(&contract.service_id);
    let mut source = format!(
        r#"// Code generated by ojos service. DO NOT EDIT.
package {package}

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"time"
)

type ContextSnapshot struct {{
	Generation uint64
	BaseURL string
	Token string
}}

type ContextSnapshotProvider interface {{
	Current(context.Context) (ContextSnapshot, error)
}}

type ContextSnapshotProviderFunc func(context.Context) (ContextSnapshot, error)

func (provider ContextSnapshotProviderFunc) Current(ctx context.Context) (ContextSnapshot, error) {{
	return provider(ctx)
}}

type StaticContextProvider struct {{ Snapshot ContextSnapshot }}

func (provider StaticContextProvider) Current(context.Context) (ContextSnapshot, error) {{
	return provider.Snapshot, nil
}}

type Client struct {{
	Context ContextSnapshotProvider
	HTTPClient *http.Client
	Timeout time.Duration
}}

type Operation struct {{
	ID string
	Method string
	Path string
	Audience string
	Permission string
	HeaderParameters []HeaderParameter
	RequestContentTypes []string
	RequestBodyRequired bool
}}

type HeaderParameter struct {{
	Name string
	Required bool
}}

type CallOptions struct {{
	IdempotencyKey string
	Timeout time.Duration
	Headers http.Header
	ContentType string
	RawBody []byte
}}

type ErrorKind string

const (
	ErrorConfiguration ErrorKind = "configuration"
	ErrorTimeout ErrorKind = "timeout"
	ErrorTransport ErrorKind = "transport"
	ErrorUnauthorized ErrorKind = "unauthorized"
	ErrorForbidden ErrorKind = "forbidden"
	ErrorNotFound ErrorKind = "not_found"
	ErrorConflict ErrorKind = "conflict"
	ErrorRateLimited ErrorKind = "rate_limited"
	ErrorServer ErrorKind = "server"
)

type ClientError struct {{
	Kind ErrorKind
	StatusCode int
	Err error
}}

func (err *ClientError) Error() string {{
	if err.Err != nil {{ return fmt.Sprintf("%s: %v", err.Kind, err.Err) }}
	return fmt.Sprintf("%s (status %d)", err.Kind, err.StatusCode)
}}

func (err *ClientError) Unwrap() error {{ return err.Err }}

var Operations = map[string]Operation{{
"#
    );
    for operation in sorted_operations(contract) {
        source.push_str(&format!(
            "\t{}: {{ID: {}, Method: {}, Path: {}, Audience: {}, Permission: {}, HeaderParameters: {}, RequestContentTypes: {}, RequestBodyRequired: {}}},\n",
            go_string(&operation.operation_id),
            go_string(&operation.operation_id),
            go_string(&operation.method),
            go_string(&operation.provider_path),
            go_string(&operation.audience),
            go_string(operation.permission.as_deref().unwrap_or("")),
            go_header_parameters(operation),
            go_request_content_types(operation),
            operation
                .request_body
                .as_ref()
                .map(|body| body.required)
                .unwrap_or(false),
        ));
    }
    source.push_str(
        r#"}

func NewClient(provider ContextSnapshotProvider, httpClient *http.Client) *Client {
	if httpClient == nil { httpClient = http.DefaultClient }
	return &Client{Context: provider, HTTPClient: httpClient, Timeout: 30 * time.Second}
}

func expandPath(template string, parameters map[string]string) (string, error) {
	path := template
	for key, value := range parameters { path = strings.ReplaceAll(path, "{"+key+"}", url.PathEscape(value)) }
	if strings.Contains(path, "{") { return "", fmt.Errorf("missing path parameter for %s", template) }
	return path, nil
}

func (c *Client) Do(ctx context.Context, operation Operation, pathParameters map[string]string, query url.Values, body []byte) (*http.Response, error) {
	return c.DoWithOptions(ctx, operation, pathParameters, query, body, CallOptions{})
}

func (c *Client) DoWithOptions(ctx context.Context, operation Operation, pathParameters map[string]string, query url.Values, body []byte, options CallOptions) (*http.Response, error) {
	if c == nil || c.Context == nil { return nil, &ClientError{Kind: ErrorConfiguration, Err: errors.New("context snapshot provider is required")} }
	snapshot, err := c.Context.Current(ctx)
	if err != nil { return nil, &ClientError{Kind: ErrorConfiguration, Err: err} }
	baseURL := strings.TrimRight(strings.TrimSpace(snapshot.BaseURL), "/")
	if baseURL == "" { return nil, &ClientError{Kind: ErrorConfiguration, Err: errors.New("context snapshot base URL is required")} }
	path, err := expandPath(operation.Path, pathParameters)
	if err != nil { return nil, &ClientError{Kind: ErrorConfiguration, Err: err} }
	target, err := url.Parse(baseURL + path)
	if err != nil { return nil, &ClientError{Kind: ErrorConfiguration, Err: err} }
	target.RawQuery = query.Encode()
	timeout := options.Timeout
	if timeout <= 0 { timeout = c.Timeout }
	effectiveBody := body
	if options.RawBody != nil {
		if body != nil { return nil, &ClientError{Kind: ErrorConfiguration, Err: errors.New("body and raw body cannot both be set")} }
		effectiveBody = options.RawBody
	}
	if operation.RequestBodyRequired && effectiveBody == nil { return nil, &ClientError{Kind: ErrorConfiguration, Err: errors.New("request body is required")} }
	contentType, err := selectContentType(operation, options.ContentType, effectiveBody != nil)
	if err != nil { return nil, &ClientError{Kind: ErrorConfiguration, Err: err} }
	requestContext := ctx
	var cancel context.CancelFunc
	if timeout > 0 { requestContext, cancel = context.WithTimeout(ctx, timeout); defer cancel() }
	request, err := http.NewRequestWithContext(requestContext, operation.Method, target.String(), bytes.NewReader(effectiveBody))
	if err != nil { return nil, &ClientError{Kind: ErrorConfiguration, Err: err} }
	for name, values := range options.Headers {
		if !validHeaderName(name) { return nil, &ClientError{Kind: ErrorConfiguration, Err: fmt.Errorf("invalid header name %q", name)} }
		if forbiddenHeader(name) { return nil, &ClientError{Kind: ErrorConfiguration, Err: fmt.Errorf("header %s cannot be overridden", name)} }
		for _, value := range values {
			if !validHeaderValue(value) { return nil, &ClientError{Kind: ErrorConfiguration, Err: fmt.Errorf("invalid value for header %s", name)} }
			request.Header.Add(name, value)
		}
	}
	if contentType != "" { request.Header.Set("Content-Type", contentType) }
	if snapshot.Token != "" { request.Header.Set("Authorization", "Bearer "+snapshot.Token) }
	if key := strings.TrimSpace(options.IdempotencyKey); key != "" { request.Header.Set("Idempotency-Key", key) }
	for _, parameter := range operation.HeaderParameters {
		if parameter.Required && strings.TrimSpace(request.Header.Get(parameter.Name)) == "" { return nil, &ClientError{Kind: ErrorConfiguration, Err: fmt.Errorf("required header %s is missing", parameter.Name)} }
	}
	response, err := c.HTTPClient.Do(request)
	if err != nil {
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(requestContext.Err(), context.DeadlineExceeded) { return nil, &ClientError{Kind: ErrorTimeout, Err: err} }
		return nil, &ClientError{Kind: ErrorTransport, Err: err}
	}
	if kind := statusErrorKind(response.StatusCode); kind != "" { return response, &ClientError{Kind: kind, StatusCode: response.StatusCode} }
	return response, nil
}

func selectContentType(operation Operation, requested string, hasBody bool) (string, error) {
	requested = strings.TrimSpace(requested)
	if !hasBody {
		if requested != "" { return "", errors.New("content type cannot be set without a request body") }
		return "", nil
	}
	if requested == "" && len(operation.RequestContentTypes) != 0 { return operation.RequestContentTypes[0], nil }
	if requested == "" { return "", nil }
	for _, declared := range operation.RequestContentTypes { if strings.EqualFold(requested, declared) { return declared, nil } }
	return "", fmt.Errorf("content type %s is not declared for operation %s", requested, operation.ID)
}

func forbiddenHeader(name string) bool {
	name = strings.ToLower(strings.TrimSpace(name))
	if strings.HasPrefix(name, "proxy-") || strings.HasPrefix(name, "sec-") || strings.HasPrefix(name, "x-forwarded-") || strings.HasPrefix(name, "x-ojos-caller-") || strings.HasPrefix(name, "x-ojos-gateway-") || strings.HasPrefix(name, "x-ojos-internal-") || strings.HasPrefix(name, "x-ojos-workload-") { return true }
	switch name {
	case "authorization", "host", "cookie", "set-cookie", "connection", "keep-alive", "te", "trailer", "transfer-encoding", "upgrade", "content-length", "content-type", "idempotency-key", "forwarded", "via", "expect", "x-api-key", "api-key":
		return true
	default:
		return false
	}
}

func validHeaderName(name string) bool {
	if name == "" { return false }
	for _, character := range name {
		if (character >= 'a' && character <= 'z') || (character >= 'A' && character <= 'Z') || (character >= '0' && character <= '9') || strings.ContainsRune("!#$%&'*+-.^_`|~", character) { continue }
		return false
	}
	return true
}

func validHeaderValue(value string) bool {
	for _, character := range value { if character == '\r' || character == '\n' || character == 0x7f || (character < 0x20 && character != '\t') { return false } }
	return true
}

func statusErrorKind(status int) ErrorKind {
	switch status {
	case http.StatusUnauthorized: return ErrorUnauthorized
	case http.StatusForbidden: return ErrorForbidden
	case http.StatusNotFound: return ErrorNotFound
	case http.StatusConflict: return ErrorConflict
	case http.StatusTooManyRequests: return ErrorRateLimited
	default:
		if status >= 500 { return ErrorServer }
		return ""
	}
}
"#,
    );
    for operation in sorted_operations(contract) {
        let name = go_exported(&operation.operation_id);
        source.push_str(&format!(
            "\nfunc (c *Client) {name}(ctx context.Context, pathParameters map[string]string, query url.Values, body []byte) (*http.Response, error) {{\n\treturn c.Do(ctx, Operations[{}], pathParameters, query, body)\n}}\n\nfunc (c *Client) {name}WithOptions(ctx context.Context, pathParameters map[string]string, query url.Values, body []byte, options CallOptions) (*http.Response, error) {{\n\treturn c.DoWithOptions(ctx, Operations[{}], pathParameters, query, body, options)\n}}\n",
            go_string(&operation.operation_id),
            go_string(&operation.operation_id)
        ));
    }
    source
}

fn render_go_client_test(contract: &ServiceContractV3) -> String {
    let package = go_package(&contract.service_id);
    format!(
        r#"// Code generated by ojos service. DO NOT EDIT.
package {package}

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"sync"
	"testing"
	"time"
)

type rotatingProvider struct {{ mu sync.RWMutex; snapshot ContextSnapshot }}
func (p *rotatingProvider) Current(context.Context) (ContextSnapshot, error) {{ p.mu.RLock(); defer p.mu.RUnlock(); return p.snapshot, nil }}
func (p *rotatingProvider) set(snapshot ContextSnapshot) {{ p.mu.Lock(); p.snapshot = snapshot; p.mu.Unlock() }}

func TestGeneratedClientReloadsContextAndCredentialsPerRequest(t *testing.T) {{
	seen := make(chan string, 2)
	serverOne := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{ seen <- "one:"+r.Header.Get("Authorization"); w.WriteHeader(http.StatusOK) }})); defer serverOne.Close()
	serverTwo := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{ seen <- "two:"+r.Header.Get("Authorization"); if r.Header.Get("Idempotency-Key") != "idem-1" {{ t.Errorf("missing idempotency key") }}; w.WriteHeader(http.StatusOK) }})); defer serverTwo.Close()
	provider := &rotatingProvider{{snapshot: ContextSnapshot{{Generation: 1, BaseURL: serverOne.URL, Token: "old"}}}}
	client := NewClient(provider, serverOne.Client())
	operation := Operation{{ID: "fixture.get", Method: http.MethodGet, Path: "/resources/{{id}}", Audience: "user"}}
	parameters := map[string]string{{"id": "1"}}
	if _, err := client.Do(context.Background(), operation, parameters, url.Values{{}}, nil); err != nil {{ t.Fatal(err) }}
	provider.set(ContextSnapshot{{Generation: 2, BaseURL: serverTwo.URL, Token: "new"}})
	if _, err := client.DoWithOptions(context.Background(), operation, parameters, url.Values{{}}, nil, CallOptions{{IdempotencyKey: "idem-1"}}); err != nil {{ t.Fatal(err) }}
	if first, second := <-seen, <-seen; first != "one:Bearer old" || second != "two:Bearer new" {{ t.Fatalf("provider/token rotation not observed: %q %q", first, second) }}
}}

func TestGeneratedClientTimeoutAndErrorMapping(t *testing.T) {{
	timeoutServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{ <-r.Context().Done() }})); defer timeoutServer.Close()
	client := NewClient(StaticContextProvider{{Snapshot: ContextSnapshot{{BaseURL: timeoutServer.URL}}}}, timeoutServer.Client())
	client.Timeout = 10 * time.Millisecond
	operation := Operation{{ID: "fixture.get", Method: http.MethodGet, Path: "/resources/{{id}}", Audience: "user"}}
	_, err := client.Do(context.Background(), operation, map[string]string{{"id": "1"}}, url.Values{{}}, nil)
	var clientErr *ClientError
	if !errors.As(err, &clientErr) || clientErr.Kind != ErrorTimeout {{ t.Fatalf("timeout mapping: %v", err) }}
	conflictServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{ w.WriteHeader(http.StatusConflict) }})); defer conflictServer.Close()
	client = NewClient(StaticContextProvider{{Snapshot: ContextSnapshot{{BaseURL: conflictServer.URL}}}}, conflictServer.Client())
	_, err = client.Do(context.Background(), operation, map[string]string{{"id": "1"}}, url.Values{{}}, nil)
	if !errors.As(err, &clientErr) || clientErr.Kind != ErrorConflict || clientErr.StatusCode != http.StatusConflict {{ t.Fatalf("status mapping: %v", err) }}
}}

func TestGeneratedClientBinaryBodyHeadersAndProtectedOverrides(t *testing.T) {{
	payload := []byte{{0, 1, 2, 255}}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{
		if got := r.Header.Get("Content-Type"); got != "application/octet-stream" {{ t.Errorf("content type = %q", got) }}
		if got := r.Header.Get("X-OJOS-Content-Sha256"); got != "digest" {{ t.Errorf("digest header = %q", got) }}
		data, err := io.ReadAll(r.Body); if err != nil {{ t.Error(err); w.WriteHeader(http.StatusBadRequest); return }}
		if !bytes.Equal(data, payload) {{ t.Errorf("binary body changed: %v", data) }}
		w.WriteHeader(http.StatusOK)
	}})); defer server.Close()
	client := NewClient(StaticContextProvider{{Snapshot: ContextSnapshot{{BaseURL: server.URL, Token: "trusted"}}}}, server.Client())
	operation := Operation{{ID: "fixture.put", Method: http.MethodPut, Path: "/resources/{{id}}", Audience: "internal", HeaderParameters: []HeaderParameter{{{{Name: "X-OJOS-Content-Sha256", Required: true}}}}, RequestContentTypes: []string{{"application/octet-stream"}}, RequestBodyRequired: true}}
	parameters := map[string]string{{"id": "1"}}
	if _, err := client.DoWithOptions(context.Background(), operation, parameters, url.Values{{}}, nil, CallOptions{{RawBody: payload, Headers: http.Header{{"X-OJOS-Content-Sha256": []string{{"digest"}}}}}}); err != nil {{ t.Fatal(err) }}
	_, err := client.DoWithOptions(context.Background(), operation, parameters, url.Values{{}}, nil, CallOptions{{RawBody: payload, Headers: http.Header{{"Authorization": []string{{"attacker"}}, "X-OJOS-Content-Sha256": []string{{"digest"}}}}}})
	var configurationErr *ClientError
	if !errors.As(err, &configurationErr) || configurationErr.Kind != ErrorConfiguration {{ t.Fatalf("protected header override: %v", err) }}
}}
"#,
    )
}

fn render_go_events(contract: &ServiceContractV3) -> Result<String> {
    let package = go_package(&contract.service_id);
    let events = all_events(contract);
    let mut source =
        format!("// Code generated by ojos service. DO NOT EDIT.\npackage {package}\n");
    if !events.is_empty() {
        source.push_str(
            "\nimport (\n\t\"bytes\"\n\t\"encoding/json\"\n\t\"errors\"\n\t\"fmt\"\n\t\"io\"\n\t\"strconv\"\n)\n",
        );
    }
    source.push_str(
        r#"
type EventDescriptor[T any] struct {
	Type string `json:"type"`
	Version uint32 `json:"version"`
	SchemaDigest string `json:"schemaDigest"`
	Delivery string `json:"delivery"`
}

type TypedEvent[T any] struct {
	Descriptor EventDescriptor[T] `json:"descriptor"`
	Payload T `json:"payload"`
}

func (descriptor EventDescriptor[T]) New(payload T) TypedEvent[T] {
	return TypedEvent[T]{Descriptor: descriptor, Payload: payload}
}

"#,
    );
    if !events.is_empty() {
        source.push_str(
            r#"func strictEventJSON(data []byte, destination any) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(destination); err != nil { return err }
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		if err == nil { return errors.New("multiple JSON values") }
		return err
	}
	return nil
}

func mustEventSchema(document string) map[string]any {
	var schema map[string]any
	if err := strictEventJSON([]byte(document), &schema); err != nil { panic(err) }
	return schema
}

func validateEventValue(value any, schema map[string]any, path string) error {
	if constant, ok := schema["const"]; ok {
		if value != constant { return fmt.Errorf("%s must equal %v", path, constant) }
		return nil
	}
	if values, ok := schema["enum"].([]any); ok {
		for _, candidate := range values { if value == candidate { return nil } }
		return fmt.Errorf("%s is not an allowed value", path)
	}
	kind, _ := schema["type"].(string)
	switch kind {
	case "object":
		object, ok := value.(map[string]any); if !ok { return fmt.Errorf("%s must be an object", path) }
		properties, ok := schema["properties"].(map[string]any); if !ok { return fmt.Errorf("%s has an invalid schema", path) }
		for key := range object { if _, ok := properties[key]; !ok { return fmt.Errorf("%s contains unknown field %q", path, key) } }
		if required, ok := schema["required"].([]any); ok {
			for _, item := range required {
				name, ok := item.(string); if !ok { return fmt.Errorf("%s has an invalid schema", path) }
				if _, present := object[name]; !present { return fmt.Errorf("%s is missing required field %q", path, name) }
			}
		}
		for key, rawSchema := range properties {
			child, present := object[key]; if !present { continue }
			childSchema, ok := rawSchema.(map[string]any); if !ok { return fmt.Errorf("%s.%s has an invalid schema", path, key) }
			if err := validateEventValue(child, childSchema, path+"."+key); err != nil { return err }
		}
		return nil
	case "array":
		array, ok := value.([]any); if !ok { return fmt.Errorf("%s must be an array", path) }
		itemSchema, ok := schema["items"].(map[string]any); if !ok { return fmt.Errorf("%s has an invalid schema", path) }
		for index, item := range array { if err := validateEventValue(item, itemSchema, fmt.Sprintf("%s[%d]", path, index)); err != nil { return err } }
		return nil
	case "string":
		if _, ok := value.(string); !ok { return fmt.Errorf("%s must be a string", path) }
	case "integer":
		number, ok := value.(json.Number); if !ok { return fmt.Errorf("%s must be an integer", path) }
		if _, err := strconv.ParseInt(number.String(), 10, 64); err != nil { return fmt.Errorf("%s must be a signed 64-bit integer", path) }
	case "number":
		number, ok := value.(json.Number); if !ok { return fmt.Errorf("%s must be a number", path) }
		if _, err := strconv.ParseFloat(number.String(), 64); err != nil { return fmt.Errorf("%s must be a finite number", path) }
	case "boolean":
		if _, ok := value.(bool); !ok { return fmt.Errorf("%s must be a boolean", path) }
	default:
		return fmt.Errorf("%s has unsupported schema type %q", path, kind)
	}
	return nil
}

func validateEventPayloadJSON(data []byte, schema map[string]any) error {
	var value any
	if err := strictEventJSON(data, &value); err != nil { return err }
	return validateEventValue(value, schema, "payload")
}

"#,
        );
    }
    source.push('\n');
    for event in events {
        let name = go_exported(&format!("{}V{}", event.event_type, event.version));
        source.push_str(&render_go_payload(&name, &event.payload_schema)?);
        let schema_json = serde_json_canonicalizer::to_vec(&event.payload_schema)
            .map_err(CodegenError::Serialize)?;
        let schema_json = String::from_utf8(schema_json)
            .map_err(|error| CodegenError::Drift(error.to_string()))?;
        source.push_str(&format!(
            "const {name}Type = {}\nconst {name}Version uint32 = {}\nconst {name}SchemaDigest = {}\nconst {name}Delivery = {}\n\nvar {name}Descriptor = EventDescriptor[{name}Payload]{{Type: {name}Type, Version: {name}Version, SchemaDigest: {name}SchemaDigest, Delivery: {name}Delivery}}\nvar {}PayloadSchema = mustEventSchema({})\n\nfunc Encode{name}(value TypedEvent[{name}Payload]) ([]byte, error) {{\n\tif value.Descriptor.Type != {name}Type || value.Descriptor.Version != {name}Version || value.Descriptor.SchemaDigest != {name}SchemaDigest || value.Descriptor.Delivery != {name}Delivery {{ return nil, errors.New(\"event descriptor mismatch\") }}\n\tpayload, err := json.Marshal(value.Payload); if err != nil {{ return nil, err }}\n\tif err := validateEventPayloadJSON(payload, {}PayloadSchema); err != nil {{ return nil, err }}\n\treturn json.Marshal(value)\n}}\nfunc Decode{name}(data []byte) (TypedEvent[{name}Payload], error) {{\n\tvar result TypedEvent[{name}Payload]\n\tvar wire struct {{ Descriptor EventDescriptor[{name}Payload] `json:\"descriptor\"`; Payload json.RawMessage `json:\"payload\"` }}\n\tif err := strictEventJSON(data, &wire); err != nil {{ return result, err }}\n\tif wire.Descriptor.Type != {name}Type || wire.Descriptor.Version != {name}Version || wire.Descriptor.SchemaDigest != {name}SchemaDigest || wire.Descriptor.Delivery != {name}Delivery {{ return result, errors.New(\"event descriptor mismatch\") }}\n\tif err := validateEventPayloadJSON(wire.Payload, {}PayloadSchema); err != nil {{ return result, err }}\n\tvar payload {name}Payload; if err := strictEventJSON(wire.Payload, &payload); err != nil {{ return result, err }}\n\treturn TypedEvent[{name}Payload]{{Descriptor: {name}Descriptor, Payload: payload}}, nil\n}}\n\n",
            go_string(&event.event_type),
            event.version,
            go_string(&event.schema.digest),
            go_string(&event.delivery),
            go_unexported(&name),
            go_string(&schema_json),
            go_unexported(&name),
            go_unexported(&name),
        ));
    }
    Ok(source)
}

fn render_go_events_test(contract: &ServiceContractV3) -> Result<String> {
    let package = go_package(&contract.service_id);
    let events = all_events(contract);
    let mut source =
        format!("// Code generated by ojos service. DO NOT EDIT.\npackage {package}\n");
    if events.is_empty() {
        return Ok(source);
    }
    source.push_str("\nimport (\n\t\"encoding/json\"\n\t\"testing\"\n)\n\n");
    for event in events {
        let name = go_exported(&format!("{}V{}", event.event_type, event.version));
        let sample = sample_event_value(&event.payload_schema)?;
        let wire = serde_json::json!({
            "descriptor": {
                "type": event.event_type,
                "version": event.version,
                "schemaDigest": event.schema.digest,
                "delivery": event.delivery,
            },
            "payload": sample,
        });
        let wire = serde_json::to_string(&wire).map_err(CodegenError::Serialize)?;
        source.push_str(&format!(
            "func Test{name}StrictCodec(t *testing.T) {{\n\traw := []byte({})\n\tif _, err := Decode{name}(raw); err != nil {{ t.Fatalf(\"valid event rejected: %v\", err) }}\n\tvar value map[string]any; if err := json.Unmarshal(raw, &value); err != nil {{ t.Fatal(err) }}\n\tdescriptor := value[\"descriptor\"].(map[string]any); descriptor[\"schemaDigest\"] = \"sha256:tampered\"\n\ttampered, err := json.Marshal(value); if err != nil {{ t.Fatal(err) }}\n\tif _, err := Decode{name}(tampered); err == nil {{ t.Fatal(\"descriptor tampering accepted\") }}\n\tif err := json.Unmarshal(raw, &value); err != nil {{ t.Fatal(err) }}; payload := value[\"payload\"].(map[string]any); payload[\"__unknown\"] = true\n\ttampered, err = json.Marshal(value); if err != nil {{ t.Fatal(err) }}\n\tif _, err := Decode{name}(tampered); err == nil {{ t.Fatal(\"unknown payload field accepted\") }}\n}}\n\n",
            go_string(&wire),
        ));
    }
    Ok(source)
}

fn schema_object(schema: &Value) -> Result<&Map<String, Value>> {
    schema.as_object().ok_or_else(|| {
        CodegenError::Drift("event payload schema must be a JSON object".to_string())
    })
}

fn schema_properties(schema: &Value) -> Result<(&Map<String, Value>, BTreeSet<&str>)> {
    let object = schema_object(schema)?;
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| CodegenError::Drift("event object schema needs properties".to_string()))?;
    let required = object
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    Ok((properties, required))
}

fn schema_kind(schema: &Value) -> Result<&str> {
    let object = schema_object(schema)?;
    if object.contains_key("enum") || object.contains_key("const") {
        Ok("string")
    } else {
        object.get("type").and_then(Value::as_str).ok_or_else(|| {
            CodegenError::Drift("event property must declare type, enum, or const".to_string())
        })
    }
}

fn schema_string_values(schema: &Value) -> Result<Vec<&str>> {
    let object = schema_object(schema)?;
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    CodegenError::Drift("event enum values must be strings".to_string())
                })
            })
            .collect();
    }
    if let Some(value) = object.get("const").and_then(Value::as_str) {
        return Ok(vec![value]);
    }
    Err(CodegenError::Drift(
        "event schema does not declare string enum or const".to_string(),
    ))
}

fn sample_event_value(schema: &Value) -> Result<Value> {
    if let Some(value) = schema_object(schema)?.get("const") {
        return Ok(value.clone());
    }
    if let Some(value) = schema_object(schema)?
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return Ok(value.clone());
    }
    Ok(match schema_kind(schema)? {
        "object" => {
            let (properties, _) = schema_properties(schema)?;
            let mut value = Map::new();
            for (name, property) in properties {
                value.insert(name.clone(), sample_event_value(property)?);
            }
            Value::Object(value)
        }
        "array" => Value::Array(vec![sample_event_value(
            schema_object(schema)?
                .get("items")
                .ok_or_else(|| CodegenError::Drift("event array needs items".to_string()))?,
        )?]),
        "string" => Value::String("generated".to_string()),
        "integer" => Value::from(1_i64),
        "number" => serde_json::json!(1.5),
        "boolean" => Value::Bool(true),
        kind => {
            return Err(CodegenError::Drift(format!(
                "unsupported event type {kind}"
            )));
        }
    })
}

fn render_go_payload(event_name: &str, schema: &Value) -> Result<String> {
    let mut declarations = String::new();
    let mut names = BTreeSet::new();
    render_go_object(
        &format!("{event_name}Payload"),
        schema,
        &mut declarations,
        &mut names,
    )?;
    Ok(declarations)
}

fn render_go_object(
    name: &str,
    schema: &Value,
    output: &mut String,
    names: &mut BTreeSet<String>,
) -> Result<()> {
    insert_identifier(names, name.to_string(), "Go event payload")?;
    let (properties, required) = schema_properties(schema)?;
    let mut nested = String::new();
    let mut fields = BTreeSet::new();
    output.push_str(&format!("type {name} struct {{\n"));
    for (property, property_schema) in properties {
        let field = go_exported(property);
        insert_identifier(&mut fields, field.clone(), "Go event field")?;
        let nested_name = format!("{name}{field}");
        let mut field_type = go_schema_type(&nested_name, property_schema, &mut nested, names)?;
        let is_required = required.contains(property.as_str());
        if !is_required {
            field_type = format!("*{field_type}");
        }
        output.push_str(&format!(
            "\t{field} {field_type} `json:\"{property}{}\"`\n",
            if is_required { "" } else { ",omitempty" }
        ));
    }
    output.push_str("}\n\n");
    output.push_str(&nested);
    Ok(())
}

fn go_schema_type(
    name: &str,
    schema: &Value,
    nested: &mut String,
    names: &mut BTreeSet<String>,
) -> Result<String> {
    Ok(match schema_kind(schema)? {
        "object" => {
            render_go_object(name, schema, nested, names)?;
            name.to_string()
        }
        "array" => {
            let items = schema_object(schema)?
                .get("items")
                .ok_or_else(|| CodegenError::Drift("event array needs items".to_string()))?;
            format!(
                "[]{}",
                go_schema_type(&format!("{name}Item"), items, nested, names)?
            )
        }
        "string"
            if schema_object(schema)?.contains_key("enum")
                || schema_object(schema)?.contains_key("const") =>
        {
            insert_identifier(names, name.to_string(), "Go event payload")?;
            let values = schema_string_values(schema)?;
            let mut constants = BTreeSet::new();
            nested.push_str(&format!("type {name} string\n\nconst (\n"));
            for value in values {
                let constant = format!("{name}Value{}", go_exported(value));
                insert_identifier(&mut constants, constant.clone(), "Go event enum value")?;
                nested.push_str(&format!("\t{constant} {name} = {}\n", go_string(value)));
            }
            nested.push_str(")\n\n");
            name.to_string()
        }
        "string" => "string".to_string(),
        "integer" => "int64".to_string(),
        "number" => "float64".to_string(),
        "boolean" => "bool".to_string(),
        kind => {
            return Err(CodegenError::Drift(format!(
                "unsupported event type {kind}"
            )));
        }
    })
}

fn render_rust_manifest(contract: &ServiceContractV3) -> String {
    format!(
        "[package]\nname = \"{}-client\"\nversion = \"{}\"\nedition = \"2021\"\npublish = false\n\n[lib]\npath = \"src/lib.rs\"\n\n[dependencies]\nserde = {{ version = \"1\", features = [\"derive\"] }}\nserde_json = \"1\"\n\n[workspace]\n",
        rust_crate(&contract.service_id),
        contract.service_version
    )
}

fn render_rust_lib(contract: &ServiceContractV3) -> String {
    format!(
        "// Code generated by ojos service. DO NOT EDIT.\npub mod client;\npub mod events;\n\npub const SERVICE_ID: &str = {};\npub const SERVICE_VERSION: &str = {};\n",
        rust_string(&contract.service_id),
        rust_string(&contract.service_version.to_string())
    )
}

fn render_rust_client(contract: &ServiceContractV3) -> String {
    let mut source = r#"// Code generated by ojos service. DO NOT EDIT.
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operation {
    pub id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub audience: &'static str,
    pub permission: Option<&'static str>,
    pub header_parameters: &'static [HeaderParameter],
    pub request_content_types: &'static [&'static str],
    pub request_body_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderParameter { pub name: &'static str, pub required: bool }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSnapshot {
    pub generation: u64,
    pub base_url: String,
    pub token: String,
}

pub trait ContextSnapshotProvider: Send + Sync {
    fn current(&self) -> Result<ContextSnapshot, ClientError>;
}

impl<F> ContextSnapshotProvider for F
where F: Fn() -> Result<ContextSnapshot, ClientError> + Send + Sync {
    fn current(&self) -> Result<ContextSnapshot, ClientError> { self() }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallOptions {
    pub timeout: Duration,
    pub idempotency_key: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub content_type: Option<String>,
    pub raw_body: Option<Vec<u8>>,
}

impl Default for CallOptions {
    fn default() -> Self { Self { timeout: Duration::from_secs(30), idempotency_key: None, headers: BTreeMap::new(), content_type: None, raw_body: None } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind { Configuration, Timeout, Transport, Unauthorized, Forbidden, NotFound, Conflict, RateLimited, Server }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    MissingPathParameter(String),
    InvalidContext(String),
    InvalidRequest(String),
    Http { kind: ErrorKind, status: Option<u16>, message: String },
}

"#.to_string();
    for operation in sorted_operations(contract) {
        source.push_str(&format!(
            "pub const {}: Operation = Operation {{ id: {}, method: {}, path: {}, audience: {}, permission: {}, header_parameters: {}, request_content_types: {}, request_body_required: {} }};\n",
            rust_const(&operation.operation_id),
            rust_string(&operation.operation_id),
            rust_string(&operation.method),
            rust_string(&operation.provider_path),
            rust_string(&operation.audience),
            rust_option(operation.permission.as_deref()),
            rust_header_parameters(operation),
            rust_request_content_types(operation),
            operation
                .request_body
                .as_ref()
                .map(|body| body.required)
                .unwrap_or(false),
        ));
    }
    source.push_str("\npub const OPERATIONS: &[Operation] = &[\n");
    for operation in sorted_operations(contract) {
        source.push_str(&format!("    {},\n", rust_const(&operation.operation_id)));
    }
    source.push_str(
        r#"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub timeout: Duration,
}

#[derive(Clone)]
pub struct Client { context: Arc<dyn ContextSnapshotProvider> }

impl Client {
    pub fn new(context: Arc<dyn ContextSnapshotProvider>) -> Self { Self { context } }

    pub fn request(&self, operation: Operation, path_parameters: &BTreeMap<String, String>, body: Vec<u8>) -> Result<Request, ClientError> {
        self.request_with_options(operation, path_parameters, body, CallOptions::default())
    }

    pub fn request_with_options(&self, operation: Operation, path_parameters: &BTreeMap<String, String>, body: Vec<u8>, options: CallOptions) -> Result<Request, ClientError> {
        let snapshot = self.context.current()?;
        let base_url = snapshot.base_url.trim_end_matches('/');
        if base_url.is_empty() { return Err(ClientError::InvalidContext("base URL is required".to_owned())); }
        let mut path = operation.path.to_owned();
        for (key, value) in path_parameters { path = path.replace(&format!("{{{key}}}"), &percent_encode(value)); }
        if let Some(start) = path.find('{') {
            let tail = &path[start + 1..];
            let key = tail.split('}').next().unwrap_or(tail);
            return Err(ClientError::MissingPathParameter(key.to_owned()));
        }
        let raw_body_present = options.raw_body.is_some();
        let effective_body = match options.raw_body {
            Some(_) if !body.is_empty() => return Err(ClientError::InvalidRequest("body and raw body cannot both be set".to_owned())),
            Some(raw_body) => raw_body,
            None => body,
        };
        let has_body = raw_body_present || !effective_body.is_empty();
        if operation.request_body_required && !has_body { return Err(ClientError::InvalidRequest("request body is required".to_owned())); }
        let content_type = select_content_type(operation, options.content_type.as_deref(), has_body)?;
        let mut headers = BTreeMap::new();
        for (name, value) in options.headers {
            if !valid_header_name(&name) { return Err(ClientError::InvalidRequest(format!("invalid header name {name:?}"))); }
            if forbidden_header(&name) { return Err(ClientError::InvalidRequest(format!("header {name} cannot be overridden"))); }
            if !valid_header_value(&value) { return Err(ClientError::InvalidRequest(format!("invalid value for header {name}"))); }
            headers.insert(name, value);
        }
        if let Some(content_type) = content_type { headers.insert("Content-Type".to_owned(), content_type); }
        if !snapshot.token.is_empty() { headers.insert("Authorization".to_owned(), format!("Bearer {}", snapshot.token)); }
        if let Some(key) = options.idempotency_key { if !key.trim().is_empty() { headers.insert("Idempotency-Key".to_owned(), key); } }
        for parameter in operation.header_parameters {
            if parameter.required && !headers.iter().any(|(name, value)| name.eq_ignore_ascii_case(parameter.name) && !value.trim().is_empty()) {
                return Err(ClientError::InvalidRequest(format!("required header {} is missing", parameter.name)));
            }
        }
        Ok(Request { method: operation.method, url: format!("{base_url}{path}"), headers, body: effective_body, timeout: options.timeout })
    }

    pub fn map_status(status: u16) -> Option<ClientError> {
        let kind = match status { 401 => ErrorKind::Unauthorized, 403 => ErrorKind::Forbidden, 404 => ErrorKind::NotFound, 409 => ErrorKind::Conflict, 429 => ErrorKind::RateLimited, 500..=599 => ErrorKind::Server, _ => return None };
        Some(ClientError::Http { kind, status: Some(status), message: format!("HTTP {status}") })
    }

    pub fn timeout_error(message: impl Into<String>) -> ClientError { ClientError::Http { kind: ErrorKind::Timeout, status: None, message: message.into() } }
}

fn select_content_type(operation: Operation, requested: Option<&str>, has_body: bool) -> Result<Option<String>, ClientError> {
    let requested = requested.map(str::trim).filter(|value| !value.is_empty());
    if !has_body {
        if requested.is_some() { return Err(ClientError::InvalidRequest("content type cannot be set without a request body".to_owned())); }
        return Ok(None);
    }
    if let Some(requested) = requested {
        if let Some(declared) = operation.request_content_types.iter().find(|declared| declared.eq_ignore_ascii_case(requested)) { return Ok(Some((*declared).to_owned())); }
        return Err(ClientError::InvalidRequest(format!("content type {requested} is not declared for operation {}", operation.id)));
    }
    Ok(operation.request_content_types.first().map(|value| (*value).to_owned()))
}

fn forbidden_header(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if ["proxy-", "sec-", "x-forwarded-", "x-ojos-caller-", "x-ojos-gateway-", "x-ojos-internal-", "x-ojos-workload-"].iter().any(|prefix| name.starts_with(prefix)) { return true; }
    matches!(name.as_str(), "authorization" | "host" | "cookie" | "set-cookie" | "connection" | "keep-alive" | "te" | "trailer" | "transfer-encoding" | "upgrade" | "content-length" | "content-type" | "idempotency-key" | "forwarded" | "via" | "expect" | "x-api-key" | "api-key")
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn valid_header_value(value: &str) -> bool {
    value.bytes().all(|byte| byte == b'\t' || byte >= 0x20 && byte != 0x7f)
}

fn percent_encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') { output.push(byte as char); }
        else { output.push_str(&format!("%{byte:02X}")); }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, RwLock};

    #[test]
    fn provider_and_credentials_are_read_per_request() {
        let snapshot = Arc::new(RwLock::new(ContextSnapshot { generation: 1, base_url: "https://one.example".into(), token: "old".into() }));
        let source = snapshot.clone();
        let provider = move || Ok(source.read().unwrap().clone());
        let client = Client::new(Arc::new(provider));
        let operation = Operation { id: "fixture.get", method: "GET", path: "/resources/{id}", audience: "user", permission: None, header_parameters: &[], request_content_types: &[], request_body_required: false };
        let mut parameters = BTreeMap::new(); parameters.insert("id".to_owned(), "7".to_owned());
        let first = client.request(operation, &parameters, Vec::new()).unwrap();
        *snapshot.write().unwrap() = ContextSnapshot { generation: 2, base_url: "https://two.example".into(), token: "new".into() };
        let second = client.request_with_options(operation, &parameters, Vec::new(), CallOptions { timeout: Duration::from_millis(25), idempotency_key: Some("idem-1".into()), ..CallOptions::default() }).unwrap();
        assert!(first.url.starts_with("https://one.example")); assert_eq!(first.headers["Authorization"], "Bearer old");
        assert!(second.url.starts_with("https://two.example")); assert_eq!(second.headers["Authorization"], "Bearer new"); assert_eq!(second.headers["Idempotency-Key"], "idem-1"); assert_eq!(second.timeout, Duration::from_millis(25));
        assert!(matches!(Client::map_status(409), Some(ClientError::Http { kind: ErrorKind::Conflict, .. })));
        assert!(matches!(Client::timeout_error("deadline"), ClientError::Http { kind: ErrorKind::Timeout, .. }));
    }

    #[test]
    fn binary_body_declared_headers_and_protected_overrides() {
        let provider = || Ok(ContextSnapshot { generation: 1, base_url: "https://storage.example".into(), token: "trusted".into() });
        let client = Client::new(Arc::new(provider));
        let operation = Operation { id: "fixture.put", method: "PUT", path: "/resources/{id}", audience: "internal", permission: None, header_parameters: &[HeaderParameter { name: "X-OJOS-Content-Sha256", required: true }], request_content_types: &["application/octet-stream"], request_body_required: true };
        let mut parameters = BTreeMap::new(); parameters.insert("id".to_owned(), "1".to_owned());
        let mut headers = BTreeMap::new(); headers.insert("X-OJOS-Content-Sha256".to_owned(), "digest".to_owned());
        let request = client.request_with_options(operation, &parameters, Vec::new(), CallOptions { raw_body: Some(vec![0, 1, 2, 255]), headers: headers.clone(), ..CallOptions::default() }).unwrap();
        assert_eq!(request.body, vec![0, 1, 2, 255]); assert_eq!(request.headers["Content-Type"], "application/octet-stream");
        headers.insert("Authorization".to_owned(), "attacker".to_owned());
        assert!(matches!(client.request_with_options(operation, &parameters, Vec::new(), CallOptions { raw_body: Some(vec![1]), headers, ..CallOptions::default() }), Err(ClientError::InvalidRequest(_))));
    }
}
"#,
    );
    source
}

fn render_rust_events(contract: &ServiceContractV3) -> Result<String> {
    let events = all_events(contract);
    let mut declarations = String::new();
    for event in &events {
        let type_name = rust_type(&format!("{}V{}", event.event_type, event.version));
        declarations.push_str(&render_rust_payload(&type_name, &event.payload_schema)?);
        let name = rust_const(&format!("{}V{}", event.event_type, event.version));
        declarations.push_str(&format!(
            "pub const {name}: EventDescriptor<{type_name}Payload> = EventDescriptor::new(EventIdentity {{ event_type: {}, version: {}, schema_digest: {}, delivery: {} }});\n\n",
            rust_string(&event.event_type),
            event.version,
            rust_string(&event.schema.digest),
            rust_string(&event.delivery),
        ));
    }
    let uses_optional_non_null = declarations.contains("deserialize_optional_non_null");

    let mut source = "// Code generated by ojos service. DO NOT EDIT.\n".to_string();
    if uses_optional_non_null {
        source
            .push_str("use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};\n");
    } else {
        source.push_str("use serde::{de::DeserializeOwned, Deserialize, Serialize};\n");
    }
    source.push_str("use std::marker::PhantomData;\n\n");
    if uses_optional_non_null {
        source.push_str(
            r#"fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

"#,
        );
    }
    source.push_str(r#"#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventIdentity {
    pub event_type: &'static str,
    pub version: u32,
    pub schema_digest: &'static str,
    pub delivery: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct EventDescriptor<T> { pub identity: EventIdentity, marker: PhantomData<fn() -> T> }

impl<T> Copy for EventDescriptor<T> {}
impl<T> Clone for EventDescriptor<T> { fn clone(&self) -> Self { *self } }

impl<T> EventDescriptor<T> {
    pub const fn new(identity: EventIdentity) -> Self { Self { identity, marker: PhantomData } }
    pub fn bind(&self, payload: T) -> TypedEvent<T> { TypedEvent { descriptor: self.identity, payload } }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedEvent<T> { pub descriptor: EventIdentity, pub payload: T }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireIdentity {
    #[serde(rename = "type")]
    event_type: String,
    version: u32,
    #[serde(rename = "schemaDigest")]
    schema_digest: String,
    delivery: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEvent<T> { descriptor: WireIdentity, payload: T }

impl EventIdentity {
    fn to_wire(self) -> WireIdentity {
        WireIdentity { event_type: self.event_type.to_owned(), version: self.version, schema_digest: self.schema_digest.to_owned(), delivery: self.delivery.to_owned() }
    }

    fn matches_wire(self, wire: &WireIdentity) -> bool {
        wire.event_type == self.event_type && wire.version == self.version && wire.schema_digest == self.schema_digest && wire.delivery == self.delivery
    }
}

impl<T: Serialize + DeserializeOwned> EventDescriptor<T> {
    pub fn encode(&self, payload: T) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&WireEvent { descriptor: self.identity.to_wire(), payload })
    }

    pub fn decode(&self, json: &[u8]) -> Result<TypedEvent<T>, String> {
        let event: WireEvent<T> = serde_json::from_slice(json).map_err(|error| error.to_string())?;
        if !self.identity.matches_wire(&event.descriptor) { return Err("event descriptor mismatch".to_owned()); }
        Ok(TypedEvent { descriptor: self.identity, payload: event.payload })
    }
}

"#);
    source.push_str(&declarations);
    source.push_str("\npub const EVENTS: &[EventIdentity] = &[\n");
    for event in &events {
        source.push_str(&format!(
            "    {}.identity,\n",
            rust_const(&format!("{}V{}", event.event_type, event.version))
        ));
    }
    source.push_str("];\n");
    if let Some(event) = events.first() {
        let descriptor = rust_const(&format!("{}V{}", event.event_type, event.version));
        let sample = sample_event_value(&event.payload_schema)?;
        let wire = serde_json::json!({
            "descriptor": {
                "type": event.event_type,
                "version": event.version,
                "schemaDigest": event.schema.digest,
                "delivery": event.delivery,
            },
            "payload": sample,
        });
        let wire = serde_json::to_string(&wire).map_err(CodegenError::Serialize)?;
        source.push_str(&format!(
            "\n#[cfg(test)]\nmod tests {{\n    use super::*;\n    #[test]\n    fn typed_codec_accepts_valid_payload_and_rejects_tampering() {{\n        let encoded = {}.as_bytes();\n        assert!({descriptor}.decode(encoded).is_ok());\n        let mut descriptor_tampered: serde_json::Value = serde_json::from_slice(encoded).unwrap();\n        descriptor_tampered[\"descriptor\"][\"schemaDigest\"] = serde_json::json!(\"sha256:tampered\");\n        assert!({descriptor}.decode(&serde_json::to_vec(&descriptor_tampered).unwrap()).is_err());\n        let mut payload_tampered: serde_json::Value = serde_json::from_slice(encoded).unwrap();\n        payload_tampered[\"payload\"][\"__unknown\"] = serde_json::json!(true);\n        assert!({descriptor}.decode(&serde_json::to_vec(&payload_tampered).unwrap()).is_err());\n    }}\n}}\n",
            rust_string(&wire),
        ));
    }
    Ok(source)
}

fn render_rust_payload(event_name: &str, schema: &Value) -> Result<String> {
    let mut declarations = String::new();
    let mut names = BTreeSet::new();
    render_rust_object(
        &format!("{event_name}Payload"),
        schema,
        &mut declarations,
        &mut names,
    )?;
    Ok(declarations)
}

fn render_rust_object(
    name: &str,
    schema: &Value,
    output: &mut String,
    names: &mut BTreeSet<String>,
) -> Result<()> {
    insert_identifier(names, name.to_string(), "Rust event payload")?;
    let (properties, required) = schema_properties(schema)?;
    let mut nested = String::new();
    let mut fields = BTreeSet::new();
    output.push_str(&format!(
        "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n#[serde(deny_unknown_fields)]\npub struct {name} {{\n"
    ));
    for (property, property_schema) in properties {
        let field = rust_field(property);
        insert_identifier(&mut fields, field.clone(), "Rust event field")?;
        let nested_name = format!("{name}{}", rust_type(property));
        let mut field_type = rust_schema_type(&nested_name, property_schema, &mut nested, names)?;
        let is_required = required.contains(property.as_str());
        if !is_required {
            field_type = format!("Option<{field_type}>");
            output.push_str(&format!(
                "    #[serde(rename = {}, default, skip_serializing_if = \"Option::is_none\", deserialize_with = \"deserialize_optional_non_null\")]\n",
                rust_string(property)
            ));
        } else {
            output.push_str(&format!(
                "    #[serde(rename = {})]\n",
                rust_string(property)
            ));
        }
        output.push_str(&format!("    pub {field}: {field_type},\n"));
    }
    output.push_str("}\n\n");
    output.push_str(&nested);
    Ok(())
}

fn rust_schema_type(
    name: &str,
    schema: &Value,
    nested: &mut String,
    names: &mut BTreeSet<String>,
) -> Result<String> {
    Ok(match schema_kind(schema)? {
        "object" => {
            render_rust_object(name, schema, nested, names)?;
            name.to_string()
        }
        "array" => {
            let items = schema_object(schema)?
                .get("items")
                .ok_or_else(|| CodegenError::Drift("event array needs items".to_string()))?;
            format!(
                "Vec<{}>",
                rust_schema_type(&format!("{name}Item"), items, nested, names)?
            )
        }
        "string"
            if schema_object(schema)?.contains_key("enum")
                || schema_object(schema)?.contains_key("const") =>
        {
            insert_identifier(names, name.to_string(), "Rust event payload")?;
            let mut variants = BTreeSet::new();
            nested.push_str(&format!(
                "#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]\npub enum {name} {{\n"
            ));
            for value in schema_string_values(schema)? {
                let variant = format!("Value{}", rust_type(value));
                insert_identifier(&mut variants, variant.clone(), "Rust event enum value")?;
                nested.push_str(&format!(
                    "    #[serde(rename = {})]\n    {variant},\n",
                    rust_string(value)
                ));
            }
            nested.push_str("}\n\n");
            name.to_string()
        }
        "string" => "String".to_string(),
        "integer" => "i64".to_string(),
        "number" => "f64".to_string(),
        "boolean" => "bool".to_string(),
        kind => {
            return Err(CodegenError::Drift(format!(
                "unsupported event type {kind}"
            )));
        }
    })
}

fn render_ts_package(contract: &ServiceContractV3) -> String {
    format!(
        "{{\n  \"name\": \"@ojos-generated/{}-client\",\n  \"version\": {},\n  \"private\": true,\n  \"type\": \"module\",\n  \"exports\": \"./src/index.ts\",\n  \"scripts\": {{\"typecheck\": \"tsc --noEmit\"}},\n  \"devDependencies\": {{\"typescript\": \"5.9.2\"}}\n}}\n",
        npm_name(&contract.service_id),
        ts_string(&contract.service_version.to_string()),
    )
}

fn render_ts_config() -> String {
    "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"module\": \"NodeNext\",\n    \"moduleResolution\": \"NodeNext\",\n    \"rootDir\": \"src\",\n    \"lib\": [\"ES2022\", \"DOM\"],\n    \"strict\": true,\n    \"noEmit\": true\n  },\n  \"include\": [\"src/**/*.ts\"]\n}\n".to_string()
}

fn render_ts_client(contract: &ServiceContractV3) -> String {
    let mut source = r#"// Code generated by ojos service. DO NOT EDIT.
export interface Operation {
  readonly id: string;
  readonly method: string;
  readonly path: string;
  readonly audience: string;
  readonly permission?: string;
  readonly headerParameters: readonly HeaderParameter[];
  readonly requestContentTypes: readonly string[];
  readonly requestBodyRequired: boolean;
}

export interface HeaderParameter { readonly name: string; readonly required: boolean; }

export interface ContextSnapshot { readonly generation: number; readonly baseUrl: string; readonly token?: string; }
export type ContextSnapshotProvider = () => ContextSnapshot | Promise<ContextSnapshot>;
export type FetchLike = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export interface ClientOptions { readonly context: ContextSnapshotProvider; readonly fetch?: FetchLike; readonly timeoutMs?: number; }
export interface CallOptions {
  readonly path?: Readonly<Record<string, string>>;
  readonly query?: Readonly<Record<string, string | number | boolean | undefined>>;
  readonly body?: unknown;
  readonly rawBody?: BodyInit;
  readonly headers?: Readonly<Record<string, string>>;
  readonly contentType?: string;
  readonly signal?: AbortSignal;
  readonly timeoutMs?: number;
  readonly idempotencyKey?: string;
}

export type ClientErrorKind = 'configuration' | 'timeout' | 'transport' | 'unauthorized' | 'forbidden' | 'not_found' | 'conflict' | 'rate_limited' | 'server';
export class ClientError extends Error {
  constructor(readonly kind: ClientErrorKind, message: string, readonly status?: number, options?: ErrorOptions) { super(message, options); }
}

export const operations = {
"#.to_string();
    for operation in sorted_operations(contract) {
        source.push_str(&format!(
            "  {}: {{ id: {}, method: {}, path: {}, audience: {},{} headerParameters: {}, requestContentTypes: {}, requestBodyRequired: {} }} as const,\n",
            ts_string(&operation.operation_id),
            ts_string(&operation.operation_id),
            ts_string(&operation.method),
            ts_string(&operation.provider_path),
            ts_string(&operation.audience),
            operation
                .permission
                .as_deref()
                .map(|permission| format!(" permission: {},", ts_string(permission)))
                .unwrap_or_default(),
            ts_header_parameters(operation),
            ts_request_content_types(operation),
            operation
                .request_body
                .as_ref()
                .map(|body| body.required)
                .unwrap_or(false),
        ));
    }
    source.push_str(
        r#"} satisfies Readonly<Record<string, Operation>>;

function expandPath(template: string, parameters: Readonly<Record<string, string>>): string {
  return template.replace(/\{([^}]+)\}/g, (_match: string, key: string) => {
    const value = parameters[key]; if (value === undefined) throw new ClientError('configuration', `missing path parameter ${key}`); return encodeURIComponent(value);
  });
}

function errorKind(status: number): ClientErrorKind | undefined {
  if (status === 401) return 'unauthorized'; if (status === 403) return 'forbidden'; if (status === 404) return 'not_found';
  if (status === 409) return 'conflict'; if (status === 429) return 'rate_limited'; if (status >= 500) return 'server'; return undefined;
}

const forbiddenHeaders = new Set(['authorization', 'host', 'cookie', 'set-cookie', 'connection', 'keep-alive', 'te', 'trailer', 'transfer-encoding', 'upgrade', 'content-length', 'content-type', 'idempotency-key', 'forwarded', 'via', 'expect', 'x-api-key', 'api-key']);
const forbiddenHeaderPrefixes = ['proxy-', 'sec-', 'x-forwarded-', 'x-ojos-caller-', 'x-ojos-gateway-', 'x-ojos-internal-', 'x-ojos-workload-'];
function forbiddenHeader(name: string): boolean { const normalized = name.trim().toLowerCase(); return forbiddenHeaders.has(normalized) || forbiddenHeaderPrefixes.some((prefix) => normalized.startsWith(prefix)); }
function validHeaderName(name: string): boolean { return /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(name); }
function validHeaderValue(value: string): boolean { return !/[\x00-\x08\x0A-\x1F\x7F]/.test(value); }
function selectContentType(operation: Operation, requested: string | undefined, hasBody: boolean): string | undefined {
  const normalized = requested?.trim();
  if (!hasBody) { if (normalized) throw new ClientError('configuration', 'content type cannot be set without a request body'); return undefined; }
  if (!normalized) return operation.requestContentTypes[0];
  const declared = operation.requestContentTypes.find((value) => value.toLowerCase() === normalized.toLowerCase());
  if (!declared) throw new ClientError('configuration', `content type ${normalized} is not declared for operation ${operation.id}`);
  return declared;
}
function jsonMediaType(contentType: string | undefined): boolean {
  const mediaType = contentType?.split(';', 1)[0]?.trim().toLowerCase();
  return mediaType === 'application/json' || mediaType?.endsWith('+json') === true;
}

export class Client {
  readonly #context: ContextSnapshotProvider; readonly #fetch: FetchLike; readonly #timeoutMs: number;
  constructor(options: ClientOptions) { this.#context = options.context; this.#fetch = options.fetch ?? globalThis.fetch.bind(globalThis); this.#timeoutMs = options.timeoutMs ?? 30_000; }

  async call(operation: Operation, options: CallOptions = {}): Promise<Response> {
    const snapshot = await this.#context();
    if (!snapshot.baseUrl.trim()) throw new ClientError('configuration', 'context snapshot base URL is required');
    const target = new URL(snapshot.baseUrl.replace(/\/$/, '') + expandPath(operation.path, options.path ?? {}));
    for (const [key, value] of Object.entries(options.query ?? {})) if (value !== undefined) target.searchParams.set(key, String(value));
    if (options.body !== undefined && options.rawBody !== undefined) throw new ClientError('configuration', 'body and raw body cannot both be set');
    const hasBody = options.body !== undefined || options.rawBody !== undefined;
    if (operation.requestBodyRequired && !hasBody) throw new ClientError('configuration', 'request body is required');
    const contentType = selectContentType(operation, options.contentType, hasBody);
    const headers = new Headers();
    for (const [name, value] of Object.entries(options.headers ?? {})) {
      if (!validHeaderName(name)) throw new ClientError('configuration', `invalid header name ${JSON.stringify(name)}`);
      if (forbiddenHeader(name)) throw new ClientError('configuration', `header ${name} cannot be overridden`);
      if (!validHeaderValue(value)) throw new ClientError('configuration', `invalid value for header ${name}`);
      headers.append(name, value);
    }
    if (contentType) headers.set('Content-Type', contentType); if (snapshot.token) headers.set('Authorization', `Bearer ${snapshot.token}`);
    if (options.idempotencyKey?.trim()) headers.set('Idempotency-Key', options.idempotencyKey);
    for (const parameter of operation.headerParameters) if (parameter.required && !headers.get(parameter.name)?.trim()) throw new ClientError('configuration', `required header ${parameter.name} is missing`);
    const controller = new AbortController(); const timeout = setTimeout(() => controller.abort(new DOMException('request timed out', 'TimeoutError')), options.timeoutMs ?? this.#timeoutMs);
    const abort = () => controller.abort(options.signal?.reason); options.signal?.addEventListener('abort', abort, { once: true });
    try {
      const requestBody = options.rawBody !== undefined ? options.rawBody : options.body === undefined ? undefined : jsonMediaType(contentType) ? JSON.stringify(options.body) : options.body as BodyInit;
      const response = await this.#fetch(target, { method: operation.method, headers, body: requestBody, signal: controller.signal });
      const kind = errorKind(response.status); if (kind) throw new ClientError(kind, `HTTP ${response.status}`, response.status); return response;
    } catch (error) {
      if (error instanceof ClientError) throw error;
      if (controller.signal.aborted && !options.signal?.aborted) throw new ClientError('timeout', 'request timed out', undefined, { cause: error });
      throw new ClientError('transport', 'request failed', undefined, { cause: error });
    } finally { clearTimeout(timeout); options.signal?.removeEventListener('abort', abort); }
  }
"#,
    );
    for operation in sorted_operations(contract) {
        source.push_str(&format!(
            "\n  {}(options: CallOptions = {{}}): Promise<Response> {{\n    return this.call(operations[{}], options);\n  }}\n",
            ts_identifier(&operation.operation_id),
            ts_string(&operation.operation_id),
        ));
    }
    source.push_str("}\n");
    source
}

fn render_ts_client_test(_contract: &ServiceContractV3) -> String {
    r#"// Code generated by ojos service. DO NOT EDIT.
import { Client, ClientError, type ContextSnapshot, type Operation } from './client.js';

const operation: Operation = { id: 'fixture.get', method: 'GET', path: '/resources/{id}', audience: 'user', headerParameters: [], requestContentTypes: [], requestBodyRequired: false };
let snapshot: ContextSnapshot = { generation: 1, baseUrl: 'https://one.example', token: 'old' };
const calls: Array<{ url: string; auth: string | null; idempotency: string | null; contentType: string | null; digest: string | null; body?: Uint8Array }> = [];
const fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
  calls.push({ url: String(input), auth: new Headers(init?.headers).get('Authorization'), idempotency: new Headers(init?.headers).get('Idempotency-Key'), contentType: new Headers(init?.headers).get('Content-Type'), digest: new Headers(init?.headers).get('X-OJOS-Content-Sha256'), body: init?.body instanceof Uint8Array ? init.body : undefined });
  return new Response(undefined, { status: calls.length === 4 ? 409 : 200 });
};
const client = new Client({ context: () => snapshot, fetch, timeoutMs: 25 });

await client.call(operation, { path: { id: '1' } });
snapshot = { generation: 2, baseUrl: 'https://two.example', token: 'new' };
await client.call(operation, { path: { id: '1' }, idempotencyKey: 'idem-1' });
if (!calls[0].url.startsWith('https://one.example') || calls[0].auth !== 'Bearer old') throw new Error('initial context not used');
if (!calls[1].url.startsWith('https://two.example') || calls[1].auth !== 'Bearer new' || calls[1].idempotency !== 'idem-1') throw new Error('context rotation not used');

const binaryOperation: Operation = { id: 'fixture.put', method: 'PUT', path: '/resources/{id}', audience: 'internal', headerParameters: [{ name: 'X-OJOS-Content-Sha256', required: true }], requestContentTypes: ['application/octet-stream'], requestBodyRequired: true };
const binary = new Uint8Array([0, 1, 2, 255]);
await client.call(binaryOperation, { path: { id: '1' }, rawBody: binary, headers: { 'X-OJOS-Content-Sha256': 'digest' } });
if (calls[2].contentType !== 'application/octet-stream' || calls[2].digest !== 'digest' || calls[2].body?.[3] !== 255) throw new Error('binary request changed');
try { await client.call(binaryOperation, { path: { id: '1' }, rawBody: binary, headers: { Authorization: 'attacker', 'X-OJOS-Content-Sha256': 'digest' } }); throw new Error('expected protected header rejection'); }
catch (error) { if (!(error instanceof ClientError) || error.kind !== 'configuration') throw error; }

const jsonOperation: Operation = { id: 'fixture.create', method: 'POST', path: '/resources', audience: 'internal', headerParameters: [], requestContentTypes: ['application/json'], requestBodyRequired: true };
let jsonBody: BodyInit | null | undefined;
const jsonClient = new Client({ context: () => snapshot, fetch: async (_input, init) => { jsonBody = init?.body; return new Response(undefined, { status: 200 }); } });
await jsonClient.call(jsonOperation, { body: { value: 7 } });
if (jsonBody !== '{"value":7}') throw new Error('JSON request was not encoded');

try {
  await client.call(operation, { path: { id: '1' } });
  throw new Error('expected conflict');
} catch (error) {
  if (!(error instanceof ClientError) || error.kind !== 'conflict' || error.status !== 409) throw error;
}

const timeoutClient = new Client({
  context: () => snapshot,
  timeoutMs: 5,
  fetch: async (_input, init) => new Promise<Response>((_resolve, reject) => {
    const signal = init?.signal;
    if (signal?.aborted) { reject(signal.reason); return; }
    signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
  }),
});
try {
  await timeoutClient.call(operation, { path: { id: '1' } });
  throw new Error('expected timeout');
} catch (error) {
  if (!(error instanceof ClientError) || error.kind !== 'timeout') throw error;
}
"#.to_string()
}

fn render_ts_events(contract: &ServiceContractV3) -> Result<String> {
    let mut source = r#"// Code generated by ojos service. DO NOT EDIT.
export interface EventDescriptor<T> { readonly type: string; readonly version: number; readonly schemaDigest: string; readonly delivery: string; readonly __payload?: T; }
export interface TypedEvent<T> { readonly descriptor: EventDescriptor<T>; readonly payload: T; }

type JsonSchema = Readonly<Record<string, unknown>>;

function assertObject(value: unknown, path: string): asserts value is Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new Error(`${path} must be an object`);
}

function assertExactKeys(value: Record<string, unknown>, allowed: readonly string[], path: string): void {
  const accepted = new Set(allowed);
  for (const key of Object.keys(value)) if (!accepted.has(key)) throw new Error(`${path} contains unknown field ${JSON.stringify(key)}`);
}

function validateEventValue(value: unknown, schema: JsonSchema, path: string): void {
  if ('const' in schema) {
    if (value !== schema.const) throw new Error(`${path} has an invalid constant value`);
    return;
  }
  if (Array.isArray(schema.enum)) {
    if (!schema.enum.includes(value)) throw new Error(`${path} is not an allowed value`);
    return;
  }
  switch (schema.type) {
    case 'object': {
      assertObject(value, path);
      const properties = schema.properties; assertObject(properties, `${path} schema properties`);
      assertExactKeys(value, Object.keys(properties), path);
      const required = schema.required;
      if (required !== undefined) {
        if (!Array.isArray(required) || !required.every((item): item is string => typeof item === 'string')) throw new Error(`${path} has an invalid required schema`);
        for (const key of required) if (!Object.prototype.hasOwnProperty.call(value, key)) throw new Error(`${path} is missing required field ${JSON.stringify(key)}`);
      }
      for (const [key, child] of Object.entries(properties)) {
        if (!Object.prototype.hasOwnProperty.call(value, key)) continue;
        assertObject(child, `${path}.${key} schema`);
        validateEventValue(value[key], child, `${path}.${key}`);
      }
      return;
    }
    case 'array': {
      if (!Array.isArray(value)) throw new Error(`${path} must be an array`);
      const items = schema.items; assertObject(items, `${path} items schema`);
      value.forEach((item, index) => validateEventValue(item, items, `${path}[${index}]`));
      return;
    }
    case 'string': if (typeof value !== 'string') throw new Error(`${path} must be a string`); return;
    case 'integer': if (typeof value !== 'number' || !Number.isSafeInteger(value)) throw new Error(`${path} must be a safe integer`); return;
    case 'number': if (typeof value !== 'number' || !Number.isFinite(value)) throw new Error(`${path} must be a finite number`); return;
    case 'boolean': if (typeof value !== 'boolean') throw new Error(`${path} must be a boolean`); return;
    default: throw new Error(`${path} has an unsupported schema type`);
  }
}

function assertWireEnvelope(value: unknown): asserts value is { readonly descriptor: Record<string, unknown>; readonly payload: unknown } {
  assertObject(value, 'event'); assertExactKeys(value, ['descriptor', 'payload'], 'event');
  if (!Object.prototype.hasOwnProperty.call(value, 'payload')) throw new Error('event is missing payload');
  assertObject(value.descriptor, 'event.descriptor');
  assertExactKeys(value.descriptor, ['type', 'version', 'schemaDigest', 'delivery'], 'event.descriptor');
}

"#.to_string();
    for event in all_events(contract) {
        let name = ts_type(&format!("{}V{}", event.event_type, event.version));
        source.push_str(&render_ts_payload(&name, &event.payload_schema)?);
        let schema = serde_json_canonicalizer::to_vec(&event.payload_schema)
            .map_err(CodegenError::Serialize)?;
        let schema =
            String::from_utf8(schema).map_err(|error| CodegenError::Drift(error.to_string()))?;
        source.push_str(&format!(
            "const {}PayloadSchema = {schema} as const satisfies JsonSchema;\nexport const {name}Descriptor = {{ type: {}, version: {}, schemaDigest: {}, delivery: {} }} as const satisfies EventDescriptor<{name}Payload>;\nexport type {name} = TypedEvent<{name}Payload>;\n\nfunction assert{name}Payload(value: unknown): asserts value is {name}Payload {{ validateEventValue(value, {}PayloadSchema, 'payload'); }}\nfunction assert{name}(value: unknown): asserts value is {name} {{\n  assertWireEnvelope(value);\n  if (value.descriptor.type !== {name}Descriptor.type || value.descriptor.version !== {name}Descriptor.version || value.descriptor.schemaDigest !== {name}Descriptor.schemaDigest || value.descriptor.delivery !== {name}Descriptor.delivery) throw new Error('event descriptor mismatch');\n  assert{name}Payload(value.payload);\n}}\nexport function new{name}(payload: {name}Payload): {name} {{ assert{name}Payload(payload); return {{ descriptor: {name}Descriptor, payload }}; }}\nexport function encode{name}(value: {name}): string {{ assert{name}(value); return JSON.stringify(value); }}\nexport function decode{name}(json: string): {name} {{\n  const value: unknown = JSON.parse(json); assert{name}(value); return value;\n}}\n\n",
            ts_identifier(&name),
            ts_string(&event.event_type),
            event.version,
            ts_string(&event.schema.digest),
            ts_string(&event.delivery),
            ts_identifier(&name),
        ));
    }
    Ok(source)
}

fn render_ts_events_test(contract: &ServiceContractV3) -> Result<String> {
    let events = all_events(contract);
    if events.is_empty() {
        return Ok("// Code generated by ojos service. DO NOT EDIT.\nexport {};\n".to_string());
    }
    let mut imports = Vec::new();
    let mut source = String::new();
    for event in events {
        let name = ts_type(&format!("{}V{}", event.event_type, event.version));
        imports.push(format!("decode{name}"));
        let sample = sample_event_value(&event.payload_schema)?;
        let wire = serde_json::json!({
            "descriptor": {
                "type": event.event_type,
                "version": event.version,
                "schemaDigest": event.schema.digest,
                "delivery": event.delivery,
            },
            "payload": sample,
        });
        let wire = serde_json::to_string(&wire).map_err(CodegenError::Serialize)?;
        source.push_str(&format!(
            "const {name}Valid = {};\ndecode{name}({name}Valid);\nconst {name}DescriptorTampered: any = JSON.parse({name}Valid); {name}DescriptorTampered.descriptor.schemaDigest = 'sha256:tampered';\ntry {{ decode{name}(JSON.stringify({name}DescriptorTampered)); throw new Error('descriptor tampering accepted'); }} catch (error) {{ if (error instanceof Error && error.message === 'descriptor tampering accepted') throw error; }}\nconst {name}PayloadTampered: any = JSON.parse({name}Valid); {name}PayloadTampered.payload.__unknown = true;\ntry {{ decode{name}(JSON.stringify({name}PayloadTampered)); throw new Error('unknown payload field accepted'); }} catch (error) {{ if (error instanceof Error && error.message === 'unknown payload field accepted') throw error; }}\n\n",
            ts_string(&wire),
        ));
    }
    Ok(format!(
        "// Code generated by ojos service. DO NOT EDIT.\nimport {{ {} }} from './events.js';\n\n{source}",
        imports.join(", ")
    ))
}

fn render_ts_payload(event_name: &str, schema: &Value) -> Result<String> {
    let mut declarations = String::new();
    let mut names = BTreeSet::new();
    render_ts_object(
        &format!("{event_name}Payload"),
        schema,
        &mut declarations,
        &mut names,
    )?;
    Ok(declarations)
}

fn render_ts_object(
    name: &str,
    schema: &Value,
    output: &mut String,
    names: &mut BTreeSet<String>,
) -> Result<()> {
    insert_identifier(names, name.to_string(), "TypeScript event payload")?;
    let (properties, required) = schema_properties(schema)?;
    let mut nested = String::new();
    output.push_str(&format!("export interface {name} {{\n"));
    for (property, property_schema) in properties {
        let nested_name = format!("{name}{}", ts_type(property));
        let field_type = ts_schema_type(&nested_name, property_schema, &mut nested, names)?;
        output.push_str(&format!(
            "  readonly {}{}: {field_type};\n",
            ts_string(property),
            if required.contains(property.as_str()) {
                ""
            } else {
                "?"
            }
        ));
    }
    output.push_str("}\n\n");
    output.push_str(&nested);
    Ok(())
}

fn ts_schema_type(
    name: &str,
    schema: &Value,
    nested: &mut String,
    names: &mut BTreeSet<String>,
) -> Result<String> {
    Ok(match schema_kind(schema)? {
        "object" => {
            render_ts_object(name, schema, nested, names)?;
            name.to_string()
        }
        "array" => {
            let items = schema_object(schema)?
                .get("items")
                .ok_or_else(|| CodegenError::Drift("event array needs items".to_string()))?;
            format!(
                "readonly {}[]",
                ts_schema_type(&format!("{name}Item"), items, nested, names)?
            )
        }
        "string"
            if schema_object(schema)?.contains_key("enum")
                || schema_object(schema)?.contains_key("const") =>
        {
            schema_string_values(schema)?
                .into_iter()
                .map(ts_string)
                .collect::<Vec<_>>()
                .join(" | ")
        }
        "string" => "string".to_string(),
        "integer" | "number" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        kind => {
            return Err(CodegenError::Drift(format!(
                "unsupported event type {kind}"
            )));
        }
    })
}

fn render_gozero_api(contract: &ServiceContractV3) -> String {
    let mut source = format!(
        "// Code generated by ojos service. DO NOT EDIT.\nsyntax = \"v1\"\n\ninfo (\n\ttitle: {}\n\tdesc: {}\n\tversion: {}\n)\n\n",
        gozero_string(&contract.display_name),
        gozero_string(&format!("Generated adapter for {}", contract.service_id)),
        gozero_string(&contract.service_version.to_string()),
    );
    for operation in sorted_operations(contract) {
        let type_name = go_exported(&operation.operation_id);
        let mut fields = Vec::new();
        for parameter in &operation.parameters {
            fields.push(format!(
                "\t{} string `{tag}:\"{}{}\"`",
                go_exported(&parameter.name),
                parameter.name,
                if parameter.required { "" } else { ",optional" },
                tag = match parameter.location.as_str() {
                    "path" => "path",
                    "header" => "header",
                    _ => "form",
                },
            ));
        }
        if operation.request_body.is_some() {
            fields.push("\tBody string `json:\"body,optional\"`".to_string());
        }
        if !fields.is_empty() {
            source.push_str(&format!(
                "type {type_name}Request {{\n{}\n}}\n\n",
                fields.join("\n")
            ));
        }
        source.push_str(&format!(
            "type {type_name}Response {{\n\tBody string `json:\"body,optional\"`\n}}\n\n"
        ));
    }
    source.push_str(&format!(
        "@server (\n\tgroup: {}\n)\nservice {}-api {{\n",
        go_package(&contract.service_id),
        go_package(&contract.service_id)
    ));
    for operation in sorted_operations(contract) {
        let name = go_exported(&operation.operation_id);
        let request = if operation.parameters.is_empty() && operation.request_body.is_none() {
            String::new()
        } else {
            format!(" ({name}Request)")
        };
        source.push_str(&format!(
            "\t@handler {name}\n\t{} {}{} returns ({name}Response)\n",
            operation.method.to_ascii_lowercase(),
            operation.provider_path,
            request,
        ));
    }
    source.push_str("}\n");
    source
}

fn render_server_adapter(contract: &ServiceContractV3) -> Result<Vec<u8>> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Adapter<'a> {
        schema_version: &'static str,
        service_id: &'a str,
        operations: Vec<AdapterOperation<'a>>,
    }
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct AdapterOperation<'a> {
        operation_id: &'a str,
        handler: String,
        method: &'a str,
        path: &'a str,
        audience: &'a str,
        auth: &'a str,
        permission: Option<&'a str>,
        request_schema_digests: Vec<&'a str>,
        response_schema_digests: Vec<&'a str>,
    }
    let operations = sorted_operations(contract)
        .into_iter()
        .map(|operation| AdapterOperation {
            operation_id: &operation.operation_id,
            handler: go_exported(&operation.operation_id),
            method: &operation.method,
            path: &operation.provider_path,
            audience: &operation.audience,
            auth: &operation.auth,
            permission: operation.permission.as_deref(),
            request_schema_digests: operation
                .request_body
                .iter()
                .flat_map(|body| &body.content)
                .filter_map(|content| content.schema_digest.as_deref())
                .collect(),
            response_schema_digests: operation
                .responses
                .iter()
                .flat_map(|response| &response.content)
                .filter_map(|content| content.schema_digest.as_deref())
                .collect(),
        })
        .collect();
    let mut bytes = serde_json::to_vec_pretty(&Adapter {
        schema_version: "ojos.dev/gozero-server-adapter/v1",
        service_id: &contract.service_id,
        operations,
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sorted_operations(contract: &ServiceContractV3) -> Vec<&ApiOperationV3> {
    let mut operations = contract.operations.iter().collect::<Vec<_>>();
    operations.sort_by(|left, right| {
        left.api_id
            .cmp(&right.api_id)
            .then(left.provider_path.cmp(&right.provider_path))
            .then(left.method.cmp(&right.method))
            .then(left.operation_id.cmp(&right.operation_id))
    });
    operations
}

fn all_events(contract: &ServiceContractV3) -> Vec<&EventContractV1> {
    let mut by_identity = BTreeMap::new();
    for event in contract
        .events
        .publishes
        .iter()
        .chain(contract.events.subscribes.iter())
    {
        by_identity.insert((event.event_type.as_str(), event.version), event);
    }
    by_identity.into_values().collect()
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn words(value: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            current.push(character);
        } else if !current.is_empty() {
            output.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        output.push(current);
    }
    if output.is_empty() {
        output.push("generated".to_string());
    }
    output
}

fn go_exported(value: &str) -> String {
    let mut output = String::new();
    for word in words(value) {
        let mut characters = word.chars();
        if let Some(first) = characters.next() {
            output.push(first.to_ascii_uppercase());
            output.extend(characters);
        }
    }
    if output.starts_with(|character: char| character.is_ascii_digit()) {
        output.insert_str(0, "Generated");
    }
    output
}

fn go_unexported(value: &str) -> String {
    let mut value = go_exported(value);
    if let Some(first) = value.get_mut(0..1) {
        first.make_ascii_lowercase();
    }
    value
}

fn go_package(value: &str) -> String {
    let mut output = words(value)
        .into_iter()
        .map(|word| word.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("_");
    if output.starts_with(|character: char| character.is_ascii_digit()) {
        output.insert_str(0, "service_");
    }
    output
}

fn rust_const(value: &str) -> String {
    let mut output = words(value)
        .into_iter()
        .map(|word| word.to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join("_");
    if output.starts_with(|character: char| character.is_ascii_digit()) {
        output.insert_str(0, "GENERATED_");
    }
    output
}

fn rust_type(value: &str) -> String {
    let mut output = go_exported(value);
    if is_rust_keyword(&output.to_ascii_lowercase()) {
        output.insert_str(0, "Generated");
    }
    output
}

fn rust_field(value: &str) -> String {
    let words = words(value);
    let mut output = words
        .first()
        .cloned()
        .unwrap_or_else(|| "generated".to_string())
        .to_ascii_lowercase();
    for word in words.iter().skip(1) {
        output.push('_');
        output.push_str(&word.to_ascii_lowercase());
    }
    if output.starts_with(|character: char| character.is_ascii_digit()) {
        output.insert_str(0, "field_");
    }
    if is_rust_keyword(&output) {
        output.insert_str(0, "field_");
    }
    output
}

fn is_rust_keyword(value: &str) -> bool {
    matches!(
        value,
        "as" | "break"
            | "const"
            | "continue"
            | "crate"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
            | "dyn"
            | "abstract"
            | "become"
            | "box"
            | "do"
            | "final"
            | "macro"
            | "override"
            | "priv"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
            | "try"
    )
}

fn rust_crate(value: &str) -> String {
    let mut output = words(value)
        .into_iter()
        .map(|word| word.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("-");
    if output.starts_with(|character: char| character.is_ascii_digit()) {
        output.insert_str(0, "service-");
    }
    output
}

fn npm_name(value: &str) -> String {
    rust_crate(value)
}

fn ts_identifier(value: &str) -> String {
    let mut words = words(value).into_iter();
    let mut output = words
        .next()
        .unwrap_or_else(|| "operation".to_string())
        .to_ascii_lowercase();
    for word in words {
        output.push_str(&go_exported(&word));
    }
    if output.starts_with(|character: char| character.is_ascii_digit())
        || matches!(
            output.as_str(),
            "break" | "case" | "class" | "delete" | "function" | "new" | "return" | "throw"
        )
    {
        output.insert_str(0, "operation");
    }
    output
}

fn ts_type(value: &str) -> String {
    go_exported(value)
}

fn go_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

fn go_header_parameters(operation: &ApiOperationV3) -> String {
    let values = operation
        .parameters
        .iter()
        .filter(|parameter| parameter.location == "header")
        .map(|parameter| {
            format!(
                "{{Name: {}, Required: {}}}",
                go_string(&parameter.name),
                parameter.required
            )
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        "nil".to_string()
    } else {
        format!("[]HeaderParameter{{{}}}", values.join(", "))
    }
}

fn go_request_content_types(operation: &ApiOperationV3) -> String {
    let Some(body) = &operation.request_body else {
        return "nil".to_string();
    };
    if body.content.is_empty() {
        return "nil".to_string();
    }
    format!(
        "[]string{{{}}}",
        body.content
            .iter()
            .map(|media| go_string(&media.media_type))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn rust_string(value: &str) -> String {
    format!("{value:?}")
}

fn rust_option(value: Option<&str>) -> String {
    value
        .map(|value| format!("Some({})", rust_string(value)))
        .unwrap_or_else(|| "None".to_string())
}

fn rust_header_parameters(operation: &ApiOperationV3) -> String {
    let values = operation
        .parameters
        .iter()
        .filter(|parameter| parameter.location == "header")
        .map(|parameter| {
            format!(
                "HeaderParameter {{ name: {}, required: {} }}",
                rust_string(&parameter.name),
                parameter.required
            )
        })
        .collect::<Vec<_>>();
    format!("&[{}]", values.join(", "))
}

fn rust_request_content_types(operation: &ApiOperationV3) -> String {
    let values = operation
        .request_body
        .iter()
        .flat_map(|body| &body.content)
        .map(|media| rust_string(&media.media_type))
        .collect::<Vec<_>>();
    format!("&[{}]", values.join(", "))
}

fn ts_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

fn ts_header_parameters(operation: &ApiOperationV3) -> String {
    let values = operation
        .parameters
        .iter()
        .filter(|parameter| parameter.location == "header")
        .map(|parameter| {
            format!(
                "{{ name: {}, required: {} }}",
                ts_string(&parameter.name),
                parameter.required
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", values.join(", "))
}

fn ts_request_content_types(operation: &ApiOperationV3) -> String {
    let values = operation
        .request_body
        .iter()
        .flat_map(|body| &body.content)
        .map(|media| ts_string(&media.media_type))
        .collect::<Vec<_>>();
    format!("[{}]", values.join(", "))
}

fn gozero_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ApiSurfaceV3, ArtifactFileV1, EventsContractV1, FrontendContractV1, HealthSource,
        RuntimeSource,
    };
    use semver::Version;
    use std::process::Command;
    use tempfile::tempdir;

    fn contract() -> ServiceContractV3 {
        let mut contract = ServiceContractV3 {
            schema_version: "ojos.dev/service-contract/v3".to_string(),
            compiler_version: "0.1.0".to_string(),
            service_id: "contest-service".to_string(),
            service_version: Version::new(1, 2, 3),
            display_name: "Contest".to_string(),
            source_digest: format!("sha256:{}", "1".repeat(64)),
            runtime: RuntimeSource {
                profile: "standard-container-v1".to_string(),
                artifact: "runtime".to_string(),
                http_port: 8080,
                health: HealthSource {
                    path: "/healthz".to_string(),
                },
                volumes: Vec::new(),
            },
            api_surfaces: vec![ApiSurfaceV3 {
                api_id: "contest.api".to_string(),
                version: Version::new(1, 0, 0),
                document: "api/openapi.yaml".to_string(),
                document_digest: format!("sha256:{}", "2".repeat(64)),
            }],
            operations: vec![ApiOperationV3 {
                api_id: "contest.api".to_string(),
                api_version: Version::new(1, 0, 0),
                operation_id: "contest.get".to_string(),
                provider_path: "/contests/{id}".to_string(),
                method: "GET".to_string(),
                audience: "user".to_string(),
                auth: "required".to_string(),
                permission: Some("contest.read".to_string()),
                permission_scope: Some(crate::PermissionScopeV1::system()),
                parameters: Vec::new(),
                request_body: None,
                responses: Vec::new(),
            }],
            api_requirements: Vec::new(),
            package_requirements: Vec::new(),
            resource_claims: Vec::new(),
            migrations: Vec::new(),
            events: EventsContractV1 {
                publishes: vec![EventContractV1 {
                    event_type: "contest.created".to_string(),
                    version: 1,
                    schema: ArtifactFileV1 {
                        path: "events/contest-created-v1.schema.json".to_string(),
                        digest: format!("sha256:{}", "3".repeat(64)),
                    },
                    payload_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "contestId": {"type": "integer"},
                            "ratio": {"type": "number"},
                            "active": {"type": "boolean"},
                            "status": {"enum": ["draft", "ready"]},
                            "kind": {"const": "contest"},
                            "tags": {"type": "array", "items": {"type": "string"}},
                            "owner": {
                                "type": "object",
                                "properties": {
                                    "user-id": {"type": "integer"},
                                    "type": {"type": "string"}
                                },
                                "required": ["user-id"],
                                "additionalProperties": false
                            },
                            "optional-note": {"type": "string"}
                        },
                        "required": ["contestId", "ratio", "active", "status", "kind", "tags", "owner"],
                        "additionalProperties": false
                    }),
                    delivery: "durable".to_string(),
                }],
                subscribes: Vec::new(),
            },
            permissions: Vec::new(),
            permission_references: Vec::new(),
            exposures: Vec::new(),
            routes: Vec::new(),
            frontends: Vec::<FrontendContractV1>::new(),
            config_schema: None,
        };
        let schema = &contract.events.publishes[0].payload_schema;
        contract.events.publishes[0].schema.digest =
            digest(&serde_json_canonicalizer::to_vec(schema).expect("test schema canonicalizes"));
        contract
    }

    #[test]
    fn render_is_byte_for_byte_deterministic_and_has_all_targets() {
        let first = render(&contract()).unwrap();
        let second = render(&contract()).unwrap();
        assert_eq!(first, second);
        for path in [
            "service.contract.json",
            "go/client.go",
            "go/client_test.go",
            "go/events.go",
            "rust/src/client.rs",
            "rust/src/events.rs",
            "ts/src/client.ts",
            "ts/src/client.test.ts",
            "ts/src/events.ts",
            "gozero/service.api",
            "gozero/server-adapter.json",
        ] {
            assert!(first.contains_key(Path::new(path)), "missing {path}");
        }
        assert!(
            first
                .values()
                .all(|bytes| !bytes.windows(4).any(|item| item == b"2026"))
        );
    }

    #[test]
    fn generated_sdk_and_event_sources_are_typed_and_transport_agnostic() {
        let files = render(&contract()).unwrap();
        let go_client = String::from_utf8(files[Path::new("go/client.go")].clone()).unwrap();
        let rust_client =
            String::from_utf8(files[Path::new("rust/src/client.rs")].clone()).unwrap();
        let ts_client = String::from_utf8(files[Path::new("ts/src/client.ts")].clone()).unwrap();
        let go_events = String::from_utf8(files[Path::new("go/events.go")].clone()).unwrap();
        let rust_events =
            String::from_utf8(files[Path::new("rust/src/events.rs")].clone()).unwrap();
        let ts_events = String::from_utf8(files[Path::new("ts/src/events.ts")].clone()).unwrap();

        for client in [&go_client, &rust_client, &ts_client] {
            assert!(client.contains("ContextSnapshot"));
            assert!(client.contains("Idempotency") || client.contains("idempotency"));
            assert!(client.contains("Timeout") || client.contains("timeout"));
            assert!(!client.contains("redis"));
            assert!(!client.contains("broker"));
        }
        for events in [&go_events, &rust_events, &ts_events] {
            assert!(events.contains("EventDescriptor"));
            assert!(events.contains("TypedEvent"));
            assert!(
                events.contains("SchemaDigest")
                    || events.contains("schema_digest")
                    || events.contains("schemaDigest")
            );
            assert!(!events.contains("redis"));
            assert!(!events.contains("stream"));
            assert!(!events.contains("broker"));
        }
        assert!(go_events.contains("ContestCreatedV1PayloadOwner struct"));
        assert!(go_events.contains("ContestCreatedV1PayloadStatus string"));
        assert!(rust_events.contains("pub struct ContestCreatedV1PayloadOwner"));
        assert!(rust_events.contains("pub enum ContestCreatedV1PayloadStatus"));
        assert!(ts_events.contains("export interface ContestCreatedV1PayloadOwner"));
        assert!(ts_events.contains("readonly \"status\": \"draft\" | \"ready\""));
        assert!(ts_events.contains("validateEventValue"));
        assert!(!go_events.contains("json.RawMessage `json:\"data\"`"));
        assert!(!rust_events.contains("pub struct JsonPayload"));
        assert!(!ts_events.contains("return value as"));
    }

    #[test]
    fn rust_optional_deserializer_is_emitted_only_when_payloads_use_it() {
        let mut without_events = contract();
        without_events.events.publishes.clear();
        let rendered = render_rust_events(&without_events).unwrap();
        assert!(!rendered.contains("deserialize_optional_non_null"));
        assert!(!rendered.contains("Deserializer"));

        let mut all_required = contract();
        let schema = &mut all_required.events.publishes[0].payload_schema;
        schema["required"] = serde_json::json!([
            "contestId",
            "ratio",
            "active",
            "status",
            "kind",
            "tags",
            "owner",
            "optional-note"
        ]);
        schema["properties"]["owner"]["required"] = serde_json::json!(["user-id", "type"]);
        all_required.events.publishes[0].schema.digest =
            digest(&serde_json_canonicalizer::to_vec(schema).unwrap());
        let rendered = render_rust_events(&all_required).unwrap();
        assert!(!rendered.contains("deserialize_optional_non_null"));
        assert!(!rendered.contains("Deserializer"));

        let rendered = render_rust_events(&contract()).unwrap();
        assert!(rendered.contains("deserialize_optional_non_null"));
        assert!(rendered.contains("Deserializer"));
    }

    #[test]
    fn embedded_event_schema_digest_tampering_is_rejected() {
        let mut contract = contract();
        contract.events.publishes[0].payload_schema["properties"]["contestId"]["type"] =
            serde_json::json!("string");
        assert!(matches!(
            render(&contract),
            Err(CodegenError::Drift(message)) if message.contains("does not match")
        ));
    }

    #[test]
    fn event_property_identifier_collisions_are_rejected() {
        let mut contract = contract();
        let schema = &mut contract.events.publishes[0].payload_schema;
        schema["properties"]["contest-id"] = serde_json::json!({"type": "integer"});
        let canonical = serde_json_canonicalizer::to_vec(schema).unwrap();
        contract.events.publishes[0].schema.digest = digest(&canonical);
        assert!(matches!(
            render(&contract),
            Err(CodegenError::IdentifierCollision { .. })
        ));
    }

    #[test]
    fn generated_clients_preserve_binary_bodies_declared_headers_and_protect_credentials() {
        let mut contract = contract();
        contract.operations[0].method = "PUT".to_string();
        contract.operations[0].parameters = vec![crate::ParameterContractV3 {
            name: "X-OJOS-Content-Sha256".to_string(),
            location: "header".to_string(),
            required: true,
            schema: serde_json::json!({"type": "string"}),
            schema_digest: format!("sha256:{}", "4".repeat(64)),
        }];
        contract.operations[0].request_body = Some(crate::RequestBodyContractV3 {
            required: true,
            content: vec![crate::MediaSchemaContractV3 {
                media_type: "application/octet-stream".to_string(),
                schema: Some(serde_json::json!({"type": "string", "format": "binary"})),
                schema_digest: Some(format!("sha256:{}", "5".repeat(64))),
            }],
        });
        let files = render(&contract).unwrap();
        let go = String::from_utf8(files[Path::new("go/client.go")].clone()).unwrap();
        let rust = String::from_utf8(files[Path::new("rust/src/client.rs")].clone()).unwrap();
        let ts = String::from_utf8(files[Path::new("ts/src/client.ts")].clone()).unwrap();

        assert!(go.contains("application/octet-stream"));
        assert!(go.contains("X-OJOS-Content-Sha256"));
        assert!(go.contains("RawBody []byte"));
        assert!(go.contains("forbiddenHeader"));
        assert!(rust.contains("application/octet-stream"));
        assert!(rust.contains("X-OJOS-Content-Sha256"));
        assert!(rust.contains("raw_body: Option<Vec<u8>>"));
        assert!(rust.contains("forbidden_header"));
        assert!(ts.contains("application/octet-stream"));
        assert!(ts.contains("X-OJOS-Content-Sha256"));
        assert!(ts.contains("readonly rawBody?: BodyInit"));
        assert!(ts.contains("forbiddenHeader"));
        assert!(ts.contains("jsonMediaType(contentType) ? JSON.stringify(options.body)"));
    }

    #[test]
    fn generated_targets_compile_for_event_only_services() {
        let mut contract = contract();
        contract.operations.clear();
        let root = tempdir().unwrap();
        generate_to(&contract, root.path()).unwrap();

        let go = Command::new("go")
            .args(["test", "./..."])
            .current_dir(root.path().join("go"))
            .output()
            .expect("go must be installed for generated fixture test");
        assert!(
            go.status.success(),
            "event-only generated Go failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&go.stdout),
            String::from_utf8_lossy(&go.stderr)
        );

        let rust = Command::new("cargo")
            .args(["test", "--quiet"])
            .env("CARGO_TARGET_DIR", root.path().join("rust-target"))
            .current_dir(root.path().join("rust"))
            .output()
            .expect("cargo must be installed for generated fixture test");
        assert!(
            rust.status.success(),
            "event-only generated Rust failed:\n{}",
            String::from_utf8_lossy(&rust.stderr)
        );
    }

    #[test]
    fn generate_report_is_stable_and_removes_only_previously_managed_files() {
        let root = tempdir().unwrap();
        let first = generate_to(&contract(), root.path()).unwrap();
        let unrelated = root.path().join("keep.txt");
        fs::write(&unrelated, "keep").unwrap();

        let second = generate_to(&contract(), root.path()).unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "keep");
        assert!(root.path().join("go/client.go").is_file());
        let verified = verify_generated(&contract(), root.path()).unwrap();
        assert_eq!(verified.files, first.files);
    }

    #[test]
    fn verify_rejects_hand_edits_and_a_forged_report() {
        let root = tempdir().unwrap();
        generate_to(&contract(), root.path()).unwrap();

        fs::write(root.path().join("go/client.go"), "hand edited\n").unwrap();
        assert!(matches!(
            verify_generated(&contract(), root.path()),
            Err(CodegenError::Drift(message)) if message.contains("go\\client.go") || message.contains("go/client.go")
        ));

        generate_to(&contract(), root.path()).unwrap();
        let report_path = root.path().join(CODEGEN_REPORT_FILE);
        let mut report: GenerationReport =
            serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
        report.files.pop();
        fs::write(&report_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        assert!(matches!(
            verify_generated(&contract(), root.path()),
            Err(CodegenError::Drift(message)) if message.contains(CODEGEN_REPORT_FILE)
        ));
    }

    #[test]
    fn normalized_identifier_collision_is_rejected() {
        let mut contract = contract();
        let mut duplicate = contract.operations[0].clone();
        duplicate.operation_id = "contest-get".to_string();
        contract.operations.push(duplicate);
        let error = render(&contract).unwrap_err();
        assert!(matches!(error, CodegenError::IdentifierCollision { .. }));
    }

    #[test]
    #[ignore = "toolchain conformance: set OJOS_TSC when tsc is not on PATH"]
    fn generated_go_rust_and_typescript_targets_compile() {
        let root = tempdir().unwrap();
        generate_to(&contract(), root.path()).unwrap();

        let go = Command::new("go")
            .args(["test", "./..."])
            .current_dir(root.path().join("go"))
            .output()
            .expect("go must be installed for conformance test");
        assert!(
            go.status.success(),
            "generated Go failed:\n{}",
            String::from_utf8_lossy(&go.stderr)
        );

        let rust_target = root.path().join("rust-target");
        let rust = Command::new("cargo")
            .args(["test", "--quiet"])
            .env("CARGO_TARGET_DIR", &rust_target)
            .current_dir(root.path().join("rust"))
            .output()
            .expect("cargo must be installed for conformance test");
        assert!(
            rust.status.success(),
            "generated Rust failed:\n{}",
            String::from_utf8_lossy(&rust.stderr)
        );

        let tsc = std::env::var_os("OJOS_TSC").unwrap_or_else(|| "tsc".into());
        let mut command = if cfg!(windows)
            && Path::new(&tsc)
                .extension()
                .is_some_and(|item| item == "cmd")
        {
            let mut command = Command::new("cmd");
            command.arg("/C").arg(&tsc);
            command
        } else {
            Command::new(&tsc)
        };
        let typescript = command
            .args(["--project", "tsconfig.json"])
            .current_dir(root.path().join("ts"))
            .output()
            .expect("tsc must be installed for conformance test");
        assert!(
            typescript.status.success(),
            "generated TypeScript failed:\n{}",
            String::from_utf8_lossy(&typescript.stderr)
        );

        let mut emit_command = if cfg!(windows)
            && Path::new(&tsc)
                .extension()
                .is_some_and(|item| item == "cmd")
        {
            let mut command = Command::new("cmd");
            command.arg("/C").arg(&tsc);
            command
        } else {
            Command::new(&tsc)
        };
        let emitted = emit_command
            .args([
                "--project",
                "tsconfig.json",
                "--noEmit",
                "false",
                "--outDir",
                "dist",
            ])
            .current_dir(root.path().join("ts"))
            .output()
            .expect("tsc must be installed for runtime conformance test");
        assert!(
            emitted.status.success(),
            "generated TypeScript emit failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&emitted.stdout),
            String::from_utf8_lossy(&emitted.stderr)
        );
        let node = Command::new("node")
            .arg("dist/client.test.js")
            .current_dir(root.path().join("ts"))
            .output()
            .expect("node must be installed for runtime conformance test");
        assert!(
            node.status.success(),
            "generated TypeScript runtime test failed:\n{}",
            String::from_utf8_lossy(&node.stderr)
        );
        let event_node = Command::new("node")
            .arg("dist/events.test.js")
            .current_dir(root.path().join("ts"))
            .output()
            .expect("node must be installed for event runtime conformance test");
        assert!(
            event_node.status.success(),
            "generated TypeScript event runtime test failed:\n{}",
            String::from_utf8_lossy(&event_node.stderr)
        );
    }
}
