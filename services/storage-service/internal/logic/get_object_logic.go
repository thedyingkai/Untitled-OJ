// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"net/http"

	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type GetObjectLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewGetObjectLogic(ctx context.Context, svcCtx *svc.ServiceContext) *GetObjectLogic {
	return &GetObjectLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *GetObjectLogic) GetObject(w http.ResponseWriter, r *http.Request, req *types.ObjectReq) error {
	return l.svcCtx.ObjectStore.Serve(w, r, req.Bucket, req.Key)
}
