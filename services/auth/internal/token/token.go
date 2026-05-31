package token

import sharedjwt "ojos-shared/security/jwt"

type Claims = sharedjwt.Claims

func Generate(
	secret string,
	expireHours int,
	userID int64,
	username string,
	roles []string,
) (string, error) {
	return sharedjwt.Generate(
		secret,
		userID,
		username,
		roles,
		expireHours,
	)
}

func Parse(secret string, tokenString string) (*Claims, error) {
	return sharedjwt.Parse(secret, tokenString)
}
