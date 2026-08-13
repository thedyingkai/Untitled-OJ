package servicecontext

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

func writeProviderContext(t *testing.T, path string, value ServiceContext) {
	t.Helper()
	bytes, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	temporary := filepath.Join(filepath.Dir(path), fmt.Sprintf(".context-%d.tmp", time.Now().UnixNano()))
	if err := os.WriteFile(temporary, bytes, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(temporary, path); err != nil {
		t.Fatal(err)
	}
}

func providerFixture(t *testing.T, generation uint64, bindings map[string]APIBinding) (string, ServiceContext) {
	t.Helper()
	root := t.TempDir()
	token := filepath.Join(root, "token")
	if err := os.WriteFile(token, []byte("token-one"), 0o600); err != nil {
		t.Fatal(err)
	}
	value := ServiceContext{
		SchemaVersion: 1,
		Deployment: DeploymentIdentity{
			ID: "deployment-a", Service: "consumer", Node: "node-a",
		},
		Gateway:        GatewayContext{Origin: "http://127.0.0.1:8080"},
		Bindings:       bindings,
		CredentialFile: token,
		Generation:     generation,
	}
	path := filepath.Join(root, "context.json")
	writeProviderContext(t, path, value)
	return path, value
}

func binding(id, api string) APIBinding {
	return APIBinding{BindingID: id, APIID: api, BasePath: "/internal/apis/" + api, TimeoutMS: 1_000}
}

func waitGeneration(t *testing.T, provider *ContextProvider, generation uint64) ServiceContext {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		current, err := provider.Current(context.Background())
		if err != nil {
			t.Fatal(err)
		}
		if current.Generation == generation {
			return current
		}
		time.Sleep(time.Millisecond)
	}
	t.Fatalf("provider did not reach generation %d", generation)
	return ServiceContext{}
}

func TestContextProviderReloadsAddRemoveAndProviderChange(t *testing.T) {
	path, value := providerFixture(t, 1, map[string]APIBinding{})
	provider, err := NewContextProvider(path, ProviderOptions{PollInterval: 2 * time.Millisecond})
	if err != nil {
		t.Fatal(err)
	}
	if err := provider.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	defer provider.Close()

	if _, err := provider.Binding(context.Background(), "storage"); !IsBindingUnavailable(err) {
		t.Fatalf("missing optional binding error = %v", err)
	}

	value.Generation = 2
	value.Bindings["storage"] = binding("binding-a", "storage.object.get")
	writeProviderContext(t, path, value)
	got := waitGeneration(t, provider, 2)
	if got.Bindings["storage"].BindingID != "binding-a" {
		t.Fatalf("binding add was not observed: %#v", got.Bindings)
	}

	value.Generation = 3
	value.Bindings["storage"] = binding("binding-b", "storage.object.v2.get")
	writeProviderContext(t, path, value)
	got = waitGeneration(t, provider, 3)
	if got.Bindings["storage"].BindingID != "binding-b" || got.Bindings["storage"].APIID != "storage.object.v2.get" {
		t.Fatalf("provider change was not observed: %#v", got.Bindings["storage"])
	}

	value.Generation = 4
	delete(value.Bindings, "storage")
	writeProviderContext(t, path, value)
	waitGeneration(t, provider, 4)
	_, err = provider.Binding(context.Background(), "storage")
	var unavailable *BindingUnavailable
	if !errors.As(err, &unavailable) || unavailable.Generation != 4 || unavailable.Name != "storage" {
		t.Fatalf("binding removal error = %#v, %v", unavailable, err)
	}
}

