package config

import (
	"github.com/spf13/viper"
)

func Load() (*Config, error) {
	v := viper.New()

	v.SetConfigName("configs")
	v.SetConfigType("yaml")

	v.AddConfigPath("./configs")

	v.AutomaticEnv()

	if err := v.ReadInConfig(); err != nil {
		return nil, err
	}

	var cfg Config

	if err := v.Unmarshal(&cfg); err != nil {
		return nil, err
	}

	return &cfg, nil
}
