package jwt

import (
	"errors"
	"fmt"
	"strconv"
	"time"

	stdjwt "github.com/golang-jwt/jwt/v5"
)

const Issuer = "ojos-auth"

type Claims struct {
	UserID   int64    `json:"user_id"`
	Username string   `json:"username"`
	Roles    []string `json:"roles"`

	stdjwt.RegisteredClaims
}

func Generate(
	secret string,
	userID int64,
	username string,
	roles []string,
	expireHours int,
) (string, error) {
	if secret == "" {
		return "", errors.New("jwt secret is empty")
	}

	if expireHours <= 0 {
		expireHours = 24
	}

	now := time.Now().UTC()

	claims := Claims{
		UserID:   userID,
		Username: username,
		Roles:    roles,
		RegisteredClaims: stdjwt.RegisteredClaims{
			Issuer:    Issuer,
			Subject:   strconv.FormatInt(userID, 10),
			IssuedAt:  stdjwt.NewNumericDate(now),
			ExpiresAt: stdjwt.NewNumericDate(now.Add(time.Duration(expireHours) * time.Hour)),
		},
	}

	token := stdjwt.NewWithClaims(stdjwt.SigningMethodHS256, claims)

	return token.SignedString([]byte(secret))
}

func Parse(secret string, tokenString string) (*Claims, error) {
	if secret == "" {
		return nil, errors.New("jwt secret is empty")
	}

	token, err := stdjwt.ParseWithClaims(
		tokenString,
		&Claims{},
		func(token *stdjwt.Token) (any, error) {
			if _, ok := token.Method.(*stdjwt.SigningMethodHMAC); !ok {
				return nil, fmt.Errorf("unexpected signing method: %v", token.Header["alg"])
			}

			return []byte(secret), nil
		},
		stdjwt.WithIssuer(Issuer),
	)
	if err != nil {
		return nil, err
	}

	claims, ok := token.Claims.(*Claims)
	if !ok || !token.Valid {
		return nil, errors.New("invalid token claims")
	}

	return claims, nil
}
