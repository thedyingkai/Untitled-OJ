package servicecontext

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

const defaultProviderPollInterval = 5 * time.Second

// ProviderOptions controls how a ContextProvider observes an Agent-published
// context file. PollInterval is intentionally configurable so callers can use
// a short interval in tests without changing the production default.
type ProviderOptions struct {
	PollInterval time.Duration
}

// Update is emitted after a valid snapshot with a strictly newer generation
// has been atomically installed. Snapshot is safe for the receiver to mutate.
type Update struct {
	PreviousGeneration uint64
	Snapshot           ServiceContext
}

// BindingUnavailable reports that a named binding is not present in the
// current snapshot. This is the normal runtime result for an optional binding
// that has not been resolved yet or was removed during an update.
type BindingUnavailable struct {
	Name       string
	Generation uint64
}

func (err *BindingUnavailable) Error() string {
	if err == nil {
		return "API binding is unavailable"
	}
	return fmt.Sprintf("API binding %q is unavailable at service context generation %d", err.Name, err.Generation)
}

// IsBindingUnavailable supports errors.As without callers depending on the
// concrete error representation.
func IsBindingUnavailable(err error) bool {
	var unavailable *BindingUnavailable
	return errors.As(err, &unavailable)
}

// ContextProvider owns one immutable, last-known-good Service Context and
// replaces it atomically after a complete file has parsed and validated.
//
// Construct it with NewContextProvider, then call Start. Current remains
// available after the watcher stops; Close only stops future reloads.
type ContextProvider struct {
	path         string
	pollInterval time.Duration

	current atomic.Pointer[ServiceContext]

	mu          sync.Mutex
	started     bool
	closed      bool
	cancel      context.CancelFunc
	done        chan struct{}
	stop        chan struct{}
	lastChecked fileIdentity
	subscribers map[uint64]chan Update
	nextSubID   uint64
}

type fileIdentity struct {
	info    os.FileInfo
	size    int64
	modTime time.Time
}

// NewContextProvider synchronously loads and validates the initial snapshot.
// A provider is never returned without a usable Current value.
func NewContextProvider(path string, options ProviderOptions) (*ContextProvider, error) {
	path = strings.TrimSpace(path)
	if path == "" {
		return nil, errors.New("service context provider path is required")
	}
	pollInterval := options.PollInterval
	if pollInterval == 0 {
		pollInterval = defaultProviderPollInterval
	}
	if pollInterval < 0 {
		return nil, errors.New("service context provider poll interval must not be negative")
	}
	snapshot, identity, err := loadSnapshotWithIdentity(path)
	if err != nil {
		return nil, err
	}
	provider := &ContextProvider{
		path:         path,
		pollInterval: pollInterval,
		lastChecked:  identity,
		subscribers:  make(map[uint64]chan Update),
		stop:         make(chan struct{}),
	}
	provider.current.Store(snapshot)
	return provider, nil
}

// Start begins polling until ctx is cancelled or Close is called. It is safe
// to call more than once; subsequent calls are no-ops.
func (provider *ContextProvider) Start(ctx context.Context) error {
	if provider == nil {
		return errors.New("service context provider is nil")
	}
	if ctx == nil {
		ctx = context.Background()
	}
	provider.mu.Lock()
	defer provider.mu.Unlock()
	if provider.closed {
		return errors.New("service context provider is closed")
	}
	if provider.started {
		return nil
	}
	watchCtx, cancel := context.WithCancel(ctx)
	provider.started = true
	provider.cancel = cancel
	provider.done = make(chan struct{})
	go provider.watch(watchCtx, provider.done)
	return nil
}

// Close stops polling and closes every subscription channel. It is idempotent.
func (provider *ContextProvider) Close() error {
	if provider == nil {
		return nil
	}
	provider.mu.Lock()
	if provider.closed {
		provider.mu.Unlock()
		return nil
	}
	provider.closed = true
	close(provider.stop)
	cancel := provider.cancel
	done := provider.done
	if cancel == nil {
		provider.closeSubscribersLocked()
	}
	provider.mu.Unlock()
	if cancel != nil {
		cancel()
		<-done
	}
	return nil
}

// Current returns an independent copy of the current last-known-good snapshot.
func (provider *ContextProvider) Current(ctx context.Context) (ServiceContext, error) {
	if provider == nil {
		return ServiceContext{}, errors.New("service context provider is nil")
	}
	if ctx != nil {
		select {
		case <-ctx.Done():
			return ServiceContext{}, ctx.Err()
		default:
		}
	}
	current := provider.current.Load()
	if current == nil {
		return ServiceContext{}, errors.New("service context provider has no valid snapshot")
	}
	return cloneServiceContext(*current), nil
}

