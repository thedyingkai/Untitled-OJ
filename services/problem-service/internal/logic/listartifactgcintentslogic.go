// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"strings"
	"time"

	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type ListArtifactGCIntentsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewListArtifactGCIntentsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ListArtifactGCIntentsLogic {
	return &ListArtifactGCIntentsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ListArtifactGCIntentsLogic) ListArtifactGCIntents(req *types.ListArtifactGCIntentsReq) (resp *types.ListArtifactGCIntentsResp, err error) {
	if _, err := requireSystemProblemPermission(l.ctx, l.svcCtx, "problem.manage.data"); err != nil {
		return nil, err
	}
	if req == nil {
		return nil, errInvalidArtifactGCRequest
	}
	status := strings.ToUpper(strings.TrimSpace(req.Status))
	if !validArtifactGCStatus(status) {
		return nil, errInvalidArtifactGCStatus
	}
	limit := req.Limit
	if limit == 0 {
		limit = 100
	}
	if limit < 1 || limit > 200 {
		return nil, errInvalidArtifactGCLimit
	}
	if l.svcCtx == nil || l.svcCtx.ArtifactGC == nil {
		return nil, svc.ErrArtifactGCUnavailable
	}
	page, err := l.svcCtx.ArtifactGC.ListIntents(l.ctx, status, strings.TrimSpace(req.Cursor), limit)
	if err != nil {
		return nil, err
	}
	items := make([]types.ArtifactGCIntentItem, 0, len(page.Items))
	for _, record := range page.Items {
		httpStatus := 0
		if record.LastFailureHTTPStatus != nil {
			httpStatus = *record.LastFailureHTTPStatus
		}
		message := strings.TrimSpace(record.LastError)
		if record.LastFailureKind == "" && message != "" {
			message = "legacy artifact GC failure; inspect service logs"
		}
		items = append(items, types.ArtifactGCIntentItem{
			ArtifactUri:       record.URI,
			ArtifactSha256:    record.SHA256,
			ArtifactSizeBytes: record.SizeBytes,
			Status:            record.Status,
			FailureCount:      record.FailureCount,
			LastFailure: types.ArtifactGCLastFailure{
				Message:        message,
				Stage:          record.LastFailureStage,
				Kind:           record.LastFailureKind,
				HttpStatus:     httpStatus,
				ProviderResult: record.LastFailureProviderResult,
				Deterministic:  record.LastFailureDeterministic,
			},
			UploadCompletedAt:          formatOptionalTime(record.UploadCompletedAt),
			NeedsAttentionAt:           formatOptionalTime(record.NeedsAttentionAt),
			ManualReconcileRequestedAt: formatOptionalTime(record.ManualReconcileRequestedAt),
			LastOperatorRetryReason:    record.LastOperatorRetryReason,
			LastOperatorRetryAt:        formatOptionalTime(record.LastOperatorRetryAt),
			UpdatedAt:                  record.UpdatedAt.UTC().Format(time.RFC3339Nano),
		})
	}
	return &types.ListArtifactGCIntentsResp{Intents: items, NextCursor: page.NextCursor}, nil
}

func formatOptionalTime(value *time.Time) string {
	if value == nil || value.IsZero() {
		return ""
	}
	return value.UTC().Format(time.RFC3339Nano)
}
