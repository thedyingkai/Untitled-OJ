package configs

type Config struct {
	Service  ServiceConfig  `mapstructure:"service"`
	Database DatabaseConfig `mapstructure:"database"`
	Jaeger   JaegerConfig   `mapstructure:"jaeger"`
	Nats     NatsConfig     `mapstructure:"nats"`
	JWT      JWTConfig      `mapstructure:"jwt"`
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

type NatsConfig struct {
	URL string `mapstructure:"url"`
}

type JWTConfig struct {
	Secret      string `mapstructure:"secret"`
	ExpireHours int    `mapstructure:"expire_hours"`
}
