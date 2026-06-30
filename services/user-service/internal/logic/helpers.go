package logic

import (
	"ojos-user-service/internal/types"
)

func profilePtr(profile types.ProfileResp, err error) (*types.ProfileResp, error) {
	if err != nil {
		return nil, err
	}
	return &profile, nil
}
