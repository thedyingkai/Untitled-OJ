package logic

import (
	"context"
	"errors"
	"net"
	"testing"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/redis/go-redis/v9"
)

func TestCreateSubmissionSucceedsWhenRedisWakeupIsUnavailable(t *testing.T) {
	beforeFailures := testutil.ToFloat64(judgeTaskWakeupFailures.WithLabelValues("submission.created", "judge-api-service"))
	redisClient := redis.NewClient(&redis.Options{Addr: "127.0.0.1:6379"})
	redisClient.AddHook(redisUnavailableHook{})
	defer redisClient.Close()
	repo := &redisFailureSubmissionRepo{
		problem: &repository.ProblemMeta{
			ID:                       101,
			Status:                   "ready",
			Visibility:               "public",
			AggregateVersion:         1,
			PackageRevision:          1,
			PackageArtifactURI:       "storage://problems/101.zip",
			PackageArtifactSHA256:    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
			PackageArtifactSizeBytes: 128,
		},
	}
	ctx := authctx.NewContext(context.Background(), &authctx.UserContext{UserID: 7})
	svcCtx := &svc.ServiceContext{
		Config: config.Config{
			Storage: config.StorageConfig{SubmissionsRoot: t.TempDir()},
			Languages: config.LanguagesConfig{Items: []config.LanguageConfig{{
				Id: "cpp17", Enabled: true, SourceFile: "main.cpp",
			}}},
		},
		SubmissionRepo: repo,
		Permission:     allowAllJudgePermissions{},
		Redis:          redisClient,
	}

	resp, err := NewCreateSubmissionLogic(ctx, svcCtx).CreateSubmission(&types.CreateSubmissionReq{
		ProblemId: 101,
		Language:  "cpp17",
		Code:      "int main() { return 0; }\n",
	})
	if err != nil {
		t.Fatalf("Redis wakeup failure rejected a durable submission: %v", err)
	}
	if resp.SubmissionId != 42 || resp.Status != "PENDING" {
		t.Fatalf("unexpected create response: %#v", resp)
	}
	if !repo.taskEnsured {
		t.Fatal("PostgreSQL task was not ensured before the best-effort wakeup")
	}
	if len(repo.systemErrors) != 0 {
		t.Fatalf("Redis wakeup failure marked submission SYSTEM_ERROR: %#v", repo.systemErrors)
	}
	afterFailures := testutil.ToFloat64(judgeTaskWakeupFailures.WithLabelValues("submission.created", "judge-api-service"))
	if afterFailures != beforeFailures+1 {
		t.Fatalf("Redis wakeup failure metric delta=%v, want 1", afterFailures-beforeFailures)
	}
}

func TestRejudgeSucceedsWhenRedisWakeupIsUnavailable(t *testing.T) {
	redisClient := redis.NewClient(&redis.Options{Addr: "127.0.0.1:6379"})
	redisClient.AddHook(redisUnavailableHook{})
	defer redisClient.Close()
	repo := &redisFailureRejudgeRepo{ids: []int64{42, 43}}
	ctx := authctx.NewContext(context.Background(), &authctx.UserContext{UserID: 9})
	svcCtx := &svc.ServiceContext{
		RejudgeRepo: repo,
		Permission:  allowAllJudgePermissions{},
		Redis:       redisClient,
	}

	resp, err := NewRejudgeProblemLogic(ctx, svcCtx).RejudgeProblem(&types.RejudgeProblemReq{Id: 101})
	if err != nil {
		t.Fatalf("Redis wakeup failure rejected a durable rejudge: %v", err)
	}
	if resp.ProblemId != 101 || resp.Enqueued != 2 {
		t.Fatalf("unexpected rejudge response: %#v", resp)
	}
	if len(repo.ensured) != 2 || repo.ensured[0] != 42 || repo.ensured[1] != 43 {
		t.Fatalf("rejudge tasks were not durably ensured: %#v", repo.ensured)
	}
}

type allowAllJudgePermissions struct{}

type redisUnavailableHook struct{}

func (redisUnavailableHook) DialHook(redis.DialHook) redis.DialHook {
	return func(context.Context, string, string) (net.Conn, error) {
		return nil, errors.New("redis unavailable")
	}
}

func (redisUnavailableHook) ProcessHook(redis.ProcessHook) redis.ProcessHook {
	return func(context.Context, redis.Cmder) error {
		return errors.New("redis unavailable")
	}
}

func (redisUnavailableHook) ProcessPipelineHook(redis.ProcessPipelineHook) redis.ProcessPipelineHook {
	return func(context.Context, []redis.Cmder) error {
		return errors.New("redis unavailable")
	}
}

func (allowAllJudgePermissions) RequireUserPermission(context.Context, int64, string, sharedperm.Scope) error {
	return nil
}

func (allowAllJudgePermissions) HasUserPermission(context.Context, int64, string, sharedperm.Scope) (bool, error) {
	return true, nil
}

type redisFailureSubmissionRepo struct {
	problem      *repository.ProblemMeta
	taskEnsured  bool
	systemErrors []string
}

func (r *redisFailureSubmissionRepo) GetProblemMeta(context.Context, int64) (*repository.ProblemMeta, error) {
	copy := *r.problem
	return &copy, nil
}

func (*redisFailureSubmissionRepo) CreateSubmission(context.Context, int64, int64, string) (int64, error) {
	return 42, nil
}

func (*redisFailureSubmissionRepo) UpdateSubmissionSource(context.Context, int64, string, string, string) error {
	return nil
}

func (r *redisFailureSubmissionRepo) EnsureTaskForSubmission(context.Context, int64) error {
	r.taskEnsured = true
	return nil
}

func (r *redisFailureSubmissionRepo) MarkSubmissionSystemError(_ context.Context, _ int64, message string) error {
	r.systemErrors = append(r.systemErrors, message)
	return nil
}

type redisFailureRejudgeRepo struct {
	ids     []int64
	ensured []int64
}

func (*redisFailureRejudgeRepo) GetProblemMeta(_ context.Context, id int64) (*repository.ProblemMeta, error) {
	if id <= 0 {
		return nil, errors.New("problem not found")
	}
	return &repository.ProblemMeta{ID: id}, nil
}

func (r *redisFailureRejudgeRepo) ResetSubmissionsForProblem(context.Context, int64) ([]int64, error) {
	return append([]int64(nil), r.ids...), nil
}

func (r *redisFailureRejudgeRepo) EnsureTaskForSubmission(_ context.Context, submissionID int64) error {
	r.ensured = append(r.ensured, submissionID)
	return nil
}
