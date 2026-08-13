package bootstrap

import (
	"context"
	"errors"
	"fmt"
	"sync"
)

type startedComponent struct {
	spec      ComponentSpec
	component Component
}

// Runtime owns one service's component graph and lifecycle. Runtime has no
// package-global state; callers may construct independent instances in tests or
// in a process hosting more than one service.
type Runtime struct {
	manifest  Manifest
	factories map[Kind]Factory

	mu          sync.RWMutex
	phase       Phase
	values      map[string]any
	started     []startedComponent
	runContext  context.Context
	cancel      context.CancelFunc
	terminalErr error
	closeErr    error

	startDone     chan struct{}
	startDoneOnce sync.Once
	closeDone     chan struct{}
	closeDoneOnce sync.Once
	failures      chan error
}

func New(manifest Manifest, options Options) (*Runtime, error) {
	normalized, err := normalizeManifest(manifest)
	if err != nil {
		return nil, err
	}
	factories := make(map[Kind]Factory, len(options.Factories))
	for kind, factory := range options.Factories {
		if !validToken(string(kind)) || factory == nil {
			return nil, errors.New("bootstrap factory registration is invalid")
		}
		factories[kind] = factory
	}
	values := make(map[string]any, len(options.InitialValues))
	for name, value := range options.InitialValues {
		if !validToken(name) || value == nil {
			return nil, errors.New("bootstrap initial value registration is invalid")
		}
		values[name] = value
	}
	return &Runtime{
		manifest:  normalized,
		factories: factories,
		phase:     PhaseNew,
		values:    values,
		startDone: make(chan struct{}),
		closeDone: make(chan struct{}),
		failures:  make(chan error, len(normalized.Components)+1),
	}, nil
}

func (runtime *Runtime) Manifest() Manifest {
	if runtime == nil {
		return Manifest{}
	}
	runtime.mu.RLock()
	defer runtime.mu.RUnlock()
	manifest := runtime.manifest
	manifest.Components = append([]ComponentSpec(nil), runtime.manifest.Components...)
	for index := range manifest.Components {
		manifest.Components[index].DependsOn = append([]string(nil), manifest.Components[index].DependsOn...)
	}
	return manifest
}

func (runtime *Runtime) Phase() Phase {
	if runtime == nil {
		return PhaseClosed
	}
	runtime.mu.RLock()
	defer runtime.mu.RUnlock()
	return runtime.phase
}

func (runtime *Runtime) Lookup(name string) (any, bool) {
	if runtime == nil {
		return nil, false
	}
	runtime.mu.RLock()
	defer runtime.mu.RUnlock()
	value, ok := runtime.values[name]
	return value, ok
}

// Errors reports unexpected background component failures. Error strings are
// sanitized ComponentErrors and never contain the originating error message.
func (runtime *Runtime) Errors() <-chan error {
	if runtime == nil {
		return nil
	}
	return runtime.failures
}

