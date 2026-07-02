// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
	rest.RestConf

	Database     DatabaseConfig
	Redis        RedisConfig
	Jaeger       JaegerConfig
	Storage      StorageConfig
	InternalAuth InternalAuthConfig
}

type DatabaseConfig struct {
	Url string
}

type RedisConfig struct {
	Url string
}

type JaegerConfig struct {
	Endpoint string
}

type StorageConfig struct {
	ProblemsRoot            string
	ServiceEndpoint         string `json:",optional"`
	InternalGatewayEndpoint string `json:",optional"`
	PutApiID                string `json:",optional"`
	Bucket                  string `json:",optional"`
	ServiceToken            string `json:",optional"`
	CallerService           string `json:",optional"`
	CallerNodeID            string `json:",optional"`
}

type InternalAuthConfig struct {
	Enabled              bool
	TimestampSkewSeconds int64 `json:",optional"`
	NonceTTLSeconds      int64 `json:",optional"`
}
