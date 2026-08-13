package logic

import (
	"context"
	"errors"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminRequeueSubmissionLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminRequeueSubmissionLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminRequeueSubmissionLogic {
	return &AdminRequeueSubmissionLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminRequeueSubmissionLogic) AdminRequeueSubmission(req *types.AdminSubmissionActionReq) (resp *types.AdminActionResp, err error) {
	if err := requireJudgeAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	if req.Id <= 0 {
		return nil, errors.New("invalid submission id")
	}
	if err := l.svcCtx.Repo.RequeueSubmission(l.ctx, req.Id); err != nil {
		if errors.Is(err, repository.ErrSubmissionNotFound) {
			return nil, errors.New("submission not found")
		}
		return nil, err
	}
	notifyJudgeTaskAvailable(l.ctx, l.svcCtx, "submission.requeued", "judge-api-admin", req.Id)
	return &types.AdminActionResp{Ok: true}, nil
}
