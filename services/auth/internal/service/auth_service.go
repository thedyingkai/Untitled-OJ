package service

import (
	"context"
	"errors"
	"fmt"
	"ojos-auth/internal/token"
	"strings"

	"ojos-auth/internal/repository"

	"golang.org/x/crypto/bcrypt"
)

var (
	ErrInvalidInput = errors.New("invalid input")
	ErrUserExists   = errors.New("user already exists")
)

type AuthService struct {
	userRepo       *repository.UserRepository
	jwtSecret      string
	jwtExpireHours int
}

func NewAuthService(
	userRepo *repository.UserRepository,
	jwtSecret string,
	jwtExpireHours int,
) *AuthService {
	return &AuthService{
		userRepo:       userRepo,
		jwtSecret:      jwtSecret,
		jwtExpireHours: jwtExpireHours,
	}
}

type RegisterRequest struct {
	Username string
	Email    string
	Password string
}

type RegisterResult struct {
	UserID   int64  `json:"user_id"`
	Username string `json:"username"`
}

func (s *AuthService) Register(ctx context.Context, req RegisterRequest) (*RegisterResult, error) {
	username := strings.TrimSpace(req.Username)
	email := strings.TrimSpace(req.Email)
	password := req.Password

	if username == "" {
		return nil, fmt.Errorf("%w: username is required", ErrInvalidInput)
	}

	if len(username) < 3 || len(username) > 32 {
		return nil, fmt.Errorf("%w: username length must be between 3 and 32", ErrInvalidInput)
	}

	if len(password) < 6 {
		return nil, fmt.Errorf("%w: password length must be at least 6", ErrInvalidInput)
	}

	hashBytes, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
	if err != nil {
		return nil, err
	}

	userID, err := s.userRepo.CreateUserWithDefaultRole(
		ctx,
		username,
		email,
		string(hashBytes),
	)

	if err != nil {
		if errors.Is(err, repository.ErrUserExists) {
			return nil, ErrUserExists
		}

		return nil, err
	}

	return &RegisterResult{
		UserID:   userID,
		Username: username,
	}, nil
}

var ErrInvalidCredentials = errors.New("invalid credentials")

type LoginRequest struct {
	Username string
	Password string
}

type LoginResult struct {
	Token    string   `json:"token"`
	UserID   int64    `json:"user_id"`
	Username string   `json:"username"`
	Roles    []string `json:"roles"`
}

func (s *AuthService) Login(ctx context.Context, req LoginRequest) (*LoginResult, error) {
	username := strings.TrimSpace(req.Username)
	password := req.Password

	if username == "" || password == "" {
		return nil, ErrInvalidCredentials
	}

	userID, passwordHash, err := s.userRepo.GetByUsername(ctx, username)
	if err != nil {
		if errors.Is(err, repository.ErrUserNotFound) {
			return nil, ErrInvalidCredentials
		}

		return nil, err
	}

	if err := bcrypt.CompareHashAndPassword([]byte(passwordHash), []byte(password)); err != nil {
		return nil, ErrInvalidCredentials
	}

	roles, err := s.userRepo.GetRolesByUserID(ctx, userID)
	if err != nil {
		return nil, err
	}

	tokenString, err := token.Generate(
		s.jwtSecret,
		s.jwtExpireHours,
		userID,
		username,
		roles,
	)

	if err != nil {
		return nil, err
	}

	return &LoginResult{
		Token:    tokenString,
		UserID:   userID,
		Username: username,
		Roles:    roles,
	}, nil
}
