// Package bootstrap composes the platform-owned edge of an OJOS service.
//
// A Runtime is deliberately an ordinary value: it installs no process-global
// singleton and can be constructed more than once in the same process. The
// manifest describes ordering and dependencies while Options supplies the
// process-specific factories and initial values.
package bootstrap

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"strings"
	"time"
)

type Kind string

const (
	KindLogging    Kind = "logging"
	KindTracing    Kind = "tracing"
	KindPostgreSQL Kind = "postgresql"
	KindEventRelay Kind = "event-relay"
	KindPermission Kind = "permission"
	KindDomain     Kind = "domain"
	KindHTTP       Kind = "http"
)

const (
	defaultShutdownTimeout = 15 * time.Second
	defaultProbeTimeout    = time.Second
)

type Manifest struct {
	Service         string
	ShutdownTimeout time.Duration
	ProbeTimeout    time.Duration
	Components      []ComponentSpec
}

// ComponentSpec is intentionally free of credentials and service addresses.
// Those belong in Agent materialization or factory Options and are never
// included in lifecycle errors or probe responses.
type ComponentSpec struct {
	Name      string
	Kind      Kind
	DependsOn []string
	Optional  bool
}

type Options struct {
	Factories     map[Kind]Factory
	InitialValues map[string]any
}

type Factory interface {
	Build(BuildContext) (Component, error)
}

type FactoryFunc func(BuildContext) (Component, error)

func (factory FactoryFunc) Build(ctx BuildContext) (Component, error) {
	return factory(ctx)
}

type BuildContext struct {
	Context context.Context
	Spec    ComponentSpec
	Values  Resolver
	Probes  Prober
}

type Component interface {
	Start(context.Context) error
	Close(context.Context) error
}

type OutputProvider interface {
	Outputs() map[string]any
}

type HealthChecker interface {
	Health(context.Context) error
}

type ReadinessChecker interface {
	Ready(context.Context) error
}

// FailureSource reports an unexpected background failure. Closing the channel
// means the component stopped without an error. Runtime cancellation and Close
// remain the authoritative normal shutdown paths.
type FailureSource interface {
	Errors() <-chan error
}

type Resolver interface {
	Lookup(string) (any, bool)
}

func Resolve[T any](values Resolver, name string) (T, error) {
	var zero T
	if values == nil {
		return zero, fmt.Errorf("bootstrap value %q is unavailable", name)
	}
	value, ok := values.Lookup(name)
	if !ok {
		return zero, fmt.Errorf("bootstrap value %q is unavailable", name)
	}
	typed, ok := value.(T)
	if !ok {
		return zero, fmt.Errorf("bootstrap value %q has an incompatible type", name)
	}
	return typed, nil
}

type Phase string

const (
	PhaseNew      Phase = "NEW"
	PhaseStarting Phase = "STARTING"
	PhaseRunning  Phase = "RUNNING"
	PhaseFailed   Phase = "FAILED"
	PhaseClosing  Phase = "CLOSING"
	PhaseClosed   Phase = "CLOSED"
)

var (
	ErrClosed     = errors.New("bootstrap runtime is closed")
	ErrNotStarted = errors.New("bootstrap runtime has not started")
)

// ComponentError deliberately omits Cause.Error(). Callers may inspect the
// cause with errors.Unwrap/errors.Is, while ordinary structured logging cannot
// accidentally serialize a DSN, token, or secret-bearing upstream error.
type ComponentError struct {
	Component string
	Operation string
	Cause     error
}

func (err *ComponentError) Error() string {
	if err == nil {
		return "bootstrap component failed"
	}
	return fmt.Sprintf("bootstrap component %q %s failed", err.Component, err.Operation)
}

func (err *ComponentError) Unwrap() error {
	if err == nil {
		return nil
	}
	return err.Cause
}

func componentError(name, operation string, cause error) error {
	if cause == nil {
		return nil
	}
	return &ComponentError{Component: name, Operation: operation, Cause: cause}
}

