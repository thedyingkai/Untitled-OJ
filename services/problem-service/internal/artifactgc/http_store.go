package artifactgc

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"

	"ojos-shared/servicecontext"
	"ojos-shared/storagecontract"
)

const (
	storageHeadBinding   = "storage.object.head"
	storageDeleteBinding = "storage.object.delete"
)

// BoundObjectStore addresses Storage only through Agent-materialized
// ApiBindings and reloads the workload token for every request. It contains no
// provider URL or management credential.
type BoundObjectStore struct {
	Context  servicecontext.ServiceContext
	Provider *servicecontext.ContextProvider
	Client   *http.Client
	Bucket   string
}

func NewBoundObjectStore(bucket string) (*BoundObjectStore, error) {
	value, err := servicecontext.LoadOptional()
	if err != nil {
		return nil, err
	}
	if value == nil {
		return nil, errors.New("managed artifact GC requires a Service Context")
	}
	if err := value.RequireService("problem-service"); err != nil {
		return nil, err
	}
	for _, name := range []string{storageHeadBinding, storageDeleteBinding} {
		if _, err := exactStorageBinding(*value, name); err != nil {
			return nil, fmt.Errorf("artifact GC: %w", err)
		}
	}
	path := strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE"))
	if path == "" {
		path = servicecontext.DefaultFile
	}
	provider, err := servicecontext.NewContextProvider(path, servicecontext.ProviderOptions{})
	if err != nil {
		return nil, err
	}
	bucket = strings.TrimSpace(bucket)
	if bucket == "" {
		return nil, errors.New("artifact GC bucket is required")
	}
	return &BoundObjectStore{Context: *value, Provider: provider, Bucket: bucket}, nil
}

func (s BoundObjectStore) Inspect(ctx context.Context, intent Intent) (Object, bool, error) {
	path, err := s.relativePath(intent)
	if err != nil {
		return Object{}, false, err
	}
	snapshot, client, err := s.snapshot(ctx)
	if err != nil {
		return Object{}, false, err
	}
	resp, err := snapshot.Do(ctx, client, storageHeadBinding, http.MethodHead, path, nil)
	if err != nil {
		return Object{}, false, err
	}
	defer resp.Body.Close()
	if resp.StatusCode == http.StatusNotFound {
		if strings.EqualFold(strings.TrimSpace(resp.Header.Get(storagecontract.ResultHeader)), storagecontract.ResultObjectNotFound) {
			return Object{}, false, nil
		}
		return Object{}, false, NewProviderHTTPError("bound Storage HEAD", resp.StatusCode, fmt.Sprintf("missing authoritative %s=%s evidence", storagecontract.ResultHeader, storagecontract.ResultObjectNotFound))
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
		return Object{}, false, NewProviderHTTPError("bound Storage HEAD", resp.StatusCode, strings.TrimSpace(string(body)))
	}
	if !strings.EqualFold(strings.TrimSpace(resp.Header.Get(storagecontract.ResultHeader)), storagecontract.ResultPresent) {
		return Object{}, false, &ProviderContractError{
			Operation: "bound Storage HEAD",
			Result:    "INVALID_" + storagecontract.ResultHeader,
		}
	}
	return Object{
		Key:       intent.Key,
		SHA256:    strings.ToLower(strings.TrimSpace(resp.Header.Get("X-OJOS-Object-Sha256"))),
		SizeBytes: resp.ContentLength,
	}, true, nil
}

