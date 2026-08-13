package logic

import (
	"context"
	"fmt"
	"reflect"
	"testing"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
)

func TestSubmissionViewAlwaysEnforcesOpenAPIMainPermission(t *testing.T) {
	checker := &recordingUserChecker{allowed: map[string]bool{
		"judge.submission.view.own:system:0": true,
		"judge.submission.view.all:system:0": true,
	}}
	ctx := authctx.NewContext(context.Background(), &authctx.UserContext{UserID: 7})
	service := &svc.ServiceContext{Permission: checker}

	if err := requireSubmissionViewPermission(ctx, service, &repository.SubmissionView{UserID: 7}); err != nil {
		t.Fatal(err)
	}
	if got, want := checker.calls, []string{"judge.submission.view.own:system:0"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("owner permission calls = %v, want %v", got, want)
	}

	checker.calls = nil
	if err := requireSubmissionViewPermission(ctx, service, &repository.SubmissionView{UserID: 9}); err != nil {
		t.Fatal(err)
	}
	if got, want := checker.calls, []string{
		"judge.submission.view.own:system:0",
		"judge.submission.view.all:system:0",
	}; !reflect.DeepEqual(got, want) {
		t.Fatalf("cross-user permission calls = %v, want %v", got, want)
	}
}

func TestSubmissionDebugEnforcesOpenAPIMainPermission(t *testing.T) {
	checker := &recordingUserChecker{allowed: map[string]bool{
		"judge.submission.view.all:system:0": true,
	}}
	ctx := authctx.NewContext(context.Background(), &authctx.UserContext{UserID: 7})
	if err := requireSubmissionDebugPermission(ctx, &svc.ServiceContext{Permission: checker}, &repository.SubmissionView{UserID: 7}); err != nil {
		t.Fatal(err)
	}
	if got, want := checker.calls, []string{"judge.submission.view.all:system:0"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("debug permission calls = %v, want %v", got, want)
	}
}

type recordingUserChecker struct {
	allowed map[string]bool
	calls   []string
}

func (checker *recordingUserChecker) RequireUserPermission(_ context.Context, _ int64, permission string, scope sharedperm.Scope) error {
	key := permissionScopeKey(permission, scope)
	checker.calls = append(checker.calls, key)
	if checker.allowed[key] {
		return nil
	}
	return sharedperm.ErrForbidden
}

func (checker *recordingUserChecker) HasUserPermission(_ context.Context, _ int64, permission string, scope sharedperm.Scope) (bool, error) {
	key := permissionScopeKey(permission, scope)
	checker.calls = append(checker.calls, key)
	return checker.allowed[key], nil
}

func permissionScopeKey(permission string, scope sharedperm.Scope) string {
	return fmt.Sprintf("%s:%s:%d", permission, scope.Type, scope.ID)
}
