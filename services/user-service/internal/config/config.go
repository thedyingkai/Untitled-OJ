// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
	rest.RestConf

	Database    DatabaseConfig
	Storage     StorageConfig
	AuthService AuthServiceConfig
}

type DatabaseConfig struct {
	Url string
}

type StorageConfig struct {
	ProfilesRoot string
}

type AuthServiceConfig struct {
	Endpoint   string `json:",optional"`
	AdminToken string `json:",optional"`
}
