// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	authsvc "ojos-auth/internal/service"

	"ojos-auth/internal/svc"
	"ojos-auth/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type LoginLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewLoginLogic(ctx context.Context, svcCtx *svc.ServiceContext) *LoginLogic {
	return &LoginLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *LoginLogic) Login(req *types.LoginReq) (resp *types.LoginResp, err error) {
	result, err := l.svcCtx.AuthService.Login(
		l.ctx,
		authsvc.LoginRequest{
			Username: req.Username,
			Password: req.Password,
		},
	)
	if err != nil {
		return &types.LoginResp{
			Code: 40012,
			Msg:  "invalid username or password",
		}, nil
	}

	return &types.LoginResp{
		Code: 0,
		Msg:  "success",
		Data: types.LoginData{
			Token:    result.Token,
			UserId:   result.UserID,
			Username: result.Username,
			Roles:    result.Roles,
		},
	}, nil
}