func normalizeManifest(manifest Manifest) (Manifest, error) {
	components := make([]ComponentSpec, len(manifest.Components))
	for index, spec := range manifest.Components {
		spec.DependsOn = append([]string(nil), spec.DependsOn...)
		components[index] = spec
	}
	manifest.Components = components
	manifest.Service = strings.TrimSpace(manifest.Service)
	if !validToken(manifest.Service) {
		return Manifest{}, errors.New("bootstrap service identity is invalid")
	}
	if manifest.ShutdownTimeout == 0 {
		manifest.ShutdownTimeout = defaultShutdownTimeout
	}
	if manifest.ProbeTimeout == 0 {
		manifest.ProbeTimeout = defaultProbeTimeout
	}
	if manifest.ShutdownTimeout < 0 || manifest.ProbeTimeout < 0 {
		return Manifest{}, errors.New("bootstrap timeouts must not be negative")
	}
	if len(manifest.Components) == 0 {
		return Manifest{}, errors.New("bootstrap manifest has no components")
	}

	byName := make(map[string]ComponentSpec, len(manifest.Components))
	position := make(map[string]int, len(manifest.Components))
	for index := range manifest.Components {
		spec := manifest.Components[index]
		spec.Name = strings.TrimSpace(spec.Name)
		spec.Kind = Kind(strings.TrimSpace(string(spec.Kind)))
		if !validToken(spec.Name) || !validToken(string(spec.Kind)) {
			return Manifest{}, fmt.Errorf("bootstrap component at position %d has an invalid identity", index)
		}
		if _, exists := byName[spec.Name]; exists {
			return Manifest{}, fmt.Errorf("bootstrap component %q is duplicated", spec.Name)
		}
		dependencies := make([]string, 0, len(spec.DependsOn))
		seen := make(map[string]bool, len(spec.DependsOn))
		for _, dependency := range spec.DependsOn {
			dependency = strings.TrimSpace(dependency)
			if !validToken(dependency) || dependency == spec.Name || seen[dependency] {
				return Manifest{}, fmt.Errorf("bootstrap component %q has an invalid dependency", spec.Name)
			}
			seen[dependency] = true
			dependencies = append(dependencies, dependency)
		}
		spec.DependsOn = dependencies
		manifest.Components[index] = spec
		byName[spec.Name] = spec
		position[spec.Name] = index
	}
	for _, spec := range manifest.Components {
		for _, dependency := range spec.DependsOn {
			if _, exists := byName[dependency]; !exists {
				return Manifest{}, fmt.Errorf("bootstrap component %q depends on an unknown component", spec.Name)
			}
		}
	}

	ordered, err := stableTopologicalOrder(manifest.Components, position)
	if err != nil {
		return Manifest{}, err
	}
	manifest.Components = ordered
	return manifest, nil
}

func stableTopologicalOrder(specs []ComponentSpec, position map[string]int) ([]ComponentSpec, error) {
	byName := make(map[string]ComponentSpec, len(specs))
	indegree := make(map[string]int, len(specs))
	dependents := make(map[string][]string, len(specs))
	for _, spec := range specs {
		byName[spec.Name] = spec
		indegree[spec.Name] = len(spec.DependsOn)
		for _, dependency := range spec.DependsOn {
			dependents[dependency] = append(dependents[dependency], spec.Name)
		}
	}
	ready := make([]string, 0, len(specs))
	for _, spec := range specs {
		if indegree[spec.Name] == 0 {
			ready = append(ready, spec.Name)
		}
	}
	sort.SliceStable(ready, func(left, right int) bool { return position[ready[left]] < position[ready[right]] })
	ordered := make([]ComponentSpec, 0, len(specs))
	for len(ready) > 0 {
		name := ready[0]
		ready = ready[1:]
		ordered = append(ordered, byName[name])
		for _, dependent := range dependents[name] {
			indegree[dependent]--
			if indegree[dependent] == 0 {
				ready = append(ready, dependent)
				sort.SliceStable(ready, func(left, right int) bool { return position[ready[left]] < position[ready[right]] })
			}
		}
	}
	if len(ordered) != len(specs) {
		return nil, errors.New("bootstrap component dependencies contain a cycle")
	}
	return ordered, nil
}

func validToken(value string) bool {
	if value == "" || len(value) > 128 {
		return false
	}
	for index, character := range value {
		switch {
		case character >= 'a' && character <= 'z', character >= 'A' && character <= 'Z', character >= '0' && character <= '9':
		case (character == '-' || character == '_' || character == '.') && index > 0 && index < len(value)-1:
		default:
			return false
		}
	}
	return true
}