func TestContextProviderKeepsLastKnownGoodOnInvalidPartialAndRegression(t *testing.T) {
	path, value := providerFixture(t, 7, map[string]APIBinding{"api": binding("one", "fixture.api")})
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}

	if err := os.WriteFile(path, []byte(`{"schema_version":1,"generation":8`), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := provider.ReloadNow(); err == nil {
		t.Fatal("partial write was accepted")
	}
	current, _ := provider.Current(context.Background())
	if current.Generation != 7 || current.Bindings["api"].BindingID != "one" {
		t.Fatalf("partial write replaced last-known-good: %#v", current)
	}

	value.Generation = 6
	value.Bindings["api"] = binding("older", "fixture.api")
	writeProviderContext(t, path, value)
	if err := provider.ReloadNow(); err == nil || !strings.Contains(err.Error(), "generation regression") {
		t.Fatalf("generation regression error = %v", err)
	}
	current, _ = provider.Current(context.Background())
	if current.Generation != 7 || current.Bindings["api"].BindingID != "one" {
		t.Fatalf("generation regression replaced last-known-good: %#v", current)
	}
}

func TestContextProviderKeepsLastKnownGoodWhenFileDisappears(t *testing.T) {
	path, _ := providerFixture(t, 3, map[string]APIBinding{"api": binding("one", "fixture.api")})
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	if err := provider.ReloadNow(); err == nil || !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("missing file error = %v", err)
	}
	current, err := provider.Current(context.Background())
	if err != nil || current.Generation != 3 {
		t.Fatalf("missing file discarded last-known-good: %#v, %v", current, err)
	}
}

func TestContextProviderRecoversAfterInvalidFileIsReplaced(t *testing.T) {
	path, value := providerFixture(t, 1, map[string]APIBinding{})
	provider, err := NewContextProvider(path, ProviderOptions{PollInterval: 2 * time.Millisecond})
	if err != nil {
		t.Fatal(err)
	}
	if err := provider.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	defer provider.Close()
	if err := os.WriteFile(path, []byte("{"), 0o600); err != nil {
		t.Fatal(err)
	}
	time.Sleep(15 * time.Millisecond)
	current, _ := provider.Current(context.Background())
	if current.Generation != 1 {
		t.Fatalf("invalid file replaced last-known-good: %#v", current)
	}
	value.Generation = 2
	value.Bindings["api"] = binding("two", "fixture.api")
	writeProviderContext(t, path, value)
	current = waitGeneration(t, provider, 2)
	if current.Bindings["api"].BindingID != "two" {
		t.Fatalf("provider did not recover after valid replacement: %#v", current)
	}
}

