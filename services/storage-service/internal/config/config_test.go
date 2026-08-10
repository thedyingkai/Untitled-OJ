package config

import "testing"

func TestPrepareObjectStreamingOverridesGeneratedThreeSecondDefault(t *testing.T) {
	cfg := Config{}
	cfg.Timeout = 3000
	cfg.Middlewares.Timeout = true
	cfg.Middlewares.Recover = true

	cfg.PrepareObjectStreaming()

	if cfg.Timeout != minimumObjectStreamNetworkTimeoutMS {
		t.Fatalf("network timeout = %dms", cfg.Timeout)
	}
	if cfg.Middlewares.Timeout || cfg.Middlewares.Recover {
		t.Fatalf("buffering/native recovery remained enabled: %#v", cfg.Middlewares)
	}
}
