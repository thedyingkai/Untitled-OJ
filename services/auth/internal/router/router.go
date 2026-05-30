package router

import (
	"net/http"

	"ojos-auth/internal/app"
	"ojos-auth/internal/handler"
	authmw "ojos-auth/internal/middleware"
	"ojos-auth/internal/repository"
	"ojos-auth/internal/service"

	"github.com/zeromicro/go-zero/rest"
)

func Register(server *rest.Server, a *app.App) {
	userRepo := repository.NewUserRepository(a.DB)
	authService := service.NewAuthService(
		userRepo,
		a.EventBus,
		a.Cfg.JWT.Secret,
		a.Cfg.JWT.ExpireHours,
	)

	server.AddRoute(rest.Route{
		Method:  http.MethodGet,
		Path:    "/health",
		Handler: handler.Health(a.EventBus),
	})

	server.AddRoute(rest.Route{
		Method:  http.MethodPost,
		Path:    "/auth/register",
		Handler: handler.Register(authService),
	})

	server.AddRoute(rest.Route{
		Method:  http.MethodPost,
		Path:    "/auth/login",
		Handler: handler.Login(authService),
	})

	server.AddRoute(rest.Route{
		Method:  http.MethodGet,
		Path:    "/auth/profile",
		Handler: authmw.JWT(a.Cfg.JWT.Secret, handler.Profile()),
	})
}
