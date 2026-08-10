package logic

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/core/logx"
)

type WorkerClaimTasksLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewWorkerClaimTasksLogic(ctx context.Context, svcCtx *svc.ServiceContext) *WorkerClaimTasksLogic {
	return &WorkerClaimTasksLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *WorkerClaimTasksLogic) WorkerClaimTasks(req *types.WorkerClaimTasksReq) (resp *types.WorkerClaimTasksResp, err error) {
	return l.WorkerClaimTasksWithWait(req, 0)
}

func (l *WorkerClaimTasksLogic) WorkerClaimTasksWithWait(req *types.WorkerClaimTasksReq, wait time.Duration) (resp *types.WorkerClaimTasksResp, err error) {
	if wait <= 0 {
		return l.workerClaimTasksOnce(req)
	}
	if wait > 25*time.Second {
		wait = 25 * time.Second
	}
	deadline := time.Now().Add(wait)
	cursor := l.currentTaskStreamCursor()
	for {
		resp, err := l.workerClaimTasksOnce(req)
		if err != nil || len(resp.Tasks) > 0 || time.Now().After(deadline) {
			return resp, err
		}
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return resp, nil
		}
		cursor = l.waitForTaskSignal(cursor, remaining)
	}
}

func (l *WorkerClaimTasksLogic) workerClaimTasksOnce(req *types.WorkerClaimTasksReq) (resp *types.WorkerClaimTasksResp, err error) {
	workerID := strings.TrimSpace(req.WorkerId)
	if workerID == "" {
		return nil, errors.New("worker_id is required")
	}
	if err := validateWorkerIdentity(l.ctx, workerID); err != nil {
		return nil, err
	}
	if req.AvailableSlots <= 0 {
		return &types.WorkerClaimTasksResp{Tasks: []types.WorkerTaskLease{}}, nil
	}

	repo := workerTaskRepo(l.svcCtx)
	if repo == nil {
		return nil, errors.New("worker repository is not configured")
	}

	if _, err := repo.RecoverStaleTasks(l.ctx); err != nil {
		return nil, err
	}

	leases, err := repo.ClaimTasks(
		l.ctx,
		workerID,
		req.SupportedLanguages,
		req.AvailableSlots,
		workerLeaseTTL(l.svcCtx),
		normalizeWorkerTaskIDs(req.TaskIds),
	)
	if err != nil {
		if errors.Is(err, repository.ErrWorkerNotFound) {
			return nil, errors.New("worker is not registered")
		}
		if errors.Is(err, repository.ErrWorkerDraining) {
			return &types.WorkerClaimTasksResp{Tasks: []types.WorkerTaskLease{}}, nil
		}
		return nil, err
	}

	items := make([]types.WorkerTaskLease, 0, len(leases))
	for i := range leases {
		item, err := taskLeaseToResp(l.ctx, l.svcCtx, &leases[i])
		if err != nil {
			return nil, l.releaseUnexposedClaims(repo, workerID, leases, fmt.Errorf("construct task lease response: %w", err))
		}
		items = append(items, item)
	}

	// Metadata and digest resolution can be slower than the initial claim TTL.
	// Refresh only after every response item is complete so no caller can ever
	// receive a partial batch or a lease that already expired during assembly.
	for i := range leases {
		refreshed, refreshErr := repo.RefreshClaimedTaskLease(
			l.ctx,
			leases[i].TaskID,
			workerID,
			leases[i].LeaseVersion,
			workerLeaseTTL(l.svcCtx),
		)
		if refreshErr != nil {
			return nil, l.releaseUnexposedClaims(repo, workerID, leases, fmt.Errorf("finalize task lease %s: %w", leases[i].TaskID, refreshErr))
		}
		leases[i] = *refreshed
		items[i].LeaseExpiresAt = refreshed.LeaseExpiresAt.UTC().Format(time.RFC3339Nano)
	}

	return &types.WorkerClaimTasksResp{Tasks: items}, nil
}

func (l *WorkerClaimTasksLogic) releaseUnexposedClaims(
	repo svc.WorkerTaskRepository,
	workerID string,
	leases []repository.TaskLeaseView,
	cause error,
) error {
	// HTTP cancellation must not strand leases which were never delivered.
	cleanupCtx, cancel := context.WithTimeout(context.WithoutCancel(l.ctx), 5*time.Second)
	defer cancel()
	released, releaseErr := repo.ReleaseClaimedTasks(
		cleanupCtx,
		workerID,
		leases,
		"claim response was not delivered",
	)
	if releaseErr != nil {
		return errors.Join(cause, fmt.Errorf("release unexposed task leases: %w", releaseErr))
	}
	if released != int64(len(leases)) {
		// A skipped row means its exact lease was already recovered, completed,
		// or replaced. The CAS deliberately leaves that newer state untouched.
		l.Errorf("released %d of %d unexposed task leases; remaining leases no longer matched worker/version", released, len(leases))
	}
	return cause
}

func (l *WorkerClaimTasksLogic) currentTaskStreamCursor() string {
	if l.svcCtx == nil || l.svcCtx.Redis == nil {
		return ""
	}
	info, err := l.svcCtx.Redis.XInfoStream(l.ctx, judgeTaskStreamName()).Result()
	if err != nil || strings.TrimSpace(info.LastGeneratedID) == "" {
		// When the stream does not exist yet, 0-0 ensures the first event cannot
		// be lost in the gap between the initial database claim and XREAD.
		return "0-0"
	}
	return info.LastGeneratedID
}

func (l *WorkerClaimTasksLogic) waitForTaskSignal(cursor string, wait time.Duration) string {
	if l.svcCtx == nil || l.svcCtx.Redis == nil || cursor == "" {
		select {
		case <-l.ctx.Done():
			return cursor
		case <-time.After(min(wait, 500*time.Millisecond)):
			return cursor
		}
	}
	// go-redis deliberately adds a socket grace period to blocking XREAD. That
	// can make a 60 ms request live for roughly ten seconds and can also delay
	// HTTP cancellation. Use bounded non-blocking stream observations and let
	// the outer long-poll loop sleep on the request context instead.
	streams, err := l.svcCtx.Redis.XRead(l.ctx, &redis.XReadArgs{
		Streams: []string{judgeTaskStreamName(), cursor},
		Count:   1,
		Block:   -1,
	}).Result()
	if errors.Is(err, redis.Nil) {
		select {
		case <-l.ctx.Done():
		case <-time.After(min(wait, 250*time.Millisecond)):
		}
		return cursor
	}
	if err != nil {
		select {
		case <-l.ctx.Done():
		case <-time.After(min(wait, 500*time.Millisecond)):
		}
		return cursor
	}
	for _, stream := range streams {
		if len(stream.Messages) > 0 {
			cursor = stream.Messages[len(stream.Messages)-1].ID
		}
	}
	return cursor
}
