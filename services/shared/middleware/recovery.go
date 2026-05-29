package middleware

import (
	"net/http"
	"runtime/debug"

	"ojos-shared/logger"

	"go.uber.org/zap"
)

func Recovery(log *zap.Logger, next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		defer func() {
			if err := recover(); err != nil {
				logger.WithTrace(r.Context(), log).Error(
					"panic recovered",
					zap.Any("error", err),
					zap.String("stack", string(debug.Stack())),
				)

				http.Error(w, "internal server error", http.StatusInternalServerError)
			}
		}()

		next(w, r)
	}
}
