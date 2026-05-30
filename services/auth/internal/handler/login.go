package handler

import (
	"encoding/json"
	"errors"
	"net/http"

	"ojos-auth/internal/service"

	"ojos-shared/response"
)

type loginRequest struct {
	Username string `json:"username"`
	Password string `json:"password"`
}

func Login(authService *service.AuthService) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		defer r.Body.Close()

		var req loginRequest

		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			response.Error(w, 40011, "invalid json body")
			return
		}

		result, err := authService.Login(
			r.Context(),
			service.LoginRequest{
				Username: req.Username,
				Password: req.Password,
			},
		)

		if err != nil {
			switch {
			case errors.Is(err, service.ErrInvalidCredentials):
				response.Error(w, 40012, "invalid username or password")
			default:
				response.Error(w, 50011, "internal server error")
			}

			return
		}

		response.Success(w, result)
	}
}
