package storage

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"strings"

	"ojos-shared/servicecontext"
)

const (
	storagePutBinding  = "storage.object.put"
	storageHeadBinding = "storage.object.head"
)

type managedStorageClient struct {
	provider *servicecontext.ContextProvider
}

func loadManagedStorageClient() (*managedStorageClient, error) {
	serviceContext, err := servicecontext.LoadOptional()
	if err != nil {
		return nil, err
	}
	if serviceContext == nil {
		return nil, nil
	}
	if err := serviceContext.RequireService("problem-service"); err != nil {
		return nil, err
	}
	for _, requirement := range []string{storagePutBinding, storageHeadBinding} {
		if _, err := serviceContext.Binding(requirement); err != nil {
			return nil, fmt.Errorf("problem-service managed storage: %w", err)
		}
	}
	path := strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE"))
	if path == "" {
		path = servicecontext.DefaultFile
	}
	provider, err := servicecontext.NewContextProvider(path, servicecontext.ProviderOptions{})
	if err != nil {
		return nil, err
	}
	return &managedStorageClient{provider: provider}, nil
}

func (client *managedStorageClient) close() {
	if client != nil && client.provider != nil {
		_ = client.provider.Close()
	}
}

func (client *managedStorageClient) snapshot(ctx context.Context) (servicecontext.ServiceContext, *http.Client, error) {
	if client == nil || client.provider == nil {
		return servicecontext.ServiceContext{}, nil, fmt.Errorf("managed storage client is unavailable")
	}
	_ = client.provider.ReloadNow()
	snapshot, err := client.provider.Current(ctx)
	if err != nil {
		return servicecontext.ServiceContext{}, nil, err
	}
	if err := snapshot.RequireService("problem-service"); err != nil {
		return servicecontext.ServiceContext{}, nil, err
	}
	for _, requirement := range []string{storagePutBinding, storageHeadBinding} {
		binding, err := snapshot.Binding(requirement)
		if err != nil || binding.APIID != requirement {
			return servicecontext.ServiceContext{}, nil, fmt.Errorf("problem-service managed storage binding %s is unavailable", requirement)
		}
	}
	httpClient, err := snapshot.Client()
	return snapshot, httpClient, err
}
