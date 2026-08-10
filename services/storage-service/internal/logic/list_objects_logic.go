package logic

import (
	"context"

	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type ListObjectsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewListObjectsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ListObjectsLogic {
	return &ListObjectsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ListObjectsLogic) ListObjects(req *types.ListObjectsReq) (*types.ListObjectsResp, error) {
	page, err := l.svcCtx.ObjectStore.List(l.ctx, req.Bucket, req.Prefix, req.Cursor, req.Limit)
	if err != nil {
		return nil, err
	}
	return &types.ListObjectsResp{Objects: page.Objects, NextCursor: page.NextCursor}, nil
}