// Subscribe returns a bounded, coalescing stream of future accepted updates.
// A slow receiver sees the newest available update rather than blocking the
// watcher or accumulating an unbounded queue. The channel closes on ctx or
// provider shutdown.
func (provider *ContextProvider) Subscribe(ctx context.Context) <-chan Update {
	updates := make(chan Update, 1)
	if provider == nil {
		close(updates)
		return updates
	}
	if ctx == nil {
		ctx = context.Background()
	}
	provider.mu.Lock()
	if provider.closed {
		close(updates)
		provider.mu.Unlock()
		return updates
	}
	provider.nextSubID++
	id := provider.nextSubID
	provider.subscribers[id] = updates
	provider.mu.Unlock()
	go func() {
		select {
		case <-ctx.Done():
		case <-provider.stop:
		}
		provider.mu.Lock()
		if subscribed, ok := provider.subscribers[id]; ok {
			delete(provider.subscribers, id)
			close(subscribed)
		}
		provider.mu.Unlock()
	}()
	return updates
}

// ReloadNow checks the file immediately. Invalid, missing, partial and stale
// snapshots return an error while retaining the last-known-good Current value.
// An unchanged file is a successful no-op.
func (provider *ContextProvider) ReloadNow() error {
	if provider == nil {
		return errors.New("service context provider is nil")
	}
	provider.mu.Lock()
	defer provider.mu.Unlock()
	if provider.closed {
		return errors.New("service context provider is closed")
	}
	return provider.reloadLocked()
}

func (provider *ContextProvider) reloadLocked() error {
	identity, err := statFileIdentity(provider.path)
	if err != nil {
		return err
	}
	if sameFileIdentity(provider.lastChecked, identity) {
		return nil
	}
	provider.lastChecked = identity
	candidate, loadedIdentity, err := loadSnapshotWithIdentity(provider.path)
	if err != nil {
		return err
	}
	provider.lastChecked = loadedIdentity
	current := provider.current.Load()
	if current == nil {
		return errors.New("service context provider has no valid snapshot")
	}
	if candidate.Generation < current.Generation {
		return fmt.Errorf("service context generation regression: current=%d candidate=%d", current.Generation, candidate.Generation)
	}
	if candidate.Generation == current.Generation {
		if !sameServiceContext(*current, *candidate) {
			return fmt.Errorf("service context generation %d was reused with different content", candidate.Generation)
		}
		return nil
	}
	previousGeneration := current.Generation
	provider.current.Store(candidate)
	provider.publishLocked(Update{
		PreviousGeneration: previousGeneration,
		Snapshot:           cloneServiceContext(*candidate),
	})
	return nil
}

// Binding resolves a name against the snapshot current at the instant of the
// call. Optional binding changes therefore do not require a process restart.
func (provider *ContextProvider) Binding(ctx context.Context, name string) (APIBinding, error) {
	snapshot, err := provider.Current(ctx)
	if err != nil {
		return APIBinding{}, err
	}
	binding, ok := snapshot.Bindings[name]
	if !ok {
		return APIBinding{}, &BindingUnavailable{Name: name, Generation: snapshot.Generation}
	}
	return binding, nil
}

func (provider *ContextProvider) BindingURL(ctx context.Context, name, relativePath string) (string, error) {
	snapshot, err := provider.Current(ctx)
	if err != nil {
		return "", err
	}
	if _, ok := snapshot.Bindings[name]; !ok {
		return "", &BindingUnavailable{Name: name, Generation: snapshot.Generation}
	}
	return snapshot.BindingURL(name, relativePath)
}

// Client builds a client from the current Gateway and CA snapshot. It should
// be called again after an Update that changes Gateway metadata.
func (provider *ContextProvider) Client(ctx context.Context) (*http.Client, error) {
	snapshot, err := provider.Current(ctx)
	if err != nil {
		return nil, err
	}
	return snapshot.Client()
}

// Credential reads the credential file named by the latest snapshot every
// time it is called. No token bytes are cached by ContextProvider.
func (provider *ContextProvider) Credential(ctx context.Context) (string, error) {
	snapshot, err := provider.Current(ctx)
	if err != nil {
		return "", err
	}
	return readCredential(snapshot.CredentialFile)
}

// NewRequest snapshots routing metadata and reloads the token for this call.
func (provider *ContextProvider) NewRequest(ctx context.Context, bindingName, method, relativePath string, body io.Reader) (*http.Request, error) {
	return provider.NewRequestWithOptions(ctx, bindingName, method, relativePath, body, RequestOptions{ContentLength: -1})
}

