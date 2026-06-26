package middleware

import (
	"context"
	"errors"
	"net/http"
	"strings"

	"ojos-shared/security/authctx"
	"ojos-shared/security/permission"

	"github.com/jackc/pgx/v5"
	"github.com/zeromicro/go-zero/rest/httpx"
)

func InstallHTTPErrorHandler() {
	httpx.SetErrorHandlerCtx(func(_ context.Context, err error) (int, any) {
		status, code, msg := classifyHTTPError(err)
		return status, map[string]any{
			"code": code,
			"msg":  msg,
		}
	})
}

func classifyHTTPError(err error) (int, int, string) {
	if err == nil {
		return http.StatusInternalServerError, 50000, "internal server error"
	}

	switch {
	case errors.Is(err, authctx.ErrUnauthenticated), errorMessageIs(err, "unauthorized"):
		return http.StatusUnauthorized, 40100, "unauthorized"
	case errors.Is(err, authctx.ErrInvalidUserID), errorMessageContains(err, "missing authorization header"),
		errorMessageContains(err, "invalid authorization header"),
		errorMessageContains(err, "invalid or expired token"):
		return http.StatusUnauthorized, 40100, sanitizeErrorMessage(err)
	case errors.Is(err, permission.ErrForbidden), errorMessageContains(err, "forbidden"):
		return http.StatusForbidden, 40300, "forbidden"
	case errors.Is(err, pgx.ErrNoRows), errorMessageContains(err, "not found"):
		return http.StatusNotFound, 40400, sanitizeErrorMessage(err)
	default:
		return http.StatusBadRequest, 40000, sanitizeErrorMessage(err)
	}
}

func errorMessageIs(err error, want string) bool {
	return strings.EqualFold(strings.TrimSpace(err.Error()), want)
}

func errorMessageContains(err error, needle string) bool {
	return strings.Contains(strings.ToLower(err.Error()), strings.ToLower(needle))
}

func sanitizeErrorMessage(err error) string {
	msg := strings.TrimSpace(err.Error())
	if msg == "" {
		return "request failed"
	}
	return msg
}
