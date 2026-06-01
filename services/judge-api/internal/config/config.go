// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
	rest.RestConf

	Database DatabaseConfig
	Redis    RedisConfig
}

type DatabaseConfig struct {
	Url string
}

type RedisConfig struct {
	Url string
}
