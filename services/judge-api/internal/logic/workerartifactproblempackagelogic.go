// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"net/http"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type WorkerArtifactProblemPackageLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewWorkerArtifactProblemPackageLogic(ctx context.Context, svcCtx *svc.ServiceContext) *WorkerArtifactProblemPackageLogic {
	return &WorkerArtifactProblemPackageLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *WorkerArtifactProblemPackageLogic) WorkerArtifactProblemPackage(req *types.WorkerArtifactProblemPackageReq) error {
	return nil
}

func (l *WorkerArtifactProblemPackageLogic) Serve(w http.ResponseWriter, r *http.Request, req *types.WorkerArtifactProblemPackageReq) error {
	return ServeWorkerProblemPackage(l.ctx, l.svcCtx, w, r, req)
}
