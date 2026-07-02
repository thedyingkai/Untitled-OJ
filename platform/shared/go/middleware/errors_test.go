package middleware

import (
	"net/http"
	"testing"
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
