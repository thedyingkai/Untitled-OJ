package logic

import (
	"encoding/hex"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"unicode/utf8"

	"ojos-shared/security/authctx"
)

var (
	errInvalidArtifactGCRequest = errors.New("invalid artifact GC request")
	errInvalidArtifactGCStatus  = errors.New("artifact GC status must be PENDING, DELETING, or NEEDS_ATTENTION")
	errInvalidArtifactGCLimit   = errors.New("artifact GC limit must be between 1 and 200")
)

func validArtifactGCStatus(status string) bool {
	switch status {
	case "PENDING", "DELETING", "NEEDS_ATTENTION":
		return true
	default:
		return false
	}
}

func validateArtifactGCMutation(idempotencyKey, uri, reason string) error {
	idempotencyKey = strings.TrimSpace(idempotencyKey)
	if idempotencyKey == "" {
		return errors.New("Idempotency-Key is required")
	}
	if len(idempotencyKey) > 255 {
		return errors.New("Idempotency-Key must not exceed 255 bytes")
	}
	for _, character := range idempotencyKey {
		if character < 0x21 || character > 0x7e {
			return errors.New("Idempotency-Key must contain visible ASCII characters only")
		}
	}
	if !strings.HasPrefix(strings.TrimSpace(uri), "storage://") {
		return errors.New("artifact_uri must be a storage URI")
	}
	reason = strings.TrimSpace(reason)
	if reason == "" || utf8.RuneCountInString(reason) > 2000 {
		return errors.New("reason must contain between 1 and 2000 characters")
	}
	return nil
}

func validateArtifactGCDigest(value string) (string, error) {
	value = strings.TrimPrefix(strings.ToLower(strings.TrimSpace(value)), "sha256:")
	decoded, err := hex.DecodeString(value)
	if err != nil || len(decoded) != 32 {
		return "", errors.New("artifact_sha256 must be 64 lowercase hexadecimal characters")
	}
	return value, nil
}

func artifactGCActor(user *authctx.UserContext) string {
	return "user:" + strconv.FormatInt(user.UserID, 10)
}

func artifactGCRequestID(actionID int64) string {
	return fmt.Sprintf("artifact-gc-action-%d", actionID)
}
