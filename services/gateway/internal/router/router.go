package router

import (
	"net/http"

	"ojos-gateway/internal/app"
	"ojos-gateway/internal/handler"

	"github.com/zeromicro/go-zero/rest"
)

func Register(server *rest.Server, a *app.App) {
	server.AddRoute(rest.Route{
		Method:  http.MethodGet,
		Path:    "/health",
		Handler: handler.Health(a.EventBus),
	})
}
