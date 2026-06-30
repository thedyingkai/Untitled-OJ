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

type HeadObjectLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewHeadObjectLogic(ctx context.Context, svcCtx *svc.ServiceContext) *HeadObjectLogic {
	return &HeadObjectLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *HeadObjectLogic) HeadObject(w http.ResponseWriter, r *http.Request, req *types.ObjectReq) error {
	return l.svcCtx.ObjectStore.Serve(w, r, req.Bucket, req.Key)
}
