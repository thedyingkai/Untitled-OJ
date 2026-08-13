package proxy

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
)

const (
	extensionArtifactPrefix   = "/__ojos/extensions"
	maxExtensionArtifactBytes = 16 << 20
)

type extensionArtifactKey struct {
	digest   string
	artifact string
}

type extensionArtifactSpec struct {
	key       extensionArtifactKey
	reference string
	moduleID  string
	revision  string
}

type extensionArtifactRegistry struct {
	mu       sync.RWMutex
	active   map[extensionArtifactKey]extensionArtifactSpec
	cache    map[extensionArtifactKey][]byte
	inflight map[extensionArtifactKey]*extensionArtifactFetch
	client   *http.Client
}

type extensionArtifactFetch struct {
	done chan struct{}
	data []byte
	err  error
}

func newExtensionArtifactRegistry() *extensionArtifactRegistry {
	return &extensionArtifactRegistry{
		active:   make(map[extensionArtifactKey]extensionArtifactSpec),
		cache:    make(map[extensionArtifactKey][]byte),
		inflight: make(map[extensionArtifactKey]*extensionArtifactFetch),
		client: &http.Client{
			Timeout: 20 * time.Second,
			CheckRedirect: func(request *http.Request, via []*http.Request) error {
				return http.ErrUseLastResponse
			},
		},
	}
}

func (r *extensionArtifactRegistry) replace(snapshot orchestratorsnapshot.ContributionSnapshot) error {
	next, err := compileExtensionArtifacts(snapshot)
	if err != nil {
		return err
	}
	r.replaceCompiled(next)
	return nil
}

func (r *extensionArtifactRegistry) replaceCompiled(next map[extensionArtifactKey]extensionArtifactSpec) {
	r.mu.Lock()
	r.active = next
	for key := range r.cache {
		if _, active := next[key]; !active {
			delete(r.cache, key)
		}
	}
	r.mu.Unlock()
}

func compileExtensionArtifacts(snapshot orchestratorsnapshot.ContributionSnapshot) (map[extensionArtifactKey]extensionArtifactSpec, error) {
	next := make(map[extensionArtifactKey]extensionArtifactSpec)
	modules := append(append([]orchestratorsnapshot.ContributionFrontendModule(nil), snapshot.UserFrontendModules...), snapshot.AdminFrontendModules...)
	if len(modules) > 256 {
		return nil, fmt.Errorf("contribution snapshot exceeds the frontend artifact limit")
	}
	for _, module := range modules {
		if !module.Enabled {
			continue
		}
		digest := strings.ToLower(strings.TrimSpace(module.BundleDigest))
		artifact := strings.TrimSpace(module.Artifact)
		reference := strings.TrimSpace(module.BundleReference)
		manifestDigest := strings.ToLower(strings.TrimSpace(module.ManifestDigest))
		manifestReference := strings.TrimSpace(module.ManifestReference)
		if !validSHA256Digest(digest) || !validArtifactPath(artifact) || !validHTTPSContentAddress(reference, digest) ||
			!validSHA256Digest(manifestDigest) || !validHTTPSContentAddress(manifestReference, manifestDigest) {
			return nil, fmt.Errorf("frontend module %s has invalid signed artifact metadata", module.ModuleID)
		}
		key := extensionArtifactKey{digest: digest, artifact: artifact}
		if existing, found := next[key]; found && existing.reference != reference {
			return nil, fmt.Errorf("frontend artifact %s %s has conflicting references", digest, artifact)
		}
		next[key] = extensionArtifactSpec{key: key, reference: reference, moduleID: module.ModuleID, revision: module.RevisionID}
	}
	return next, nil
}

func (r *extensionArtifactRegistry) serve(w http.ResponseWriter, request *http.Request) bool {
	digest, artifact, ok := extensionArtifactRequest(request.URL.Path)
	if !ok {
		return false
	}
	key := extensionArtifactKey{digest: digest, artifact: artifact}
	r.mu.RLock()
	spec, active := r.active[key]
	cached := append([]byte(nil), r.cache[key]...)
	r.mu.RUnlock()
	if !active {
		http.NotFound(w, request)
		return true
	}
	if request.Method != http.MethodGet && request.Method != http.MethodHead {
		w.Header().Set("Allow", "GET, HEAD")
		writeJSONError(w, http.StatusMethodNotAllowed, 40501, "extension artifact method is not allowed")
		return true
	}
	if len(cached) == 0 {
		var err error
		cached, err = r.load(request.Context(), spec)
		if err != nil {
			writeJSONError(w, http.StatusBadGateway, 50202, "verified extension artifact is unavailable")
			return true
		}
	}
	w.Header().Set("Content-Type", "text/javascript; charset=utf-8")
	w.Header().Set("Cache-Control", "private, max-age=31536000, immutable")
	w.Header().Set("ETag", `"`+digest+`"`)
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.Header().Set("Content-Length", fmt.Sprintf("%d", len(cached)))
	if request.Method == http.MethodHead {
		w.WriteHeader(http.StatusOK)
		return true
	}
	_, _ = w.Write(cached)
	return true
}

