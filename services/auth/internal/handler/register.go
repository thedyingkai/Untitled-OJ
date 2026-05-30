package handler

import (
	"encoding/json"
	"errors"
	"net/http"

	"ojos-auth/internal/service"

	"ojos-shared/response"
)

type registerRequest struct {
	Username string `json:"username"`
	Email    string `json:"email"`
	Password string `json:"password"`
}

func Register(authService *service.AuthService) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		defer r.Body.Close()

		var req registerRequest

		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			response.Error(w, 40001, "invalid json body")
			return
		}

		result, err := authService.Register(
			r.Context(),
			service.RegisterRequest{
				Username: req.Username,
				Email:    req.Email,
				Password: req.Password,
			},
		)

		if err != nil {
			switch {
			case errors.Is(err, service.ErrInvalidInput):
				response.Error(w, 40002, err.Error())
			case errors.Is(err, service.ErrUserExists):
				response.Error(w, 40003, "user already exists")
			default:
				response.Error(w, 50001, "internal server error")
			}

			return
		}

		response.Success(w, result)
	}
}
