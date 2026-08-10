package service

import (
	"context"
	"errors"
	"fmt"
	"ojos-auth-service/internal/token"
	"strings"

	"ojos-auth-service/internal/repository"

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
	username, email, passwordHash, err := normalizeAndHashNewUser(req.Username, req.Email, req.Password)
	if err != nil {
		return nil, err
	}

	userID, err := s.userRepo.CreateUserWithDefaultRole(
		ctx,
		username,
		email,
		passwordHash,
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

func normalizeAndHashNewUser(username string, email string, password string) (string, string, string, error) {
	username = strings.TrimSpace(username)
	email = strings.TrimSpace(email)
	if username == "" {
		return "", "", "", fmt.Errorf("%w: username is required", ErrInvalidInput)
	}
	if len(username) < 3 || len(username) > 32 {
		return "", "", "", fmt.Errorf("%w: username length must be between 3 and 32", ErrInvalidInput)
	}
	if len(password) < 6 {
		return "", "", "", fmt.Errorf("%w: password length must be at least 6", ErrInvalidInput)
	}
	hashBytes, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
	if err != nil {
		return "", "", "", err
	}
	return username, email, string(hashBytes), nil
}

var ErrInvalidCredentials = errors.New("invalid credentials")

type LoginRequest struct {
	Username string
	Password string
}

type LoginResult struct {
	Token       string   `json:"token"`
	UserID      int64    `json:"user_id"`
	Username    string   `json:"username"`
	Roles       []string `json:"roles"`
	Permissions []string `json:"permissions"`
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

	permissions, err := s.userRepo.GetPermissionCodesByUserID(ctx, userID)
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
		Token:       tokenString,
		UserID:      userID,
		Username:    username,
		Roles:       roles,
		Permissions: permissions,
	}, nil
}
