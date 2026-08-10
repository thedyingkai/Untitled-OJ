package artifactgc

import (
	"errors"
	"fmt"
	"strings"
)

// ProviderHTTPError preserves the bound provider's status classification.
// Callers must only construct a 404 error after response provenance proves it
// came from the selected provider; an unproven HEAD 404 is not object absence.
type ProviderHTTPError struct {
	Operation  string
	StatusCode int
	Detail     string
}

func (e *ProviderHTTPError) Error() string {
	operation := strings.TrimSpace(e.Operation)
	if operation == "" {
		operation = "bound provider request"
	}
	detail := strings.TrimSpace(e.Detail)
	if detail == "" {
		return fmt.Sprintf("%s returned HTTP %d", operation, e.StatusCode)
	}
	return fmt.Sprintf("%s returned HTTP %d: %s", operation, e.StatusCode, detail)
}

// Deterministic reports failures for which retrying the same contract and
// credential cannot safely make progress without operator or Topology action.
func (e *ProviderHTTPError) Deterministic() bool {
	return isDeterministicProviderStatus(e.StatusCode)
}

func NewProviderHTTPError(operation string, statusCode int, detail string) error {
	return &ProviderHTTPError{Operation: operation, StatusCode: statusCode, Detail: detail}
}

// ProviderContractError represents an authenticated provider response that
// violates the bound API contract. Retrying the same route cannot repair a
// missing or invalid authoritative result marker.
type ProviderContractError struct {
	Operation string
	Result    string
}

func (e *ProviderContractError) Error() string {
	operation := strings.TrimSpace(e.Operation)
	if operation == "" {
		operation = "bound provider request"
	}
	result := strings.TrimSpace(e.Result)
	if result == "" {
		result = "missing"
	}
	return fmt.Sprintf("%s violated the provider result contract (%s)", operation, result)
}

func (e *ProviderContractError) Deterministic() bool { return true }

type deterministicFailure interface {
	Deterministic() bool
}

func isDeterministicProviderFailure(err error) bool {
	if err == nil {
		return false
	}
	var classified deterministicFailure
	if errors.As(err, &classified) {
		return classified.Deterministic()
	}

	// Keep compatibility with providers compiled before ProviderHTTPError. New
	// bound-store code should return the typed error above.
	message := strings.ToLower(strings.TrimSpace(err.Error()))
	if strings.Contains(message, "outside the configured bucket or key contract") || strings.Contains(message, "precondition failed") {
		return true
	}
	for _, status := range []string{" 400 ", " 401 ", " 403 ", " 404 ", " 405 ", " 409 ", " 410 ", " 412 ", " 422 "} {
		if strings.Contains(message, "returned"+status) {
			return true
		}
	}
	return false
}

func isDeterministicProviderStatus(statusCode int) bool {
	switch statusCode {
	case 400, 401, 403, 404, 405, 409, 410, 412, 422:
		return true
	default:
		return false
	}
}
