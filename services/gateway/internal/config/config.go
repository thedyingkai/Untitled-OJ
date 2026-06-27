// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
	rest.RestConf

	Database     DatabaseConfig
	Redis        RedisConfig
	Jaeger       JaegerConfig
	Jwt          JwtConfig
	Storage      StorageConfig
	Proxy        ProxyConfig
	InternalAuth InternalAuthConfig
	Installer    InstallerConfig
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

type JwtConfig struct {
	Secret string
}

type StorageConfig struct {
	ProblemsRoot    string `json:",optional"`
	SubmissionsRoot string `json:",optional"`
}

type ProxyConfig struct {
	Routes []ProxyRouteConfig
}

type ProxyRouteConfig struct {
	Prefix      string
	Target      string
	StripPrefix string `json:",optional"`
	AuthMode    string `json:",optional"`
}

type InternalAuthConfig struct {
	Enabled                 bool
	RotationIntervalSeconds int64 `json:",optional"`
	VerifyGraceSeconds      int64 `json:",optional"`
	RotateBeforeSeconds     int64 `json:",optional"`
	TimestampSkewSeconds    int64 `json:",optional"`
	NonceTTLSeconds         int64 `json:",optional"`
}

type InstallerConfig struct {
	Endpoint      string
	InternalToken string
}