func (r *extensionArtifactRegistry) load(ctx context.Context, spec extensionArtifactSpec) ([]byte, error) {
	r.mu.Lock()
	if data := r.cache[spec.key]; len(data) > 0 {
		cloned := append([]byte(nil), data...)
		r.mu.Unlock()
		return cloned, nil
	}
	if current := r.inflight[spec.key]; current != nil {
		r.mu.Unlock()
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-current.done:
			return append([]byte(nil), current.data...), current.err
		}
	}
	fetch := &extensionArtifactFetch{done: make(chan struct{})}
	r.inflight[spec.key] = fetch
	r.mu.Unlock()

	fetch.data, fetch.err = r.download(ctx, spec)
	r.mu.Lock()
	delete(r.inflight, spec.key)
	if fetch.err == nil {
		if active, ok := r.active[spec.key]; ok && active.reference == spec.reference {
			r.cache[spec.key] = append([]byte(nil), fetch.data...)
		} else {
			fetch.data = nil
			fetch.err = fmt.Errorf("extension artifact revision retired while downloading")
		}
	}
	close(fetch.done)
	r.mu.Unlock()
	return append([]byte(nil), fetch.data...), fetch.err
}

func (r *extensionArtifactRegistry) download(ctx context.Context, spec extensionArtifactSpec) ([]byte, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, spec.reference, nil)
	if err != nil {
		return nil, err
	}
	response, err := r.client.Do(request)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("artifact source returned %s", response.Status)
	}
	limited := io.LimitReader(response.Body, maxExtensionArtifactBytes+1)
	data, err := io.ReadAll(limited)
	if err != nil {
		return nil, err
	}
	if len(data) == 0 || len(data) > maxExtensionArtifactBytes {
		return nil, fmt.Errorf("artifact size is outside the allowed range")
	}
	sum := sha256.Sum256(data)
	actual := "sha256:" + hex.EncodeToString(sum[:])
	if actual != spec.key.digest {
		return nil, fmt.Errorf("artifact digest mismatch")
	}
	return data, nil
}

func extensionArtifactRequest(path string) (string, string, bool) {
	prefix := extensionArtifactPrefix + "/"
	if !strings.HasPrefix(path, prefix) {
		return "", "", false
	}
	rest := strings.TrimPrefix(path, prefix)
	digest, artifact, found := strings.Cut(rest, "/")
	digest = strings.ToLower(strings.TrimSpace(digest))
	if !found || !validSHA256Hex(digest) || !validArtifactPath(artifact) {
		return "", "", false
	}
	return "sha256:" + digest, artifact, true
}

func validSHA256Digest(digest string) bool {
	if !strings.HasPrefix(digest, "sha256:") || len(digest) != len("sha256:")+64 {
		return false
	}
	return validSHA256Hex(strings.TrimPrefix(digest, "sha256:"))
}

func validSHA256Hex(digest string) bool {
	if len(digest) != 64 {
		return false
	}
	_, err := hex.DecodeString(digest)
	return err == nil
}

func validArtifactPath(artifact string) bool {
	if artifact == "" || len(artifact) > 1024 || strings.HasPrefix(artifact, "/") || strings.ContainsAny(artifact, "\\?#") {
		return false
	}
	for _, segment := range strings.Split(artifact, "/") {
		if segment == "" || segment == "." || segment == ".." {
			return false
		}
		for _, char := range segment {
			if !(char >= 'a' && char <= 'z' || char >= 'A' && char <= 'Z' || char >= '0' && char <= '9' || strings.ContainsRune("._-", char)) {
				return false
			}
		}
	}
	return true
}

func validHTTPSContentAddress(reference string, digest string) bool {
	parsed, err := url.Parse(reference)
	if err != nil || parsed.Scheme != "https" || parsed.Host == "" || parsed.User != nil || parsed.Fragment != "" {
		return false
	}
	return strings.Contains(strings.ToLower(reference), strings.TrimPrefix(digest, "sha256:"))
}
