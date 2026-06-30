// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type GetMetadataLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewGetMetadataLogic(ctx context.Context, svcCtx *svc.ServiceContext) *GetMetadataLogic {
	return &GetMetadataLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *GetMetadataLogic) GetMetadata(req *types.ObjectReq) (resp *types.ObjectMetadata, err error) {
	meta, err := l.svcCtx.ObjectStore.Metadata(req.Bucket, req.Key)
	if err != nil {
		return nil, err
	}
	return &meta, nil
}
