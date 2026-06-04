// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strconv"
	"time"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"

	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/core/logx"
)

type RejudgeProblemLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewRejudgeProblemLogic(ctx context.Context, svcCtx *svc.ServiceContext) *RejudgeProblemLogic {
	return &RejudgeProblemLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *RejudgeProblemLogic) RejudgeProblem(req *types.RejudgeProblemReq) (resp *types.RejudgeProblemResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if req.Id <= 0 {
		return nil, errors.New("invalid problem id")
	}

	if _, err := l.svcCtx.Repo.GetProblemMeta(l.ctx, req.Id); err != nil {
		return nil, err
	}

	if err := sharedperm.RequireUserPermission(
		l.ctx,
		l.svcCtx.DB,
		user.UserID,
		"problem.manage.data",
		sharedperm.Scope{Type: "problem", ID: req.Id},
	); err != nil {
		return nil, err
	}

	ids, err := l.svcCtx.Repo.ResetSubmissionsForProblem(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}

	enqueued := 0
	for _, submissionID := range ids {
		if err := l.publishSubmissionCreated(submissionID); err != nil {
			return nil, err
		}
		enqueued++
	}

	return &types.RejudgeProblemResp{
		ProblemId: req.Id,
		Enqueued:  enqueued,
	}, nil
}

func (l *RejudgeProblemLogic) publishSubmissionCreated(submissionID int64) error {
	return l.svcCtx.Redis.XAdd(
		l.ctx,
		&redis.XAddArgs{
			Stream: judgeSubmissionStream,
			Values: map[string]any{
				"type":          "submission.created",
				"producer":      "judge-api-service",
				"submission_id": strconv.FormatInt(submissionID, 10),
				"created_at":    time.Now().UTC().Format(time.RFC3339Nano),
			},
		},
	).Err()
}
