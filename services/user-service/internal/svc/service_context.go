// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"ojos-user-service/internal/config"
	"ojos-user-service/internal/store"
)

type ServiceContext struct {
	Config       config.Config
	ProfileStore *store.ProfileStore
}

func NewServiceContext(c config.Config) *ServiceContext {
	profileStore, err := store.NewProfileStore(c.Storage.ProfilesRoot)
	if err != nil {
		panic(err)
	}
	return &ServiceContext{
		Config:       c,
		ProfileStore: profileStore,
	}
}