func (s BoundObjectStore) DeleteIfMatches(ctx context.Context, intent Intent) error {
	path, err := s.relativePath(intent)
	if err != nil {
		return err
	}
	headers := http.Header{
		"X-OJOS-Expected-Sha256": []string{intent.SHA256},
		"X-OJOS-Expected-Size":   []string{strconv.FormatInt(intent.SizeBytes, 10)},
	}
	snapshot, client, err := s.snapshot(ctx)
	if err != nil {
		return err
	}
	resp, err := snapshot.DoWithOptions(ctx, client, storageDeleteBinding, http.MethodDelete, path, nil, servicecontext.RequestOptions{Headers: headers, ContentLength: 0})
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
		return NewProviderHTTPError("bound conditional Storage DELETE", resp.StatusCode, strings.TrimSpace(string(body)))
	}
	if !strings.EqualFold(strings.TrimSpace(resp.Header.Get(storagecontract.ResultHeader)), storagecontract.ResultDeleted) {
		return &ProviderContractError{
			Operation: "bound conditional Storage DELETE",
			Result:    "INVALID_" + storagecontract.ResultHeader,
		}
	}
	// A successful HTTP status alone is not authoritative: a misrouted Gateway
	// endpoint can also return 200. Storage's conditional DELETE contract must
	// explicitly attest that the object was deleted before the durable ledger is
	// advanced to COMPLETED.
	body, err := io.ReadAll(io.LimitReader(resp.Body, 4097))
	if err != nil || len(body) > 4096 {
		return &ProviderContractError{
			Operation: "bound conditional Storage DELETE",
			Result:    "INVALID_DELETE_RESPONSE",
		}
	}
	var result struct {
		Deleted *bool `json:"deleted"`
	}
	if err := json.Unmarshal(body, &result); err != nil || result.Deleted == nil || !*result.Deleted {
		return &ProviderContractError{
			Operation: "bound conditional Storage DELETE",
			Result:    "INVALID_DELETE_RESPONSE",
		}
	}
	return nil
}

func (s BoundObjectStore) DeleteBindingTimeout() (time.Duration, error) {
	snapshot := s.Context
	if s.Provider != nil {
		_ = s.Provider.ReloadNow()
		var err error
		snapshot, err = s.Provider.Current(context.Background())
		if err != nil {
			return 0, err
		}
	}
	binding, err := exactStorageBinding(snapshot, storageDeleteBinding)
	if err != nil {
		return 0, fmt.Errorf("artifact GC: %w", err)
	}
	timeout := time.Duration(binding.TimeoutMS) * time.Millisecond
	if timeout <= 0 {
		return 0, errors.New("artifact GC storage.object.delete binding timeout is invalid")
	}
	return timeout, nil
}

func (s BoundObjectStore) Close() error {
	if s.Provider != nil {
		return s.Provider.Close()
	}
	return nil
}

func (s BoundObjectStore) snapshot(ctx context.Context) (servicecontext.ServiceContext, *http.Client, error) {
	snapshot := s.Context
	client := s.Client
	if s.Provider != nil {
		_ = s.Provider.ReloadNow()
		var err error
		snapshot, err = s.Provider.Current(ctx)
		if err != nil {
			return servicecontext.ServiceContext{}, nil, err
		}
		if err := snapshot.RequireService("problem-service"); err != nil {
			return servicecontext.ServiceContext{}, nil, err
		}
		client, err = snapshot.Client()
		if err != nil {
			return servicecontext.ServiceContext{}, nil, err
		}
	}
	if client == nil {
		var err error
		client, err = snapshot.Client()
		if err != nil {
			return servicecontext.ServiceContext{}, nil, err
		}
	}
	for _, name := range []string{storageHeadBinding, storageDeleteBinding} {
		if _, err := exactStorageBinding(snapshot, name); err != nil {
			return servicecontext.ServiceContext{}, nil, fmt.Errorf("artifact GC: %w", err)
		}
	}
	return snapshot, client, nil
}

func exactStorageBinding(snapshot servicecontext.ServiceContext, requirement string) (servicecontext.APIBinding, error) {
	binding, err := snapshot.Binding(requirement)
	if err != nil {
		return servicecontext.APIBinding{}, err
	}
	if binding.APIID != requirement {
		return servicecontext.APIBinding{}, fmt.Errorf(
			"binding %s resolves unexpected API %s",
			requirement,
			binding.APIID,
		)
	}
	return binding, nil
}

func (s BoundObjectStore) relativePath(intent Intent) (string, error) {
	expectedPrefix := "storage://" + s.Bucket + "/"
	if !strings.HasPrefix(intent.URI, expectedPrefix) || intent.Key == "" || strings.Contains(intent.Key, "/") {
		return "", errors.New("artifact intent is outside the configured bucket or key contract")
	}
	return "/" + url.PathEscape(s.Bucket) + "/" + url.PathEscape(intent.Key), nil
}
