package contest

import "context"

type Repository interface {
	Ping(context.Context) error
	List(context.Context) ([]Contest, error)
	Get(context.Context, int64) (Contest, error)
	Create(context.Context, CreateInput) (Contest, error)
	Update(context.Context, int64, UpdateInput) (Contest, error)
	Delete(context.Context, int64) error
}
