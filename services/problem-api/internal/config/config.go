// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
	rest.RestConf

	Database DatabaseConfig
	Jaeger   JaegerConfig
	Storage  StorageConfig
}

type DatabaseConfig struct {
	Url string
}

type JaegerConfig struct {
	Endpoint string
}

type StorageConfig struct {
	ProblemsRoot string
}
