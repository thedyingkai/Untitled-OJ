package bootstrap

import (
	"context"
	"errors"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"ojos-shared/resourceoutput"
)

const ValuePostgreSQL = "platform.postgresql"

type PostgreSQLOptions struct {
	ResourceOutputFile string
	ConnectTimeout     time.Duration
	ValueName          string
}

func NewPostgreSQLFactory(options PostgreSQLOptions) Factory {
	return FactoryFunc(func(BuildContext) (Component, error) {
		dsn, err := resourceoutput.ReadPostgreSQLDSN(options.ResourceOutputFile)
		if err != nil {
			return nil, errors.New("read PostgreSQL resource output")
		}
		configuration, err := pgxpool.ParseConfig(dsn)
		if err != nil {
			return nil, errors.New("parse PostgreSQL resource output")
		}
		timeout := options.ConnectTimeout
		if timeout == 0 {
			timeout = 5 * time.Second
		}
		if timeout < 0 {
			return nil, errors.New("PostgreSQL connect timeout is invalid")
		}
		configuration.ConnConfig.ConnectTimeout = timeout
		name := defaultValueName(options.ValueName, ValuePostgreSQL)
		if !validToken(name) {
			return nil, errors.New("PostgreSQL output name is invalid")
		}
		return &postgresqlComponent{configuration: configuration, valueName: name}, nil
	})
}

type postgresqlComponent struct {
	configuration *pgxpool.Config
	valueName     string
	pool          *pgxpool.Pool
}

func (component *postgresqlComponent) Start(ctx context.Context) error {
	pool, err := pgxpool.NewWithConfig(ctx, component.configuration)
	if err != nil {
		return errors.New("connect PostgreSQL resource")
	}
	if err := pool.Ping(ctx); err != nil {
		pool.Close()
		return errors.New("probe PostgreSQL resource")
	}
	component.pool = pool
	return nil
}

func (component *postgresqlComponent) Close(context.Context) error {
	if component.pool != nil {
		component.pool.Close()
	}
	return nil
}

func (component *postgresqlComponent) Ready(ctx context.Context) error {
	if component.pool == nil {
		return errors.New("PostgreSQL resource is not started")
	}
	if err := component.pool.Ping(ctx); err != nil {
		return errors.New("PostgreSQL resource is unavailable")
	}
	return nil
}

func (component *postgresqlComponent) Outputs() map[string]any {
	if component.pool == nil {
		return nil
	}
	return map[string]any{component.valueName: component.pool}
}
