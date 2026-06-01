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

type RegisterLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewRegisterLogic(ctx context.Context, svcCtx *svc.ServiceContext) *RegisterLogic {
	return &RegisterLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *RegisterLogic) Register(req *types.RegisterReq) (resp *types.RegisterResp, err error) {
	result, err := l.svcCtx.AuthService.Register(
		l.ctx,
		authsvc.RegisterRequest{
			Username: req.Username,
			Email:    req.Email,
			Password: req.Password,
		},
	)
	if err != nil {
		return &types.RegisterResp{
			Code: 40003,
			Msg:  err.Error(),
		}, nil
	}

	return &types.RegisterResp{
		Code: 0,
		Msg:  "success",
		Data: types.RegisterData{
			UserId:   result.UserID,
			Username: result.Username,
		},
	}, nil
}
