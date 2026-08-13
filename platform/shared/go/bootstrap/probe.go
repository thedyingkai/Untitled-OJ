package bootstrap

import (
	"context"
	"errors"
	"fmt"
	"sync"
)

type ProbeKind string

const (
	ProbeHealth      ProbeKind = "health"
	ProbeReadiness   ProbeKind = "readiness"
	ProbeOK                    = "ok"
	ProbeUnavailable           = "unavailable"
)

type ProbeResult struct {
	Name   string `json:"name"`
	Status string `json:"status"`
}

type ProbeReport struct {
	Status     string        `json:"status"`
	Components []ProbeResult `json:"components,omitempty"`
}

func (report ProbeReport) OK() bool { return report.Status == ProbeOK }

func (report ProbeReport) Err(kind ProbeKind) error {
	if report.OK() {
		return nil
	}
	failed := make([]string, 0, len(report.Components))
	for _, component := range report.Components {
		if component.Status != ProbeOK {
			failed = append(failed, component.Name)
		}
	}
	return &ProbeError{Kind: kind, Components: failed}
}

type ProbeError struct {
	Kind       ProbeKind
	Components []string
}

func (err *ProbeError) Error() string {
	if err == nil {
		return "bootstrap probe failed"
	}
	return fmt.Sprintf("bootstrap %s probe failed for %d component(s)", err.Kind, len(err.Components))
}

type Prober interface {
	Health(context.Context) ProbeReport
	Ready(context.Context) ProbeReport
}

func (runtime *Runtime) Health(ctx context.Context) ProbeReport {
	return runtime.probe(ctx, ProbeHealth)
}

func (runtime *Runtime) Ready(ctx context.Context) ProbeReport {
	return runtime.probe(ctx, ProbeReadiness)
}

func (runtime *Runtime) probe(ctx context.Context, kind ProbeKind) ProbeReport {
	if runtime == nil {
		return ProbeReport{Status: ProbeUnavailable}
	}
	if ctx == nil {
		ctx = context.Background()
	}
	runtime.mu.RLock()
	phase := runtime.phase
	started := append([]startedComponent(nil), runtime.started...)
	timeout := runtime.manifest.ProbeTimeout
	runtime.mu.RUnlock()
	if phase != PhaseRunning && phase != PhaseStarting {
		return ProbeReport{Status: ProbeUnavailable}
	}

	type target struct {
		index int
		name  string
		check func(context.Context) error
	}
	targets := make([]target, 0, len(started))
	for _, entry := range started {
		var check func(context.Context) error
		if kind == ProbeHealth {
			if checker, ok := entry.component.(HealthChecker); ok {
				check = checker.Health
			}
		} else if checker, ok := entry.component.(ReadinessChecker); ok {
			check = checker.Ready
		}
		if check != nil {
			targets = append(targets, target{index: len(targets), name: entry.spec.Name, check: check})
		}
	}
	results := make([]ProbeResult, len(targets))
	var wait sync.WaitGroup
	wait.Add(len(targets))
	for _, current := range targets {
		current := current
		go func() {
			defer wait.Done()
			probeContext, cancel := context.WithTimeout(ctx, timeout)
			defer cancel()
			status := ProbeOK
			if err := isolatedProbe(probeContext, current.check); err != nil {
				status = ProbeUnavailable
			}
			results[current.index] = ProbeResult{Name: current.name, Status: status}
		}()
	}
	wait.Wait()
	report := ProbeReport{Status: ProbeOK, Components: results}
	for _, result := range results {
		if result.Status != ProbeOK {
			report.Status = ProbeUnavailable
			break
		}
	}
	return report
}

func isolatedProbe(ctx context.Context, check func(context.Context) error) error {
	result := make(chan error, 1)
	go func() {
		defer func() {
			if recover() != nil {
				result <- errors.New("probe panicked")
			}
		}()
		result <- check(ctx)
	}()
	select {
	case err := <-result:
		return err
	case <-ctx.Done():
		return ctx.Err()
	}
}
