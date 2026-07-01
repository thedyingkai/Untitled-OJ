// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type EnsureBucketLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewEnsureBucketLogic(ctx context.Context, svcCtx *svc.ServiceContext) *EnsureBucketLogic {
	return &EnsureBucketLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *EnsureBucketLogic) EnsureBucket(req *types.BucketReq) (resp *types.BucketResp, err error) {
	created, err := l.svcCtx.ObjectStore.EnsureBucket(req.Bucket)
	if err != nil {
		return nil, err
	}
	return &types.BucketResp{
		Bucket:  req.Bucket,
		Created: created,
	}, nil
}
