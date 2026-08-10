package repository

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"golang.org/x/crypto/bcrypt"
)

// TestAdminBootstrapRepositoryPostgresSingleWinner is an opt-in real
// PostgreSQL contract test. CI/E2E supplies a disposable database through
// AUTH_BOOTSTRAP_TEST_DATABASE_URL; ordinary package tests skip it.
func TestAdminBootstrapRepositoryPostgresSingleWinner(t *testing.T) {
	databaseURL := os.Getenv("AUTH_BOOTSTRAP_TEST_DATABASE_URL")
	if databaseURL == "" {
		t.Skip("AUTH_BOOTSTRAP_TEST_DATABASE_URL is not configured")
	}
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()

	adminPool, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	defer adminPool.Close()
	schema := fmt.Sprintf("auth_bootstrap_test_%d", time.Now().UnixNano())
	if _, err := adminPool.Exec(ctx, "CREATE SCHEMA "+schema); err != nil {
		t.Fatal(err)
	}
	defer func() { _, _ = adminPool.Exec(context.Background(), "DROP SCHEMA "+schema+" CASCADE") }()

	poolConfig, err := pgxpool.ParseConfig(databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	poolConfig.ConnConfig.RuntimeParams["search_path"] = schema
	testPool, err := pgxpool.NewWithConfig(ctx, poolConfig)
	if err != nil {
		t.Fatal(err)
	}
	defer testPool.Close()
	for _, migration := range []string{
		"000001_init_schema.up.sql",
		"000003_permission_core.up.sql",
		"000014_initial_admin_bootstrap.up.sql",
	} {
		contents, err := os.ReadFile(filepath.Join("..", "..", "migrations", migration))
		if err != nil {
			t.Fatal(err)
		}
		if _, err := testPool.Exec(ctx, string(contents)); err != nil {
			t.Fatalf("apply %s: %v", migration, err)
		}
	}

	repository := NewAdminBootstrapRepository(testPool)
	if err := repository.ValidateState(ctx); err != nil {
		t.Fatal(err)
	}
	password := "correct horse battery staple"
	passwordHash, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.MinCost)
	if err != nil {
		t.Fatal(err)
	}
	type result struct {
		userID int64
		err    error
	}
	results := make(chan result, 2)
	var start sync.WaitGroup
	start.Add(1)
	for index := 0; index < 2; index++ {
		index := index
		go func() {
			start.Wait()
			userID, err := repository.BootstrapAdmin(
				ctx,
				fmt.Sprintf("initial-admin-%d", index),
				fmt.Sprintf("admin-%d@example.test", index),
				string(passwordHash),
			)
			results <- result{userID: userID, err: err}
		}()
	}
	start.Done()

	successes := 0
	consumed := 0
	var winnerID int64
	for index := 0; index < 2; index++ {
		result := <-results
		switch {
		case result.err == nil:
			successes++
			winnerID = result.userID
		case errors.Is(result.err, ErrAdminBootstrapConsumed):
			consumed++
		default:
			t.Fatalf("unexpected concurrent result: user=%d err=%v", result.userID, result.err)
		}
	}
	if successes != 1 || consumed != 1 || winnerID == 0 {
		t.Fatalf("expected one winner and one consumed request; successes=%d consumed=%d winner=%d", successes, consumed, winnerID)
	}

	var completed bool
	var roleCount int
	var auditCount int
	var storedPasswordHash string
	if err := testPool.QueryRow(ctx, `
SELECT completed_at IS NOT NULL AND user_id = $1
FROM auth_bootstrap_state
WHERE bootstrap_key = 'initial-super-admin'
`, winnerID).Scan(&completed); err != nil {
		t.Fatal(err)
	}
	if err := testPool.QueryRow(ctx, `
SELECT COUNT(*)
FROM user_roles ur
JOIN roles r ON r.id = ur.role_id
WHERE ur.user_id = $1 AND r.name IN ('user', 'super_admin')
`, winnerID).Scan(&roleCount); err != nil {
		t.Fatal(err)
	}
	if err := testPool.QueryRow(ctx, `
SELECT COUNT(*)
FROM permission_audit_logs
WHERE action = 'auth.bootstrap.initial_admin' AND target_id = $1
`, winnerID).Scan(&auditCount); err != nil {
		t.Fatal(err)
	}
	if err := testPool.QueryRow(ctx, `SELECT password_hash FROM users WHERE id = $1`, winnerID).Scan(&storedPasswordHash); err != nil {
		t.Fatal(err)
	}
	if !completed || roleCount != 2 || auditCount != 1 {
		t.Fatalf("invalid committed state: completed=%v roles=%d audit=%d", completed, roleCount, auditCount)
	}
	if err := bcrypt.CompareHashAndPassword([]byte(storedPasswordHash), []byte(password)); err != nil {
		t.Fatalf("committed bootstrap password cannot be used for login: %v", err)
	}

	_, err = NewAdminBootstrapRepository(testPool).BootstrapAdmin(
		ctx,
		"restart-attempt",
		"restart@example.test",
		"bcrypt-hash-placeholder",
	)
	if !errors.Is(err, ErrAdminBootstrapConsumed) {
		t.Fatalf("restart did not preserve consumed marker: %v", err)
	}
}
