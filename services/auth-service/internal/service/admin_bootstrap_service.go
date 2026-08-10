package service

import (
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"errors"
	"fmt"

	"ojos-auth-service/internal/repository"
)

var ErrInvalidAdminBootstrapSecret = errors.New("invalid initial administrator bootstrap secret")

type AdminBootstrapStore interface {
	BootstrapAdmin(ctx context.Context, username string, email string, passwordHash string) (int64, error)
}

type AdminBootstrapService struct {
	store        AdminBootstrapStore
	secretDigest [sha256.Size]byte
}

type AdminBootstrapRequest struct {
	Secret   string
	Username string
	Email    string
	Password string
}

type AdminBootstrapResult struct {
	UserID   int64
	Username string
}

func NewAdminBootstrapService(store AdminBootstrapStore, secret []byte) (*AdminBootstrapService, error) {
	if store == nil {
		return nil, errors.New("admin bootstrap store is required")
	}
	if len(secret) < 32 || len(secret) > 512 {
		return nil, errors.New("admin bootstrap secret must contain between 32 and 512 bytes")
	}
	return &AdminBootstrapService{
		store:        store,
		secretDigest: sha256.Sum256(secret),
	}, nil
}

func (s *AdminBootstrapService) Bootstrap(
	ctx context.Context,
	req AdminBootstrapRequest,
) (*AdminBootstrapResult, error) {
	providedDigest := sha256.Sum256([]byte(req.Secret))
	if subtle.ConstantTimeCompare(providedDigest[:], s.secretDigest[:]) != 1 {
		return nil, ErrInvalidAdminBootstrapSecret
	}

	username, email, passwordHash, err := normalizeAndHashNewUser(req.Username, req.Email, req.Password)
	if err != nil {
		return nil, err
	}
	userID, err := s.store.BootstrapAdmin(ctx, username, email, passwordHash)
	if err != nil {
		return nil, fmt.Errorf("bootstrap initial administrator: %w", err)
	}
	return &AdminBootstrapResult{UserID: userID, Username: username}, nil
}

func IsAdminBootstrapConsumed(err error) bool {
	return errors.Is(err, repository.ErrAdminBootstrapConsumed) ||
		errors.Is(err, repository.ErrAdminBootstrapAlreadyInitialized)
}

func IsAdminBootstrapUserExists(err error) bool {
	return errors.Is(err, repository.ErrAdminBootstrapUserExists)
}
