package eventing

import "testing"

func TestGenericTransportUsesOnlyPlatformDefaults(t *testing.T) {
	consumer := &Consumer{}
	if consumer.stream() != DefaultEventStream {
		t.Fatalf("unexpected default event stream %q", consumer.stream())
	}
	if consumer.group() != "" {
		t.Fatalf("consumer group must be deployment supplied, got %q", consumer.group())
	}
	if DefaultMaxAttempts != 10 {
		t.Fatalf("default retry budget drifted: %d", DefaultMaxAttempts)
	}
}
