package logic

import (
	"context"
	"testing"

	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"
)

func TestListProblemsAuthorizedRejectsMissingAuthenticatedUserBeforeRepository(t *testing.T) {
	logic := NewListProblemsLogic(context.Background(), &svc.ServiceContext{})
	if _, err := logic.ListProblemsAuthorized(&types.ListProblemsReq{}); err == nil || err.Error() != "unauthorized" {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestListProblemsAuthorizedRejectsNilRequest(t *testing.T) {
	logic := NewListProblemsLogic(context.Background(), &svc.ServiceContext{})
	if _, err := logic.ListProblemsAuthorized(nil); err == nil || err.Error() != "request is required" {
		t.Fatalf("unexpected error: %v", err)
	}
}