// Start builds and starts components in stable dependency order. A start
// failure closes the failing component and all previously started components
// in reverse order before returning.
func (runtime *Runtime) Start(ctx context.Context) error {
	if runtime == nil {
		return ErrClosed
	}
	if ctx == nil {
		ctx = context.Background()
	}
	runtime.mu.Lock()
	if runtime.phase != PhaseNew {
		phase := runtime.phase
		runtime.mu.Unlock()
		if phase == PhaseClosed || phase == PhaseFailed || phase == PhaseClosing {
			return ErrClosed
		}
		return fmt.Errorf("bootstrap runtime cannot start from phase %s", phase)
	}
	runtime.phase = PhaseStarting
	runtime.runContext, runtime.cancel = context.WithCancel(ctx)
	runContext := runtime.runContext
	runtime.mu.Unlock()
	defer runtime.startDoneOnce.Do(func() { close(runtime.startDone) })

	for _, spec := range runtime.manifest.Components {
		if err := runContext.Err(); err != nil {
			startErr := componentError(spec.Name, "start", err)
			runtime.failStart(startErr, nil)
			return startErr
		}
		factory, exists := runtime.factories[spec.Kind]
		if !exists {
			if spec.Optional {
				continue
			}
			startErr := componentError(spec.Name, "build", errors.New("factory is unavailable"))
			runtime.failStart(startErr, nil)
			return startErr
		}
		component, err := factory.Build(BuildContext{
			Context: runContext,
			Spec:    spec,
			Values:  runtime,
			Probes:  runtime,
		})
		if err != nil || component == nil {
			if err == nil {
				err = errors.New("factory returned no component")
			}
			startErr := componentError(spec.Name, "build", err)
			runtime.failStart(startErr, nil)
			return startErr
		}
		if err := component.Start(runContext); err != nil {
			startErr := componentError(spec.Name, "start", err)
			runtime.failStart(startErr, &startedComponent{spec: spec, component: component})
			return startErr
		}
		entry := startedComponent{spec: spec, component: component}
		if err := runtime.publish(entry); err != nil {
			startErr := componentError(spec.Name, "publish outputs", err)
			runtime.failStart(startErr, &entry)
			return startErr
		}
		runtime.mu.Lock()
		runtime.started = append(runtime.started, entry)
		runtime.mu.Unlock()
	}

	runtime.mu.Lock()
	if runtime.phase != PhaseStarting {
		runtime.mu.Unlock()
		startErr := componentError(runtime.manifest.Service, "start", context.Canceled)
		runtime.failStart(startErr, nil)
		return startErr
	}
	runtime.phase = PhaseRunning
	started := append([]startedComponent(nil), runtime.started...)
	runtime.mu.Unlock()
	for _, entry := range started {
		runtime.monitor(entry)
	}
	go func() {
		<-runContext.Done()
		_ = runtime.Close(context.Background())
	}()
	return nil
}

func (runtime *Runtime) publish(entry startedComponent) error {
	provider, ok := entry.component.(OutputProvider)
	if !ok {
		return nil
	}
	outputs := provider.Outputs()
	for name, value := range outputs {
		if !validToken(name) || value == nil {
			return errors.New("component output registration is invalid")
		}
	}
	runtime.mu.Lock()
	defer runtime.mu.Unlock()
	for name := range outputs {
		if _, exists := runtime.values[name]; exists {
			return fmt.Errorf("component output %q is duplicated", name)
		}
	}
	for name, value := range outputs {
		runtime.values[name] = value
	}
	return nil
}

func (runtime *Runtime) failStart(startErr error, current *startedComponent) {
	runtime.mu.Lock()
	if runtime.cancel != nil {
		runtime.cancel()
	}
	started := append([]startedComponent(nil), runtime.started...)
	if current != nil {
		started = append(started, *current)
	}
	runtime.mu.Unlock()
	rollbackErr := runtime.closeComponents(context.Background(), started)
	runtime.mu.Lock()
	runtime.phase = PhaseFailed
	runtime.terminalErr = startErr
	runtime.closeErr = rollbackErr
	runtime.mu.Unlock()
	runtime.closeDoneOnce.Do(func() {
		close(runtime.failures)
		close(runtime.closeDone)
	})
}

func (runtime *Runtime) monitor(entry startedComponent) {
	source, ok := entry.component.(FailureSource)
	if !ok {
		return
	}
	failures := source.Errors()
	if failures == nil {
		return
	}
	go func() {
		select {
		case err, open := <-failures:
			if open && err != nil {
				runtime.fail(componentError(entry.spec.Name, "run", err))
			}
		case <-runtime.runContext.Done():
		}
	}()
}

func (runtime *Runtime) fail(err error) {
	if err == nil {
		return
	}
	runtime.mu.Lock()
	if runtime.phase != PhaseRunning {
		runtime.mu.Unlock()
		return
	}
	runtime.terminalErr = err
	select {
	case runtime.failures <- err:
	default:
	}
	if runtime.cancel != nil {
		runtime.cancel()
	}
	runtime.mu.Unlock()
}

