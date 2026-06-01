// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
	rest.RestConf

	Database DatabaseConfig
	Jaeger   JaegerConfig
	Jwt      JwtConfig
	Proxy    ProxyConfig
}

type DatabaseConfig struct {
	Url string
}

type JaegerConfig struct {
	Endpoint string
}

type JwtConfig struct {
	Secret string
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
