package bootstrap

import (
	"context"
	"errors"
	"reflect"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func testManifest(names ...string) Manifest {
	components := make([]ComponentSpec, 0, len(names))
	for index, name := range names {
		spec := ComponentSpec{Name: name, Kind: KindDomain}
		if index > 0 {
			spec.DependsOn = []string{names[index-1]}
		}
		components = append(components, spec)
	}
	return Manifest{
		Service: "test-service", ShutdownTimeout: 100 * time.Millisecond,
		ProbeTimeout: 20 * time.Millisecond, Components: components,
	}
}

func TestStartFailureClosesFailingComponentAndRollsBackInReverse(t *testing.T) {
	var lock sync.Mutex
	var events []string
	record := func(value string) {
		lock.Lock()
		defer lock.Unlock()
		events = append(events, value)
	}
	runtime, err := New(testManifest("database", "events", "http"), Options{Factories: map[Kind]Factory{
		KindDomain: FactoryFunc(func(ctx BuildContext) (Component, error) {
			name := ctx.Spec.Name
			return &ComponentFuncs{
				StartFunc: func(context.Context) error {
					record("start:" + name)
					if name == "events" {
						return errors.New("postgres://user:secret@example.invalid/db")
					}
					return nil
				},
				CloseFunc: func(context.Context) error { record("close:" + name); return nil },
			}, nil
		}),
	}})
	if err != nil {
		t.Fatal(err)
	}
	err = runtime.Start(context.Background())
	if err == nil || strings.Contains(err.Error(), "secret") {
		t.Fatalf("start error was not sanitized: %v", err)
	}
	want := []string{"start:database", "start:events", "close:events", "close:database"}
	if !reflect.DeepEqual(events, want) {
		t.Fatalf("events = %#v, want %#v", events, want)
	}
	if runtime.Phase() != PhaseFailed {
		t.Fatalf("phase = %s", runtime.Phase())
	}
}

func TestConcurrentCloseClosesEachComponentOnce(t *testing.T) {
	var closes atomic.Int32
	runtime, err := New(testManifest("worker"), Options{Factories: map[Kind]Factory{
		KindDomain: FactoryFunc(func(BuildContext) (Component, error) {
			return &ComponentFuncs{CloseFunc: func(context.Context) error {
				closes.Add(1)
				time.Sleep(10 * time.Millisecond)
				return nil
			}}, nil
		}),
	}})
	if err != nil {
		t.Fatal(err)
	}
	if err := runtime.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	var wait sync.WaitGroup
	for range 32 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			if err := runtime.Close(context.Background()); err != nil {
				t.Errorf("close: %v", err)
			}
		}()
	}
	wait.Wait()
	if closes.Load() != 1 {
		t.Fatalf("close count = %d", closes.Load())
	}
}

func TestParentCancellationTriggersBoundedShutdown(t *testing.T) {
	closeStarted := make(chan context.Context, 1)
	closeObservedDeadline := make(chan struct{})
	releaseClose := make(chan struct{})
	closed := make(chan struct{})
	runtime, err := New(Manifest{
		Service: "test-service", ShutdownTimeout: 30 * time.Millisecond,
		ProbeTimeout: time.Second,
		Components:   []ComponentSpec{{Name: "worker", Kind: KindDomain}},
	}, Options{Factories: map[Kind]Factory{
		KindDomain: FactoryFunc(func(BuildContext) (Component, error) {
			return &ComponentFuncs{CloseFunc: func(ctx context.Context) error {
				defer close(closed)
				closeStarted <- ctx
				<-ctx.Done()
				close(closeObservedDeadline)
				<-releaseClose
				return ctx.Err()
			}}, nil
		}),
	}})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	if err := runtime.Start(ctx); err != nil {
		t.Fatal(err)
	}
	started := time.Now()
	cancel()
	var closeContext context.Context
	select {
	case closeContext = <-closeStarted:
	case <-time.After(time.Second):
		t.Fatal("parent cancellation did not start component shutdown")
	}
	err = runtime.Wait(context.Background())
	if !errors.Is(err, context.DeadlineExceeded) || time.Since(started) > 500*time.Millisecond {
		t.Fatalf("bounded shutdown error=%v elapsed=%s", err, time.Since(started))
	}
	select {
	case <-closeObservedDeadline:
	case <-time.After(time.Second):
		t.Fatal("component close did not observe the bounded context")
	}
	if !errors.Is(closeContext.Err(), context.DeadlineExceeded) {
		t.Fatalf("component close context error = %v", closeContext.Err())
	}

	// Deadline expiry deliberately lets Runtime stop waiting for code that
	// ignores cancellation. Release and join that code explicitly so the test
	// proves the boundary without leaking a goroutine.
	close(releaseClose)
	select {
	case <-closed:
	case <-time.After(time.Second):
		t.Fatal("component close did not exit after test release")
	}
}

