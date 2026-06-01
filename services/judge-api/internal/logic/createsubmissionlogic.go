// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"ojos-shared/security/authctx"
	"strconv"
	"time"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
	sharedperm "ojos-shared/security/permission"

	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/core/logx"
)

type CreateSubmissionLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewCreateSubmissionLogic(ctx context.Context, svcCtx *svc.ServiceContext) *CreateSubmissionLogic {
	return &CreateSubmissionLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *CreateSubmissionLogic) CreateSubmission(req *types.CreateSubmissionReq) (resp *types.CreateSubmissionResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if err := sharedperm.RequireUserPermission(
		l.ctx,
		l.svcCtx.DB,
		user.UserID,
		"judge.submit",
		sharedperm.SystemScope(),
	); err != nil {
		return nil, err
	}

	if req.ProblemId <= 0 {
		return nil, errors.New("invalid problem id")
	}

	if req.Language == "" {
		req.Language = "cpp17"
	}

	if req.Code == "" {
		return nil, errors.New("empty code")
	}

	submissionID, err := l.svcCtx.Repo.CreateSubmission(
		l.ctx,
		req.ProblemId,
		user.UserID,
		req.Language,
		req.Code,
	)
	if err != nil {
		return nil, err
	}

	if err := l.publishSubmissionCreated(submissionID); err != nil {
		return nil, err
	}

	return &types.CreateSubmissionResp{
		SubmissionId: submissionID,
		Status:       "PENDING",
	}, nil
}

const judgeSubmissionStream = "ojos:judge:submissions"

func (l *CreateSubmissionLogic) publishSubmissionCreated(submissionID int64) error {
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
