// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type ListBucketsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewListBucketsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ListBucketsLogic {
	return &ListBucketsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ListBucketsLogic) ListBuckets() (resp *types.BucketsResp, err error) {
	return &types.BucketsResp{Buckets: l.svcCtx.ObjectStore.BucketNames()}, nil
}
