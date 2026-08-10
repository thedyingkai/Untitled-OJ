package config

import (
	"path/filepath"
	"testing"

	"github.com/zeromicro/go-zero/core/conf"
)

func TestProductionConfigKeepsOrdinaryTimeoutAndLongPollBudgetSeparate(t *testing.T) {
	var cfg Config
	if err := conf.Load(filepath.Join("..", "..", "etc", "judgeapi.yaml"), &cfg); err != nil {
		t.Fatal(err)
	}
	if cfg.Timeout != 3000 {
		t.Fatalf("ordinary API timeout = %dms, want 3000ms", cfg.Timeout)
	}
	if !cfg.Middlewares.Timeout {
		t.Fatal("ordinary API timeout middleware is disabled")
	}
}
