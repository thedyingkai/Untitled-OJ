package middleware

import (
	"encoding/json"
	"errors"
	"net/http"

	"ojos-shared/security/internalauth"
)

type InternalAuthMiddleware struct {
	enabled  bool
	verifier *internalauth.Verifier
}

func NewInternalAuthMiddleware(
	enabled bool,
	verifier *internalauth.Verifier,
) *InternalAuthMiddleware {
	return &InternalAuthMiddleware{
		enabled:  enabled,
		verifier: verifier,
	}
}

func (m *InternalAuthMiddleware) Handle(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !m.enabled {
			next(w, r)
			return
		}

		if m.verifier == nil {
			writeInternalAuthError(w, http.StatusUnauthorized, 40120, "internal auth verifier not configured")
			return
		}

		if err := m.verifier.VerifyRequest(r.Context(), r); err != nil {
			writeInternalAuthError(w, http.StatusUnauthorized, 40121, internalAuthErrorMessage(err))
			return
		}

		next(w, r)
	}
}

func internalAuthErrorMessage(err error) string {
	switch {
	case errors.Is(err, internalauth.ErrMissingInternalAuth):
		return "missing internal auth"
	case errors.Is(err, internalauth.ErrInvalidSignature):
		return "invalid internal signature"
	case errors.Is(err, internalauth.ErrInvalidTimestamp):
		return "invalid internal timestamp"
	case errors.Is(err, internalauth.ErrTimestampSkew):
		return "internal auth timestamp skew exceeded"
	case errors.Is(err, internalauth.ErrReplay):
		return "internal auth nonce replay"
	case errors.Is(err, internalauth.ErrKeyNotFound):
		return "internal auth key not found"
	case errors.Is(err, internalauth.ErrInvalidBodyHash):
		return "invalid internal body hash"
	default:
		return "invalid internal auth"
	}
}

func writeInternalAuthError(w http.ResponseWriter, httpStatus int, code int, msg string) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(httpStatus)

	_ = json.NewEncoder(w).Encode(map[string]any{
		"code": code,
		"msg":  msg,
	})
}
