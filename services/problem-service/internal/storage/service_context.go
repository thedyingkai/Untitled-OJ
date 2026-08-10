package storage

import (
	"fmt"
	"net/http"

	"ojos-shared/servicecontext"
)

const (
	storagePutBinding  = "storage_put"
	storageHeadBinding = "storage_head"
)

type managedStorageClient struct {
	context servicecontext.ServiceContext
	client  *http.Client
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
	client, err := serviceContext.Client()
	if err != nil {
		return nil, err
	}
	return &managedStorageClient{context: *serviceContext, client: client}, nil
}
