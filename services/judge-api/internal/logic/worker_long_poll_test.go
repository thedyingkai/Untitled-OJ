package logic

import (
	"context"
	"testing"
	"time"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
)

func TestWorkerClaimLongPollIsBoundedWhenQueueIsEmpty(t *testing.T) {
	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer redisClient.Close()

	repo := &fakeWorkerRuntimeRepo{}
	svcCtx := &svc.ServiceContext{WorkerRepo: repo, Redis: redisClient}
	started := time.Now()
	response, err := NewWorkerClaimTasksLogic(context.Background(), svcCtx).WorkerClaimTasksWithWait(
		&types.WorkerClaimTasksReq{WorkerId: "worker-a", AvailableSlots: 1},
		60*time.Millisecond,
	)
	if err != nil {
		t.Fatal(err)
	}
	elapsed := time.Since(started)
	if len(response.Tasks) != 0 {
		t.Fatalf("empty queue returned tasks: %#v", response.Tasks)
	}
	if elapsed < 40*time.Millisecond || elapsed > time.Second {
		t.Fatalf("long poll did not honor its bound: %s", elapsed)
	}
}
