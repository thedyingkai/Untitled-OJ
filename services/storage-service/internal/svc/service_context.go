// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"ojos-storage-service/internal/config"
	"ojos-storage-service/internal/store"
)

type ServiceContext struct {
	Config      config.Config
	ObjectStore store.ObjectStorage
}

func NewServiceContext(c config.Config) *ServiceContext {
	objectStore, err := store.NewObjectStorage(store.Options{
		Backend: c.Storage.Backend,
		Root:    c.Storage.Root,
		Buckets: c.Storage.Buckets,
		MinIO: store.MinIOOptions{
			Endpoint:  c.Storage.MinIO.Endpoint,
			AccessKey: c.Storage.MinIO.AccessKey,
			SecretKey: c.Storage.MinIO.SecretKey,
			UseSSL:    c.Storage.MinIO.UseSSL,
		},
	})
	if err != nil {
		panic(err)
	}
	return &ServiceContext{
		Config:      c,
		ObjectStore: objectStore,
	}
}
