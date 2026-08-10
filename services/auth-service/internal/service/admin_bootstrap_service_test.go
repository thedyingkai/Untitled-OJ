package service

import (
	"context"
	"errors"
	"strings"
	"testing"
)

type fakeAdminBootstrapStore struct {
	calls        int
	username     string
	email        string
	passwordHash string
	userID       int64
	err          error
}

func (f *fakeAdminBootstrapStore) BootstrapAdmin(
	_ context.Context,
	username string,
	email string,
	passwordHash string,
) (int64, error) {
	f.calls++
	f.username = username
	f.email = email
	f.passwordHash = passwordHash
	return f.userID, f.err
}

func TestAdminBootstrapRejectsWrongSecretBeforeDatabaseOrPasswordHash(t *testing.T) {
	store := &fakeAdminBootstrapStore{}
	service, err := NewAdminBootstrapService(store, []byte(strings.Repeat("correct-", 4)))
	if err != nil {
		t.Fatal(err)
	}
	_, err = service.Bootstrap(t.Context(), AdminBootstrapRequest{
		Secret:   strings.Repeat("wrong---", 4),
		Username: "initial-admin",
		Password: "correct horse battery staple",
	})
	if !errors.Is(err, ErrInvalidAdminBootstrapSecret) {
		t.Fatalf("expected invalid secret, got %v", err)
	}
	if store.calls != 0 {
		t.Fatalf("invalid secret reached database %d times", store.calls)
	}
}

func TestAdminBootstrapCreatesLoginCapableAdministrator(t *testing.T) {
	store := &fakeAdminBootstrapStore{userID: 42}
	secret := strings.Repeat("bootstrap-", 4)
	service, err := NewAdminBootstrapService(store, []byte(secret))
	if err != nil {
		t.Fatal(err)
	}
	result, err := service.Bootstrap(t.Context(), AdminBootstrapRequest{
		Secret:   secret,
		Username: "  initial-admin ",
		Email:    "  admin@example.test ",
		Password: "correct horse battery staple",
	})
	if err != nil {
		t.Fatal(err)
	}
	if result.UserID != 42 || result.Username != "initial-admin" {
		t.Fatalf("unexpected result: %#v", result)
	}
	if store.calls != 1 || store.username != "initial-admin" || store.email != "admin@example.test" {
		t.Fatalf("unexpected store call: %#v", store)
	}
	if store.passwordHash == "correct horse battery staple" || store.passwordHash == "" {
		t.Fatal("bootstrap store did not receive a password hash")
	}
}
