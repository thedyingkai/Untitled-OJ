package problemclient

import (
	"context"
	"errors"
	"testing"

	"ojos-shared/servicecontext"
)

func TestProbeWithoutProviderReturnsTypedBindingUnavailable(t *testing.T) {
	err := (*Client)(nil).Probe(context.Background())
	var unavailable *servicecontext.BindingUnavailable
	if !errors.As(err, &unavailable) || unavailable.Name != BindingName {
		t.Fatalf("error = %v, want BindingUnavailable for %q", err, BindingName)
	}
}
