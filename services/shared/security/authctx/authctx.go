package authctx

import (
	"context"
	"errors"
	"net/http"
	"strconv"
	"strings"
)

const (
	HeaderAuthVerified = "X-Auth-Verified"
	HeaderUserID       = "X-User-Id"
	HeaderUsername     = "X-Username"
	HeaderRoles        = "X-Roles"
)

var (
	ErrUnauthenticated = errors.New("unauthenticated")
	ErrInvalidUserID   = errors.New("invalid user id")
)

type UserContext struct {
	UserID   int64
	Username string
	Roles    []string
}

type contextKey struct{}

func FromHeaders(header http.Header) (*UserContext, error) {
	if !strings.EqualFold(strings.TrimSpace(header.Get(HeaderAuthVerified)), "true") {
		return nil, ErrUnauthenticated
	}

	userIDText := strings.TrimSpace(header.Get(HeaderUserID))
	if userIDText == "" {
		return nil, ErrInvalidUserID
	}

	userID, err := strconv.ParseInt(userIDText, 10, 64)
	if err != nil || userID <= 0 {
		return nil, ErrInvalidUserID
	}

	return &UserContext{
		UserID:   userID,
		Username: strings.TrimSpace(header.Get(HeaderUsername)),
		Roles:    parseRoles(header.Get(HeaderRoles)),
	}, nil
}

func NewContext(ctx context.Context, user *UserContext) context.Context {
	return context.WithValue(ctx, contextKey{}, user)
}

func FromContext(ctx context.Context) (*UserContext, bool) {
	user, ok := ctx.Value(contextKey{}).(*UserContext)
	return user, ok
}

func parseRoles(raw string) []string {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return nil
	}

	parts := strings.Split(raw, ",")
	roles := make([]string, 0, len(parts))

	for _, part := range parts {
		role := strings.TrimSpace(part)
		if role != "" {
			roles = append(roles, role)
		}
	}

	return roles
}
