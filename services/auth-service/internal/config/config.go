// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
	rest.RestConf

	Database     DatabaseConfig
	Jaeger       JaegerConfig
	Jwt          JwtConfig
	InternalAuth InternalAuthConfig
}

type DatabaseConfig struct {
	Url string
}

type JaegerConfig struct {
	Endpoint string
}

type JwtConfig struct {
	Secret      string
	ExpireHours int
}

type InternalAuthConfig struct {
	Token string `json:",optional"`
}