// Close is idempotent and safe for concurrent callers. It cancels the runtime
// context first, then closes components in reverse start order under the
// manifest's overall shutdown deadline. A component that has not returned when
// that deadline expires is abandoned; Close does not join code that ignores its
// cancellation context.
func (runtime *Runtime) Close(ctx context.Context) error {
	if runtime == nil {
		return nil
	}
	if ctx == nil {
		ctx = context.Background()
	}
	for {
		runtime.mu.Lock()
		switch runtime.phase {
		case PhaseNew:
			runtime.phase = PhaseClosed
			runtime.startDoneOnce.Do(func() { close(runtime.startDone) })
			runtime.closeDoneOnce.Do(func() {
				close(runtime.failures)
				close(runtime.closeDone)
			})
			runtime.mu.Unlock()
			return nil
		case PhaseStarting:
			if runtime.cancel != nil {
				runtime.cancel()
			}
			startDone := runtime.startDone
			runtime.mu.Unlock()
			select {
			case <-startDone:
				continue
			case <-ctx.Done():
				return ctx.Err()
			}
		case PhaseRunning:
			runtime.phase = PhaseClosing
			if runtime.cancel != nil {
				runtime.cancel()
			}
			started := append([]startedComponent(nil), runtime.started...)
			runtime.mu.Unlock()
			closeErr := runtime.closeComponents(ctx, started)
			runtime.mu.Lock()
			runtime.closeErr = closeErr
			runtime.phase = PhaseClosed
			runtime.mu.Unlock()
			runtime.closeDoneOnce.Do(func() {
				close(runtime.failures)
				close(runtime.closeDone)
			})
			return closeErr
		case PhaseClosing:
			closeDone := runtime.closeDone
			runtime.mu.Unlock()
			select {
			case <-closeDone:
				runtime.mu.RLock()
				err := runtime.closeErr
				runtime.mu.RUnlock()
				return err
			case <-ctx.Done():
				return ctx.Err()
			}
		case PhaseFailed, PhaseClosed:
			err := runtime.closeErr
			runtime.mu.Unlock()
			return err
		default:
			runtime.mu.Unlock()
			return ErrClosed
		}
	}
}

func (runtime *Runtime) closeComponents(parent context.Context, components []startedComponent) error {
	shutdownContext, cancel := context.WithTimeout(parent, runtime.manifest.ShutdownTimeout)
	defer cancel()
	var closeErrors []error
	for index := len(components) - 1; index >= 0; index-- {
		entry := components[index]
		if err := closeComponent(shutdownContext, entry.component); err != nil {
			closeErrors = append(closeErrors, componentError(entry.spec.Name, "close", err))
		}
	}
	return errors.Join(closeErrors...)
}

func closeComponent(ctx context.Context, component Component) error {
	result := make(chan error, 1)
	go func() {
		defer func() {
			if recovered := recover(); recovered != nil {
				result <- errors.New("component close panicked")
			}
		}()
		result <- component.Close(ctx)
	}()
	select {
	case err := <-result:
		return err
	case <-ctx.Done():
		return ctx.Err()
	}
}

// Wait waits for the runtime's shutdown procedure and returns an unexpected
// run/start failure before a close failure. Normal parent cancellation followed
// by a clean close is nil. If the shutdown deadline expires, Wait returns the
// timeout once the runtime has stopped waiting; a non-cooperative component may
// still be unwinding in its own goroutine.
func (runtime *Runtime) Wait(ctx context.Context) error {
	if runtime == nil {
		return ErrClosed
	}
	if ctx == nil {
		ctx = context.Background()
	}
	runtime.mu.RLock()
	phase := runtime.phase
	runtime.mu.RUnlock()
	if phase == PhaseNew {
		return ErrNotStarted
	}
	select {
	case <-runtime.closeDone:
		runtime.mu.RLock()
		defer runtime.mu.RUnlock()
		if runtime.terminalErr != nil {
			return runtime.terminalErr
		}
		return runtime.closeErr
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (runtime *Runtime) Run(ctx context.Context) error {
	if err := runtime.Start(ctx); err != nil {
		return err
	}
	return runtime.Wait(context.Background())
}
