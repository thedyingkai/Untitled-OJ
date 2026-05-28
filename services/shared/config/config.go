package config

type Config struct {
	Service  ServiceConfig  `mapstructure:"service"`
	Database DatabaseConfig `mapstructure:"database"`
	Jaeger   JaegerConfig   `mapstructure:"jaeger"`
}

type ServiceConfig struct {
	Name string `mapstructure:"name"`
	Port int    `mapstructure:"port"`
}

type DatabaseConfig struct {
	URL string `mapstructure:"url"`
}

type JaegerConfig struct {
	Endpoint string `mapstructure:"endpoint"`
}
