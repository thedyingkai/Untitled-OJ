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
	Submission   SubmissionConfig
	Languages    LanguagesConfig
	WorkerAuth   WorkerAuthConfig
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
	SubmissionsRoot         string
	ServiceEndpoint         string `json:",optional"`
	InternalGatewayEndpoint string `json:",optional"`
	GetApiID                string `json:",optional"`
	PutApiID                string `json:",optional"`
	HeadApiID               string `json:",optional"`
	Bucket                  string `json:",optional"`
	CallerService           string `json:",optional"`
	CallerNodeID            string `json:",optional"`
	ServiceToken            string `json:",optional"`
}

type SubmissionConfig struct {
	MaxCodeBytes int64 `json:",optional"`
}

type LanguagesConfig struct {
	Items []LanguageConfig `json:",optional"`
}

type LanguageConfig struct {
	Id          string
	DisplayName string
	Version     string `json:",optional"`
	Enabled     bool
}

type WorkerAuthConfig struct {
	Token           string `json:",optional"`
	LeaseTTLSeconds int64  `json:",optional"`
}

type InternalAuthConfig struct {
	Enabled              bool
	TimestampSkewSeconds int64 `json:",optional"`
	NonceTTLSeconds      int64 `json:",optional"`
}
