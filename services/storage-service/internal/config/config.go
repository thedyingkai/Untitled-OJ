// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

const minimumObjectStreamNetworkTimeoutMS int64 = 600000

type Config struct {
	rest.RestConf

	Storage StorageConfig
	Jaeger  JaegerConfig
}

type JaegerConfig struct {
	Endpoint string
}

type StorageConfig struct {
	Backend string
	Root    string
	Buckets []string
	MinIO   MinIOConfig
}

type MinIOConfig struct {
	Endpoint  string
	AccessKey string
	SecretKey string
	UseSSL    bool
}

// PrepareObjectStreaming prevents the framework timeout middleware from
// buffering complete object bodies. Gateway/client contexts retain the
// operation-specific deadline and this value remains the socket-level bound.
func (c *Config) PrepareObjectStreaming() {
	if c.Timeout < minimumObjectStreamNetworkTimeoutMS {
		c.Timeout = minimumObjectStreamNetworkTimeoutMS
	}
	c.Middlewares.Timeout = false
	c.Middlewares.Recover = false
}
