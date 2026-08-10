// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type ReconcileArtifactGCIntentLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewReconcileArtifactGCIntentLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ReconcileArtifactGCIntentLogic {
	return &ReconcileArtifactGCIntentLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ReconcileArtifactGCIntentLogic) ReconcileArtifactGCIntent(req *types.ReconcileArtifactGCIntentReq) (resp *types.ArtifactGCActionResp, err error) {
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
	digest, err := validateArtifactGCDigest(req.ArtifactSha256)
	if err != nil {
		return nil, err
	}
	if req.ArtifactSizeBytes < 0 {
		return nil, errors.New("artifact_size_bytes must not be negative")
	}
	if l.svcCtx == nil || l.svcCtx.ArtifactGC == nil {
		return nil, svc.ErrArtifactGCUnavailable
	}
	result, err := l.svcCtx.ArtifactGC.RequestReconcile(
		l.ctx,
		strings.TrimSpace(req.ArtifactUri),
		digest,
		req.ArtifactSizeBytes,
		artifactGCActor(user),
		strings.TrimSpace(req.Reason),
		strings.TrimSpace(req.IdempotencyKey),
	)
	if err != nil {
		return nil, err
	}
	return artifactGCActionResponse(strings.TrimSpace(req.ArtifactUri), result), nil
}
