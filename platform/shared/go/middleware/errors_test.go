package middleware

import (
	"fmt"
	"net/http"
	"testing"

	"github.com/jackc/pgx/v5/pgconn"
)

type testCodedError struct{}

func (testCodedError) Error() string         { return "coded" }
func (testCodedError) HTTPStatus() int       { return http.StatusTeapot }
func (testCodedError) ErrorCode() int        { return 41810 }
func (testCodedError) PublicMessage() string { return "short and stable" }

func TestClassifyHTTPErrorUsesCodedError(t *testing.T) {
	status, code, msg := classifyHTTPError(testCodedError{})
	if status != http.StatusTeapot || code != 41810 || msg != "short and stable" {
		t.Fatalf("coded error was not preserved: status=%d code=%d msg=%q", status, code, msg)
	}
}

func TestClassifyHTTPErrorTreatsPostgresFailureAsInternal(t *testing.T) {
	databaseError := &pgconn.PgError{
		Code:    "P0001",
		Message: "injected problem snapshot rollback for artifact GC proof",
	}
	status, code, msg := classifyHTTPError(
		fmt.Errorf("publish problem snapshot: %w", databaseError),
	)
	if status != http.StatusInternalServerError || code != 50000 || msg != "internal server error" {
		t.Fatalf(
			"PostgreSQL failure was exposed as a client error: status=%d code=%d msg=%q",
			status,
			code,
			msg,
		)
	}
}
