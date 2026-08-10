package logic

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	"ojos-problem-service/internal/artifactgc"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
)

type operatorPermissionChecker struct {
	allowed        bool
	permissionCode string
	scope          sharedperm.Scope
}

func (p *operatorPermissionChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) error {
	allowed, err := p.HasUserPermission(ctx, userID, permissionCode, scope)
	if err != nil {
		return err
	}
	if !allowed {
		return sharedperm.ErrForbidden
	}
	return nil
}

func (p *operatorPermissionChecker) HasUserPermission(_ context.Context, _ int64, permissionCode string, scope sharedperm.Scope) (bool, error) {
	p.permissionCode = permissionCode
	p.scope = scope
	return p.allowed, nil
}

type operatorLedgerFake struct {
	page             artifactgc.IntentPage
	result           artifactgc.OperatorActionResult
	reconcileCalls   int
	retryCalls       int
	actor            string
	idempotencyKey   string
	expectedFailures int
}

func (l *operatorLedgerFake) ListIntents(context.Context, string, string, int) (artifactgc.IntentPage, error) {
	return l.page, nil
}

func (l *operatorLedgerFake) RecoveryDue(context.Context) (bool, error) {
	return false, nil
}

func (l *operatorLedgerFake) RequestReconcile(_ context.Context, _ string, _ string, _ int64, actor, _ string, idempotencyKey string) (artifactgc.OperatorActionResult, error) {
	l.reconcileCalls++
	l.actor = actor
	l.idempotencyKey = idempotencyKey
	return l.result, nil
}

func (l *operatorLedgerFake) RetryNeedsAttention(_ context.Context, _ string, expectedFailureCount int, actor, _ string, idempotencyKey string) (artifactgc.OperatorActionResult, error) {
	l.retryCalls++
	l.expectedFailures = expectedFailureCount
	l.actor = actor
	l.idempotencyKey = idempotencyKey
	return l.result, nil
}

func operatorLogicContext(userID int64) context.Context {
	return authctx.NewContext(context.Background(), &authctx.UserContext{UserID: userID})
}

func operatorServiceContext(checker *operatorPermissionChecker, ledger *operatorLedgerFake) *svc.ServiceContext {
	return &svc.ServiceContext{
		Permission: checker,
		ArtifactGC: svc.NewArtifactGCController(ledger, artifactgc.Collector{}),
	}
}

func TestListArtifactGCIntentsRequiresSystemManageDataAndOmitsCollectorCredentials(t *testing.T) {
	now := time.Now().UTC().Truncate(time.Microsecond)
	status := 404
	ledger := &operatorLedgerFake{page: artifactgc.IntentPage{Items: []artifactgc.IntentRecord{{
		URI: "storage://problems/package-sha256-" + strings.Repeat("a", 64) + ".zip", SHA256: strings.Repeat("a", 64),
		SizeBytes: 17, Status: "NEEDS_ATTENTION", FailureCount: 3,
		LastError: "inspect failed with provider HTTP 404", LastFailureStage: "inspect",
		LastFailureKind: artifactgc.FailureKindProviderHTTP, LastFailureHTTPStatus: &status,
		LastFailureProviderResult: "HTTP_404", LastFailureDeterministic: true,
		UploadCompletedAt: &now, NeedsAttentionAt: &now, UpdatedAt: now,
	}}, NextCursor: "next"}}
	checker := &operatorPermissionChecker{allowed: true}
	logic := NewListArtifactGCIntentsLogic(operatorLogicContext(42), operatorServiceContext(checker, ledger))
	resp, err := logic.ListArtifactGCIntents(&types.ListArtifactGCIntentsReq{Status: "needs_attention", Limit: 100})
	if err != nil {
		t.Fatal(err)
	}
	if checker.permissionCode != "problem.manage.data" || checker.scope.Type != sharedperm.SystemScope().Type {
		t.Fatalf("wrong permission scope: permission=%s scope=%#v", checker.permissionCode, checker.scope)
	}
	if len(resp.Intents) != 1 || resp.Intents[0].UploadCompletedAt == "" || resp.Intents[0].LastFailure.Kind != artifactgc.FailureKindProviderHTTP {
		t.Fatalf("operator projection mismatch: %#v", resp)
	}
	wire, err := json.Marshal(resp)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(strings.ToLower(string(wire)), "claim_token") || strings.Contains(strings.ToLower(string(wire)), "claim_until") {
		t.Fatalf("collector credential leaked: %s", wire)
	}
}

