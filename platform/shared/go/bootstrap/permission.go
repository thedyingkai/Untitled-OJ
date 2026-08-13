package bootstrap

import (
	"context"
	"errors"
	"os"

	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"
)

const (
	ValueServiceContext    = "platform.service-context"
	ValuePermissionChecker = "platform.permission-checker"
)

type PermissionOptions struct {
	Service         string
	ContextFile     string
	Managed         bool
	BindingName     string
	ProviderValue   string
	CheckerValue    string
	ProviderOptions servicecontext.ProviderOptions
}

func NewPermissionFactory(options PermissionOptions) Factory {
	return FactoryFunc(func(BuildContext) (Component, error) {
		_, err := os.Stat(options.ContextFile)
		if errors.Is(err, os.ErrNotExist) && !options.Managed {
			return &ComponentFuncs{}, nil
		}
		if err != nil {
			return nil, errors.New("inspect managed service context")
		}
		provider, err := servicecontext.NewContextProvider(options.ContextFile, options.ProviderOptions)
		if err != nil {
			return nil, errors.New("load managed service context")
		}
		snapshot, err := provider.Current(context.Background())
		if err != nil || snapshot.RequireService(options.Service) != nil {
			_ = provider.Close()
			return nil, errors.New("managed service context identity is invalid")
		}
		checker, err := sharedperm.NewContextProviderUserChecker(provider, options.BindingName)
		if err != nil {
			_ = provider.Close()
			return nil, errors.New("configure managed permission binding")
		}
		providerName := defaultValueName(options.ProviderValue, ValueServiceContext)
		checkerName := defaultValueName(options.CheckerValue, ValuePermissionChecker)
		if !validToken(providerName) || !validToken(checkerName) || providerName == checkerName {
			_ = provider.Close()
			return nil, errors.New("permission output names are invalid")
		}
		return &permissionComponent{
			provider: provider,
			outputs:  map[string]any{providerName: provider, checkerName: checker},
		}, nil
	})
}

// NewServiceContextFactory installs only the rotating Service Context. It is
// useful for services that need generated API clients but do not contribute
// operation permissions. NewPermissionFactory composes this provider with the
// standard permission checker for the common case.
func NewServiceContextFactory(options PermissionOptions) Factory {
	return FactoryFunc(func(BuildContext) (Component, error) {
		_, err := os.Stat(options.ContextFile)
		if errors.Is(err, os.ErrNotExist) && !options.Managed {
			return &ComponentFuncs{}, nil
		}
		if err != nil {
			return nil, errors.New("inspect managed service context")
		}
		provider, err := servicecontext.NewContextProvider(options.ContextFile, options.ProviderOptions)
		if err != nil {
			return nil, errors.New("load managed service context")
		}
		snapshot, err := provider.Current(context.Background())
		if err != nil || snapshot.RequireService(options.Service) != nil {
			_ = provider.Close()
			return nil, errors.New("managed service context identity is invalid")
		}
		providerName := defaultValueName(options.ProviderValue, ValueServiceContext)
		if !validToken(providerName) {
			_ = provider.Close()
			return nil, errors.New("service context output name is invalid")
		}
		return &permissionComponent{provider: provider, outputs: map[string]any{providerName: provider}}, nil
	})
}

type permissionComponent struct {
	provider *servicecontext.ContextProvider
	outputs  map[string]any
}

func (component *permissionComponent) Start(ctx context.Context) error {
	if err := component.provider.Start(ctx); err != nil {
		return errors.New("start managed service context")
	}
	return nil
}

func (component *permissionComponent) Close(context.Context) error {
	if err := component.provider.Close(); err != nil {
		return errors.New("close managed service context")
	}
	return nil
}

func (component *permissionComponent) Outputs() map[string]any {
	result := make(map[string]any, len(component.outputs))
	for name, value := range component.outputs {
		result[name] = value
	}
	return result
}