func (provider *ContextProvider) NewRequestWithOptions(ctx context.Context, bindingName, method, relativePath string, body io.Reader, options RequestOptions) (*http.Request, error) {
	snapshot, err := provider.Current(ctx)
	if err != nil {
		return nil, err
	}
	if _, ok := snapshot.Bindings[bindingName]; !ok {
		return nil, &BindingUnavailable{Name: bindingName, Generation: snapshot.Generation}
	}
	return snapshot.NewRequestWithOptions(ctx, bindingName, method, relativePath, body, options)
}

// Do uses one coherent routing snapshot for binding lookup, request timeout
// and URL construction while still reading the workload credential for this
// request. A concurrent reload affects the next call.
func (provider *ContextProvider) Do(ctx context.Context, client *http.Client, bindingName, method, relativePath string, body io.Reader) (*http.Response, error) {
	return provider.DoWithOptions(ctx, client, bindingName, method, relativePath, body, RequestOptions{ContentLength: -1})
}

func (provider *ContextProvider) DoWithOptions(ctx context.Context, client *http.Client, bindingName, method, relativePath string, body io.Reader, options RequestOptions) (*http.Response, error) {
	snapshot, err := provider.Current(ctx)
	if err != nil {
		return nil, err
	}
	if _, ok := snapshot.Bindings[bindingName]; !ok {
		return nil, &BindingUnavailable{Name: bindingName, Generation: snapshot.Generation}
	}
	return snapshot.DoWithOptions(ctx, client, bindingName, method, relativePath, body, options)
}

func (provider *ContextProvider) watch(ctx context.Context, done chan struct{}) {
	defer func() {
		provider.mu.Lock()
		provider.closeSubscribersLocked()
		provider.mu.Unlock()
		close(done)
	}()
	ticker := time.NewTicker(provider.pollInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			provider.mu.Lock()
			if provider.closed {
				provider.mu.Unlock()
				return
			}
			_ = provider.reloadLocked()
			provider.mu.Unlock()
		}
	}
}

func (provider *ContextProvider) publishLocked(update Update) {
	for _, subscriber := range provider.subscribers {
		copy := Update{PreviousGeneration: update.PreviousGeneration, Snapshot: cloneServiceContext(update.Snapshot)}
		select {
		case subscriber <- copy:
		default:
			select {
			case <-subscriber:
			default:
			}
			select {
			case subscriber <- copy:
			default:
			}
		}
	}
}

func (provider *ContextProvider) closeSubscribersLocked() {
	for id, subscriber := range provider.subscribers {
		delete(provider.subscribers, id)
		close(subscriber)
	}
}

func loadSnapshotWithIdentity(path string) (*ServiceContext, fileIdentity, error) {
	before, err := statFileIdentity(path)
	if err != nil {
		return nil, fileIdentity{}, err
	}
	snapshot, err := Load(path)
	if err != nil {
		return nil, before, err
	}
	after, err := statFileIdentity(path)
	if err != nil {
		return nil, before, err
	}
	if !sameFileIdentity(before, after) {
		return nil, after, errors.New("service context changed while it was being loaded")
	}
	copy := cloneServiceContext(snapshot)
	return &copy, after, nil
}

func statFileIdentity(path string) (fileIdentity, error) {
	info, err := os.Stat(path)
	if err != nil {
		return fileIdentity{}, fmt.Errorf("inspect service context: %w", err)
	}
	return fileIdentity{info: info, size: info.Size(), modTime: info.ModTime()}, nil
}

func sameFileIdentity(left, right fileIdentity) bool {
	if left.info == nil || right.info == nil {
		return false
	}
	return os.SameFile(left.info, right.info) && left.size == right.size && left.modTime.Equal(right.modTime)
}

func cloneServiceContext(value ServiceContext) ServiceContext {
	copy := value
	copy.Bindings = make(map[string]APIBinding, len(value.Bindings))
	for name, binding := range value.Bindings {
		copy.Bindings[name] = binding
	}
	return copy
}

func sameServiceContext(left, right ServiceContext) bool {
	if left.SchemaVersion != right.SchemaVersion || left.Deployment != right.Deployment ||
		left.Gateway != right.Gateway || left.CredentialFile != right.CredentialFile ||
		left.Generation != right.Generation || len(left.Bindings) != len(right.Bindings) {
		return false
	}
	for name, binding := range left.Bindings {
		if right.Bindings[name] != binding {
			return false
		}
	}
	return true
}
