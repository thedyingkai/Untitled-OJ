// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"io"

	"ojos-storage-service/internal/store"
	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type PutObjectLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewPutObjectLogic(ctx context.Context, svcCtx *svc.ServiceContext) *PutObjectLogic {
	return &PutObjectLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *PutObjectLogic) PutObject(req *types.ObjectReq, options store.PutOptions, body io.Reader) (resp *types.ObjectMetadata, err error) {
	meta, err := l.svcCtx.ObjectStore.Put(l.ctx, req.Bucket, req.Key, options, body)
	if err != nil {
		return nil, err
	}
	return &meta, nil
}
