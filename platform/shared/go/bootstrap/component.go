package bootstrap

import "context"

// ComponentFuncs is a compact adapter for domain composition roots and tests.
// Values are published only after StartFunc succeeds.
type ComponentFuncs struct {
	StartFunc  func(context.Context) error
	CloseFunc  func(context.Context) error
	HealthFunc func(context.Context) error
	ReadyFunc  func(context.Context) error
	ErrorsC    <-chan error
	Values     map[string]any
}

func (component *ComponentFuncs) Start(ctx context.Context) error {
	if component == nil || component.StartFunc == nil {
		return nil
	}
	return component.StartFunc(ctx)
}

func (component *ComponentFuncs) Close(ctx context.Context) error {
	if component == nil || component.CloseFunc == nil {
		return nil
	}
	return component.CloseFunc(ctx)
}

func (component *ComponentFuncs) Health(ctx context.Context) error {
	if component == nil || component.HealthFunc == nil {
		return nil
	}
	return component.HealthFunc(ctx)
}

func (component *ComponentFuncs) Ready(ctx context.Context) error {
	if component == nil || component.ReadyFunc == nil {
		return nil
	}
	return component.ReadyFunc(ctx)
}

func (component *ComponentFuncs) Errors() <-chan error {
	if component == nil {
		return nil
	}
	return component.ErrorsC
}

func (component *ComponentFuncs) Outputs() map[string]any {
	if component == nil || len(component.Values) == 0 {
		return nil
	}
	values := make(map[string]any, len(component.Values))
	for name, value := range component.Values {
		values[name] = value
	}
	return values
}
