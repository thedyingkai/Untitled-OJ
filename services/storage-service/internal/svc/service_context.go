// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"ojos-storage-service/internal/config"
	"ojos-storage-service/internal/store"
)

type ServiceContext struct {
	Config      config.Config
	ObjectStore *store.ObjectStore
}

func NewServiceContext(c config.Config) *ServiceContext {
	objectStore, err := store.NewObjectStore(c.Storage.Root, c.Storage.Buckets)
	if err != nil {
		panic(err)
	}
	return &ServiceContext{
		Config:      c,
		ObjectStore: objectStore,
	}
}