func TestContextProviderDetectsAtomicReplacementWithPreservedTimestampAndSize(t *testing.T) {
	path, value := providerFixture(t, 1, map[string]APIBinding{"api": binding("provider-a", "fixture.api")})
	original, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	value.Generation = 2
	value.Bindings["api"] = binding("provider-b", "fixture.api")
	bytes, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	if int64(len(bytes)) != original.Size() {
		t.Skip("fixture did not retain identical serialized size")
	}
	temporary := filepath.Join(filepath.Dir(path), ".same-metadata-context")
	if err := os.WriteFile(temporary, bytes, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(temporary, original.ModTime(), original.ModTime()); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(temporary, path); err != nil {
		t.Fatal(err)
	}
	if err := provider.ReloadNow(); err != nil {
		t.Fatal(err)
	}
	current, _ := provider.Current(context.Background())
	if current.Generation != 2 || current.Bindings["api"].BindingID != "provider-b" {
		t.Fatalf("atomic file identity replacement was missed: %#v", current)
	}
}

func TestContextProviderRejectsSameGenerationDifferentContent(t *testing.T) {
	path, value := providerFixture(t, 2, map[string]APIBinding{"api": binding("one", "fixture.api")})
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	value.Bindings["api"] = binding("two", "fixture.api")
	writeProviderContext(t, path, value)
	if err := provider.ReloadNow(); err == nil || !strings.Contains(err.Error(), "reused with different content") {
		t.Fatalf("same generation mutation error = %v", err)
	}
}

func TestContextProviderSubscriptionCoalescesAndCloses(t *testing.T) {
	path, value := providerFixture(t, 1, map[string]APIBinding{})
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	updates := provider.Subscribe(ctx)
	for generation := uint64(2); generation <= 5; generation++ {
		value.Generation = generation
		writeProviderContext(t, path, value)
		if err := provider.ReloadNow(); err != nil {
			t.Fatal(err)
		}
	}
	select {
	case update := <-updates:
		if update.Snapshot.Generation != 5 {
			t.Fatalf("coalesced update generation = %d", update.Snapshot.Generation)
		}
	case <-time.After(time.Second):
		t.Fatal("subscriber did not receive update")
	}
	cancel()
	select {
	case _, ok := <-updates:
		if ok {
			t.Fatal("subscription stayed open after cancellation")
		}
	case <-time.After(time.Second):
		t.Fatal("subscription was not closed")
	}
}

func TestContextProviderSubscribeDoesNotEmitRejectedUpdate(t *testing.T) {
	path, value := providerFixture(t, 3, map[string]APIBinding{})
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	updates := provider.Subscribe(context.Background())
	value.Generation = 2
	writeProviderContext(t, path, value)
	if err := provider.ReloadNow(); err == nil {
		t.Fatal("generation regression was accepted")
	}
	select {
	case update := <-updates:
		t.Fatalf("rejected update was published: %#v", update)
	case <-time.After(20 * time.Millisecond):
	}
}

func TestContextProviderSubscriptionClosesWhenWatcherContextEnds(t *testing.T) {
	path, _ := providerFixture(t, 1, map[string]APIBinding{})
	provider, err := NewContextProvider(path, ProviderOptions{PollInterval: time.Millisecond})
	if err != nil {
		t.Fatal(err)
	}
	watchCtx, cancelWatch := context.WithCancel(context.Background())
	if err := provider.Start(watchCtx); err != nil {
		t.Fatal(err)
	}
	updates := provider.Subscribe(context.Background())
	cancelWatch()
	select {
	case _, ok := <-updates:
		if ok {
			t.Fatal("subscription stayed open after watcher context ended")
		}
	case <-time.After(time.Second):
		t.Fatal("watcher shutdown did not close subscription")
	}
	if err := provider.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestContextProviderCurrentHonorsCancelledContext(t *testing.T) {
	path, _ := providerFixture(t, 1, map[string]APIBinding{})
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := provider.Current(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("Current cancellation error = %v", err)
	}
}

func TestContextProviderCurrentAndUpdatesAreDefensiveCopies(t *testing.T) {
	path, value := providerFixture(t, 1, map[string]APIBinding{"api": binding("one", "fixture.api")})
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	current, _ := provider.Current(context.Background())
	current.Bindings["api"] = binding("attacker", "fixture.api")
	current, _ = provider.Current(context.Background())
	if current.Bindings["api"].BindingID != "one" {
		t.Fatal("Current exposed provider-owned map")
	}
	updates := provider.Subscribe(context.Background())
	value.Generation = 2
	writeProviderContext(t, path, value)
	if err := provider.ReloadNow(); err != nil {
		t.Fatal(err)
	}
	update := <-updates
	update.Snapshot.Bindings["api"] = binding("attacker", "fixture.api")
	current, _ = provider.Current(context.Background())
	if current.Bindings["api"].BindingID != "one" {
		t.Fatal("Update exposed provider-owned map")
	}
}

func TestContextProviderCredentialAndRequestReloadTokenPerCall(t *testing.T) {
	path, value := providerFixture(t, 1, map[string]APIBinding{"api": binding("one", "fixture.api")})
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	first, err := provider.NewRequest(context.Background(), "api", http.MethodGet, "/items", nil)
	if err != nil {
		t.Fatal(err)
	}
	if first.Header.Get("Authorization") != "Bearer token-one" {
		t.Fatalf("first token = %q", first.Header.Get("Authorization"))
	}
	if err := os.WriteFile(value.CredentialFile, []byte("token-two"), 0o600); err != nil {
		t.Fatal(err)
	}
	token, err := provider.Credential(context.Background())
	if err != nil || token != "token-two" {
		t.Fatalf("rotated Credential = %q, %v", token, err)
	}
	second, err := provider.NewRequest(context.Background(), "api", http.MethodGet, "/items", nil)
	if err != nil {
		t.Fatal(err)
	}
	if second.Header.Get("Authorization") != "Bearer token-two" {
		t.Fatalf("rotated request token = %q", second.Header.Get("Authorization"))
	}
}

func TestContextProviderDoUsesCurrentBindingAndCredential(t *testing.T) {
	var mu sync.Mutex
	paths := []string{}
	tokens := []string{}
	server := httptest.NewServer(http.HandlerFunc(func(response http.ResponseWriter, request *http.Request) {
		mu.Lock()
		paths = append(paths, request.URL.Path)
		tokens = append(tokens, request.Header.Get("Authorization"))
		mu.Unlock()
		response.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()
	path, value := providerFixture(t, 1, map[string]APIBinding{"api": binding("one", "fixture.api.v1")})
	value.Gateway.Origin = server.URL
	writeProviderContext(t, path, value)
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	client, err := provider.Client(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	response, err := provider.Do(context.Background(), client, "api", http.MethodGet, "/items", nil)
	if err != nil {
		t.Fatal(err)
	}
	response.Body.Close()
	value.Generation = 2
	value.Bindings["api"] = binding("two", "fixture.api.v2")
	writeProviderContext(t, path, value)
	if err := provider.ReloadNow(); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(value.CredentialFile, []byte("token-two"), 0o600); err != nil {
		t.Fatal(err)
	}
	response, err = provider.Do(context.Background(), client, "api", http.MethodGet, "/items", nil)
	if err != nil {
		t.Fatal(err)
	}
	response.Body.Close()
	mu.Lock()
	defer mu.Unlock()
	if fmt.Sprint(paths) != "[/internal/apis/fixture.api.v1/items /internal/apis/fixture.api.v2/items]" {
		t.Fatalf("request paths = %v", paths)
	}
	if fmt.Sprint(tokens) != "[Bearer token-one Bearer token-two]" {
		t.Fatalf("request tokens = %v", tokens)
	}
}

func TestContextProviderConcurrentReadersAndReloads(t *testing.T) {
	path, value := providerFixture(t, 1, map[string]APIBinding{"api": binding("binding-1", "fixture.api")})
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var readers sync.WaitGroup
	for i := 0; i < 32; i++ {
		readers.Add(1)
		go func() {
			defer readers.Done()
			for {
				select {
				case <-ctx.Done():
					return
				default:
				}
				current, err := provider.Current(context.Background())
				if err != nil || current.Generation == 0 || current.Bindings["api"].BindingID == "" {
					t.Errorf("invalid concurrent snapshot: %#v, %v", current, err)
					return
				}
			}
		}()
	}
	for generation := uint64(2); generation <= 100; generation++ {
		value.Generation = generation
		value.Bindings["api"] = binding(fmt.Sprintf("binding-%d", generation), "fixture.api")
		writeProviderContext(t, path, value)
		if err := provider.ReloadNow(); err != nil {
			t.Fatal(err)
		}
	}
	cancel()
	readers.Wait()
	current, _ := provider.Current(context.Background())
	if current.Generation != 100 || current.Bindings["api"].BindingID != "binding-100" {
		t.Fatalf("final snapshot = %#v", current)
	}
}

func TestContextProviderCloseIsIdempotent(t *testing.T) {
	path, _ := providerFixture(t, 1, map[string]APIBinding{})
	provider, err := NewContextProvider(path, ProviderOptions{PollInterval: time.Millisecond})
	if err != nil {
		t.Fatal(err)
	}
	if err := provider.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	if err := provider.Close(); err != nil {
		t.Fatal(err)
	}
	if err := provider.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := provider.Current(context.Background()); err != nil {
		t.Fatalf("last-known-good unavailable after close: %v", err)
	}
}

func TestContextProviderCloseClosesSubscriptionWithoutWatcher(t *testing.T) {
	path, _ := providerFixture(t, 1, map[string]APIBinding{})
	provider, err := NewContextProvider(path, ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	updates := provider.Subscribe(context.Background())
	if err := provider.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case _, ok := <-updates:
		if ok {
			t.Fatal("subscription stayed open after Close")
		}
	case <-time.After(time.Second):
		t.Fatal("Close did not close subscription")
	}
}
