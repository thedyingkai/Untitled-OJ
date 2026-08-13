package contributionprojection

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"sort"
	"strings"
	"sync"
	"time"

	"ojos-auth-service/internal/repository"
)

const (
	snapshotSchema             = "ojos.dev/contribution-snapshot/v1"
	acknowledgementSchema      = "ojos.dev/contribution-projection-ack/v1"
	acknowledgementPath        = "/api/v1/contributions/projections:ack"
	maximumBody                = 8 * 1024 * 1024
	maximumAcknowledgementBody = 1024 * 1024
)

type PermissionDefinition struct {
	ServiceID   string `json:"service_id"`
	RevisionID  string `json:"revision_id"`
	Generation  uint64 `json:"generation"`
	Key         string `json:"key"`
	Title       string `json:"title"`
	Description string `json:"description"`
}

type Snapshot struct {
	SchemaVersion         string                 `json:"schema_version"`
	Digest                string                 `json:"digest"`
	ScopeID               string                 `json:"scope_id"`
	Acknowledgements      []Acknowledgement      `json:"acknowledgements"`
	PermissionDefinitions []PermissionDefinition `json:"permission_definitions"`
}

type Acknowledgement struct {
	ActivationID        string  `json:"activation_id"`
	ServiceID           string  `json:"service_id"`
	CandidateRevisionID string  `json:"candidate_revision_id"`
	CandidateGeneration uint64  `json:"candidate_generation"`
	ExpectedState       string  `json:"expected_state"`
	ObservedRevisionID  *string `json:"observed_revision_id"`
	ObservedGeneration  *uint64 `json:"observed_generation"`
}

type envelope struct {
	Data Snapshot        `json:"data"`
	Meta json.RawMessage `json:"meta"`
}

type DefinitionStore interface {
	ReconcileContributionPermissions(context.Context, string, []repository.ContributionPermissionDefinitionInput) error
}

type Reconciler struct {
	endpoint string
	token    string
	ackToken string
	store    DefinitionStore
	client   *http.Client

	mu          sync.Mutex
	reconcileMu sync.Mutex
	pending     *Snapshot
	cancel      context.CancelFunc
	done        chan struct{}
}

func New(endpoint, token, acknowledgementToken string, store DefinitionStore) (*Reconciler, error) {
	endpoint = strings.TrimRight(strings.TrimSpace(endpoint), "/")
	token = strings.TrimSpace(token)
	acknowledgementToken = strings.TrimSpace(acknowledgementToken)
	if endpoint == "" && token == "" {
		return nil, nil
	}
	parsed, err := url.Parse(endpoint)
	if err != nil || parsed.Scheme == "" || parsed.Host == "" || parsed.User != nil || parsed.RawQuery != "" || parsed.Fragment != "" {
		return nil, errors.New("Auth Contribution projection requires an absolute Orchestrator endpoint")
	}
	if token == "" || store == nil {
		return nil, errors.New("Auth Contribution projection requires an internal token and durable store")
	}
	return &Reconciler{
		endpoint: endpoint,
		token:    token,
		ackToken: acknowledgementToken,
		store:    store,
		client:   &http.Client{Timeout: 5 * time.Second},
	}, nil
}

func (r *Reconciler) Reconcile(ctx context.Context) error {
	if r == nil {
		return nil
	}
	r.reconcileMu.Lock()
	defer r.reconcileMu.Unlock()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, r.endpoint+"/api/v1/contributions/snapshot", nil)
	if err != nil {
		return err
	}
	req.Header.Set("x-ojos-orchestrator-token", r.token)
	resp, err := r.client.Do(req)
	if err != nil {
		return fmt.Errorf("fetch Contribution snapshot: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("fetch Contribution snapshot: Orchestrator returned %s", resp.Status)
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, maximumBody+1))
	if err != nil {
		return fmt.Errorf("read Contribution snapshot: %w", err)
	}
	if len(data) > maximumBody {
		return errors.New("Contribution snapshot exceeds the configured limit")
	}
	var payload envelope
	if err := json.Unmarshal(data, &payload); err != nil {
		return fmt.Errorf("decode Contribution snapshot: %w", err)
	}
	if payload.Data.SchemaVersion != snapshotSchema || !canonicalSHA256(payload.Data.Digest) || strings.TrimSpace(payload.Data.ScopeID) == "" {
		return errors.New("Contribution snapshot identity is invalid")
	}
	if err := validateAcknowledgements(payload.Data.Acknowledgements); err != nil {
		return err
	}
	definitions := make([]repository.ContributionPermissionDefinitionInput, 0, len(payload.Data.PermissionDefinitions))
	seen := make(map[string]struct{}, len(payload.Data.PermissionDefinitions))
	for _, item := range payload.Data.PermissionDefinitions {
		if strings.TrimSpace(item.ServiceID) == "" || strings.TrimSpace(item.Key) == "" || item.Generation == 0 || !canonicalSHA256(item.RevisionID) {
			return errors.New("Contribution permission definition is invalid")
		}
		if _, duplicate := seen[item.Key]; duplicate {
			return fmt.Errorf("Contribution permission %s is duplicated", item.Key)
		}
		seen[item.Key] = struct{}{}
		definitions = append(definitions, repository.ContributionPermissionDefinitionInput{
			Code:        item.Key,
			ServiceCode: item.ServiceID,
			Title:       item.Title,
			Description: item.Description,
		})
	}
	sort.Slice(definitions, func(i, j int) bool { return definitions[i].Code < definitions[j].Code })
	if err := r.store.ReconcileContributionPermissions(ctx, payload.Data.Digest, definitions); err != nil {
		return err
	}
	if r.ackToken == "" {
		return nil
	}
	pending := payload.Data
	r.pending = &pending
	if err := r.acknowledge(ctx, pending); err != nil {
		return err
	}
	if r.pending != nil && r.pending.Digest == pending.Digest {
		r.pending = nil
	}
	return nil
}

