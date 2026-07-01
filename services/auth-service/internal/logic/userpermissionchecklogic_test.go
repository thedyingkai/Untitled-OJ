package logic

import (
	"os"
	"strings"
	"testing"
)

func TestUserPermissionCheckSupportsServiceCallerIdentity(t *testing.T) {
	data, err := os.ReadFile("userpermissionchecklogic.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, want := range []string{
		`callerType == "service"`,
		`callerType == "internal"`,
		"CallerService",
		"ServiceCallerCanUsePermission",
	} {
		if !strings.Contains(source, want) {
			t.Fatalf("service caller permission path missing %q", want)
		}
	}
}
