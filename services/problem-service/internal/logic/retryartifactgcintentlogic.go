// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-problem-service/internal/artifactgc"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type RetryArtifactGCIntentLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewRetryArtifactGCIntentLogic(ctx context.Context, svcCtx *svc.ServiceContext) *RetryArtifactGCIntentLogic {
	return &RetryArtifactGCIntentLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *RetryArtifactGCIntentLogic) RetryArtifactGCIntent(req *types.RetryArtifactGCIntentReq) (resp *types.ArtifactGCActionResp, err error) {
	user, err := requireSystemProblemPermission(l.ctx, l.svcCtx, "problem.manage.data")
	if err != nil {
		return nil, err
	}
	if req == nil {
		return nil, errInvalidArtifactGCRequest
	}
	if err := validateArtifactGCMutation(req.IdempotencyKey, req.ArtifactUri, req.Reason); err != nil {
		return nil, err
	}
	if req.ExpectedFailureCount < 1 {
		return nil, errors.New("expected_failure_count must be at least 1")
	}
	if l.svcCtx == nil || l.svcCtx.ArtifactGC == nil {
		return nil, svc.ErrArtifactGCUnavailable
	}
	result, err := l.svcCtx.ArtifactGC.RetryNeedsAttention(
		l.ctx,
		strings.TrimSpace(req.ArtifactUri),
		req.ExpectedFailureCount,
		artifactGCActor(user),
		strings.TrimSpace(req.Reason),
		strings.TrimSpace(req.IdempotencyKey),
	)
	if err != nil {
		return nil, err
	}
	return artifactGCActionResponse(strings.TrimSpace(req.ArtifactUri), result), nil
}

func artifactGCActionResponse(uri string, result artifactgc.OperatorActionResult) *types.ArtifactGCActionResp {
	return &types.ArtifactGCActionResp{
		ActionId:         result.ActionID,
		RequestId:        artifactGCRequestID(result.ActionID),
		ArtifactUri:      uri,
		FromStatus:       result.FromStatus,
		ToStatus:         result.ToStatus,
		ReasonRecorded:   true,
		Queued:           true,
		IdempotentReplay: result.Replayed,
	}
}