func (r *Reconciler) acknowledge(ctx context.Context, snapshot Snapshot) error {
	body, err := json.Marshal(map[string]any{
		"schema_version":   acknowledgementSchema,
		"target":           "AUTH",
		"scope_id":         snapshot.ScopeID,
		"snapshot_digest":  snapshot.Digest,
		"acknowledgements": snapshot.Acknowledgements,
	})
	if err != nil {
		return fmt.Errorf("encode Contribution acknowledgement: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, r.endpoint+acknowledgementPath, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("create Contribution acknowledgement: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	req.Header.Set("x-ojos-orchestrator-token", r.token)
	req.Header.Set("x-ojos-contribution-ack-token", r.ackToken)
	req.Header.Set("Idempotency-Key", "contribution-projection-ack:AUTH:"+snapshot.Digest)
	resp, err := r.client.Do(req)
	if err != nil {
		return fmt.Errorf("send Contribution acknowledgement: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("Contribution acknowledgement: Orchestrator returned %s", resp.Status)
	}
	if mediaType := strings.TrimSpace(strings.Split(resp.Header.Get("Content-Type"), ";")[0]); mediaType != "application/json" {
		return fmt.Errorf("Contribution acknowledgement returned unsupported Content-Type %q", mediaType)
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, maximumAcknowledgementBody+1))
	if err != nil {
		return fmt.Errorf("read Contribution acknowledgement: %w", err)
	}
	if len(data) > maximumAcknowledgementBody {
		return errors.New("Contribution acknowledgement exceeds the configured limit")
	}
	var response struct {
		Data struct {
			SchemaVersion  string `json:"schema_version"`
			Target         string `json:"target"`
			ScopeID        string `json:"scope_id"`
			SnapshotDigest string `json:"snapshot_digest"`
			Accepted       bool   `json:"accepted"`
		} `json:"data"`
		Meta json.RawMessage `json:"meta"`
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&response); err != nil {
		return fmt.Errorf("decode Contribution acknowledgement: %w", err)
	}
	var trailing any
	if err := decoder.Decode(&trailing); !errors.Is(err, io.EOF) {
		if err == nil {
			return errors.New("Contribution acknowledgement contains trailing JSON")
		}
		return fmt.Errorf("decode Contribution acknowledgement trailer: %w", err)
	}
	if response.Data.SchemaVersion != acknowledgementSchema || response.Data.Target != "AUTH" || response.Data.ScopeID != snapshot.ScopeID || response.Data.SnapshotDigest != snapshot.Digest || !response.Data.Accepted {
		return errors.New("Contribution acknowledgement response identity is invalid")
	}
	return nil
}

func validateAcknowledgements(items []Acknowledgement) error {
	for _, item := range items {
		if strings.TrimSpace(item.ActivationID) == "" || strings.TrimSpace(item.ServiceID) == "" || !canonicalSHA256(item.CandidateRevisionID) || item.CandidateGeneration == 0 || (item.ExpectedState != "ACTIVE" && item.ExpectedState != "RESTORED") {
			return errors.New("Contribution acknowledgement obligation is invalid")
		}
		if item.ObservedRevisionID != nil && !canonicalSHA256(*item.ObservedRevisionID) {
			return errors.New("Contribution acknowledgement observed revision is invalid")
		}
		if item.ObservedGeneration != nil && *item.ObservedGeneration == 0 {
			return errors.New("Contribution acknowledgement observed generation is invalid")
		}
	}
	return nil
}

func (r *Reconciler) Start(interval time.Duration, onError func(error)) {
	if r == nil || interval <= 0 {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.cancel != nil {
		return
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	r.cancel, r.done = cancel, done
	go func() {
		defer close(done)
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				if err := r.Reconcile(ctx); err != nil && onError != nil {
					onError(err)
				}
			}
		}
	}()
}

func (r *Reconciler) Close() {
	if r == nil {
		return
	}
	r.mu.Lock()
	cancel, done := r.cancel, r.done
	r.cancel, r.done = nil, nil
	r.mu.Unlock()
	if cancel != nil {
		cancel()
	}
	if done != nil {
		<-done
	}
}

func canonicalSHA256(value string) bool {
	if len(value) != len("sha256:")+64 || !strings.HasPrefix(value, "sha256:") {
		return false
	}
	for _, char := range value[len("sha256:"):] {
		if !(char >= '0' && char <= '9' || char >= 'a' && char <= 'f') {
			return false
		}
	}
	return true
}