func TestArtifactGCOperatorPermissionDenialStopsBeforeLedger(t *testing.T) {
	ledger := &operatorLedgerFake{}
	checker := &operatorPermissionChecker{allowed: false}
	logic := NewRetryArtifactGCIntentLogic(operatorLogicContext(42), operatorServiceContext(checker, ledger))
	_, err := logic.RetryArtifactGCIntent(&types.RetryArtifactGCIntentReq{
		IdempotencyKey: "request-1", ArtifactUri: "storage://problems/x", ExpectedFailureCount: 3, Reason: "verified",
	})
	if err == nil || !strings.Contains(err.Error(), "forbidden") || ledger.retryCalls != 0 {
		t.Fatalf("permission denial reached ledger: err=%v calls=%d", err, ledger.retryCalls)
	}
}

func TestArtifactGCMutationsDeriveActorAndReturnStableTransition(t *testing.T) {
	ledger := &operatorLedgerFake{result: artifactgc.OperatorActionResult{
		ActionID: 91, FromStatus: "NEEDS_ATTENTION", ToStatus: "PENDING",
	}}
	checker := &operatorPermissionChecker{allowed: true}
	logic := NewRetryArtifactGCIntentLogic(operatorLogicContext(42), operatorServiceContext(checker, ledger))
	resp, err := logic.RetryArtifactGCIntent(&types.RetryArtifactGCIntentReq{
		IdempotencyKey: "request-1", ArtifactUri: "storage://problems/x", ExpectedFailureCount: 3, Reason: "verified",
	})
	if err != nil {
		t.Fatal(err)
	}
	if ledger.actor != "user:42" || ledger.expectedFailures != 3 || ledger.idempotencyKey != "request-1" {
		t.Fatalf("untrusted operator context reached ledger: %#v", ledger)
	}
	if resp.ActionId != 91 || resp.RequestId != "artifact-gc-action-91" || resp.FromStatus != "NEEDS_ATTENTION" ||
		resp.ToStatus != "PENDING" || !resp.ReasonRecorded || !resp.Queued {
		t.Fatalf("unstable action response: %#v", resp)
	}
}

func TestArtifactGCMutationRequiresIdempotencyKey(t *testing.T) {
	ledger := &operatorLedgerFake{}
	logic := NewReconcileArtifactGCIntentLogic(operatorLogicContext(42), operatorServiceContext(&operatorPermissionChecker{allowed: true}, ledger))
	_, err := logic.ReconcileArtifactGCIntent(&types.ReconcileArtifactGCIntentReq{
		ArtifactUri: "storage://problems/x", ArtifactSha256: strings.Repeat("a", 64), ArtifactSizeBytes: 1, Reason: "verified",
	})
	if err == nil || !errors.Is(err, artifactgc.ErrOperatorIdempotencyMissing) && !strings.Contains(err.Error(), "Idempotency-Key") {
		t.Fatalf("missing Idempotency-Key was accepted: %v", err)
	}
	if ledger.reconcileCalls != 0 {
		t.Fatal("invalid request reached durable ledger")
	}
}

func TestArtifactGCOptionalTimesAreOmittedInsteadOfInvalidEmptyDates(t *testing.T) {
	wire, err := json.Marshal(types.ArtifactGCIntentItem{
		ArtifactUri: "storage://problems/x", ArtifactSha256: strings.Repeat("a", 64),
		Status: "PENDING", UpdatedAt: time.Now().UTC().Format(time.RFC3339Nano),
	})
	if err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{"upload_completed_at", "needs_attention_at", "manual_reconcile_requested_at", "last_operator_retry_at"} {
		if strings.Contains(string(wire), `"`+field+`"`) {
			t.Fatalf("optional date-time %s serialized an invalid empty string: %s", field, wire)
		}
	}
}
