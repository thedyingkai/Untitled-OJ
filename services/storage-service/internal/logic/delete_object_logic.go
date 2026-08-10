// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type DeleteObjectLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewDeleteObjectLogic(ctx context.Context, svcCtx *svc.ServiceContext) *DeleteObjectLogic {
	return &DeleteObjectLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *DeleteObjectLogic) DeleteObject(req *types.ObjectReq) (resp *types.DeleteObjectResp, err error) {
	if err := l.svcCtx.ObjectStore.Delete(req.Bucket, req.Key); err != nil {
		return nil, err
	}
	return &types.DeleteObjectResp{Deleted: true}, nil
}

func (l *DeleteObjectLogic) DeleteObjectIfMatches(req *types.ObjectReq, expectedSHA256 string, expectedSize int64) (resp *types.DeleteObjectResp, err error) {
	if err := l.svcCtx.ObjectStore.DeleteIfMatches(l.ctx, req.Bucket, req.Key, expectedSHA256, expectedSize); err != nil {
		return nil, err
	}
	return &types.DeleteObjectResp{Deleted: true}, nil
}