func TestWaitIncludesCooperativeComponentShutdown(t *testing.T) {
	closeStarted := make(chan struct{})
	releaseClose := make(chan struct{})
	closed := make(chan struct{})
	runtime, err := New(testManifest("worker"), Options{Factories: map[Kind]Factory{
		KindDomain: FactoryFunc(func(BuildContext) (Component, error) {
			return &ComponentFuncs{CloseFunc: func(context.Context) error {
				close(closeStarted)
				<-releaseClose
				close(closed)
				return nil
			}}, nil
		}),
	}})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	if err := runtime.Start(ctx); err != nil {
		t.Fatal(err)
	}
	waitResult := make(chan error, 1)
	go func() { waitResult <- runtime.Wait(context.Background()) }()
	cancel()
	select {
	case <-closeStarted:
	case <-time.After(time.Second):
		t.Fatal("parent cancellation did not start component shutdown")
	}
	select {
	case err := <-waitResult:
		t.Fatalf("Wait returned before cooperative component shutdown: %v", err)
	default:
	}
	close(releaseClose)
	select {
	case <-closed:
	case <-time.After(time.Second):
		t.Fatal("cooperative component did not close")
	}
	select {
	case err := <-waitResult:
		if err != nil {
			t.Fatalf("Wait returned a clean-shutdown error: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Wait did not include cooperative component shutdown")
	}
}

func TestProbeIsolationAndStableOrder(t *testing.T) {
	runtime, err := New(testManifest("fast", "slow", "panic"), Options{Factories: map[Kind]Factory{
		KindDomain: FactoryFunc(func(ctx BuildContext) (Component, error) {
			component := &ComponentFuncs{}
			switch ctx.Spec.Name {
			case "fast":
				component.HealthFunc = func(context.Context) error { return nil }
			case "slow":
				component.HealthFunc = func(ctx context.Context) error { <-ctx.Done(); return ctx.Err() }
			case "panic":
				component.HealthFunc = func(context.Context) error { panic("sensitive") }
			}
			return component, nil
		}),
	}})
	if err != nil {
		t.Fatal(err)
	}
	if err := runtime.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	report := runtime.Health(context.Background())
	if report.Status != ProbeUnavailable {
		t.Fatalf("report = %#v", report)
	}
	want := []ProbeResult{
		{Name: "fast", Status: ProbeOK},
		{Name: "slow", Status: ProbeUnavailable},
		{Name: "panic", Status: ProbeUnavailable},
	}
	if !reflect.DeepEqual(report.Components, want) {
		t.Fatalf("components = %#v, want %#v", report.Components, want)
	}
	if text := report.Err(ProbeHealth).Error(); strings.Contains(text, "sensitive") {
		t.Fatalf("probe error leaked cause: %s", text)
	}
	_ = runtime.Close(context.Background())
}

func TestBackgroundFailureCancelsAndClosesRuntime(t *testing.T) {
	failures := make(chan error, 1)
	var closes atomic.Int32
	runtime, err := New(testManifest("relay"), Options{Factories: map[Kind]Factory{
		KindDomain: FactoryFunc(func(BuildContext) (Component, error) {
			return &ComponentFuncs{
				ErrorsC:   failures,
				CloseFunc: func(context.Context) error { closes.Add(1); return nil },
			}, nil
		}),
	}})
	if err != nil {
		t.Fatal(err)
	}
	if err := runtime.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	failures <- errors.New("redis://:secret@example.invalid")
	err = runtime.Wait(context.Background())
	if err == nil || strings.Contains(err.Error(), "secret") || closes.Load() != 1 {
		t.Fatalf("wait error=%v closes=%d", err, closes.Load())
	}
}

func TestManifestUsesStableDependencyOrderAndRejectsCycles(t *testing.T) {
	manifest := Manifest{Service: "test-service", Components: []ComponentSpec{
		{Name: "http", Kind: KindHTTP, DependsOn: []string{"domain"}},
		{Name: "database", Kind: KindPostgreSQL},
		{Name: "events", Kind: KindEventRelay, DependsOn: []string{"database"}},
		{Name: "domain", Kind: KindDomain, DependsOn: []string{"database"}},
	}}
	normalized, err := normalizeManifest(manifest)
	if err != nil {
		t.Fatal(err)
	}
	got := make([]string, 0, len(normalized.Components))
	for _, spec := range normalized.Components {
		got = append(got, spec.Name)
	}
	if want := []string{"database", "events", "domain", "http"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("order = %#v, want %#v", got, want)
	}
	manifest.Components = []ComponentSpec{
		{Name: "one", Kind: KindDomain, DependsOn: []string{"two"}},
		{Name: "two", Kind: KindDomain, DependsOn: []string{"one"}},
	}
	if _, err := normalizeManifest(manifest); err == nil {
		t.Fatal("cycle was accepted")
	}
}
