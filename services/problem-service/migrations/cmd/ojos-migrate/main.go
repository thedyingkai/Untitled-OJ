package main

import (
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"io/fs"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"ojos-shared/resourceoutput"
)

func main() {
	if err := run(os.Args[1:]); err != nil {
		log.Printf("problem migration failed: %v", err)
		os.Exit(1)
	}
}

func run(arguments []string) error {
	if len(arguments) > 0 && arguments[0] == "apply" {
		arguments = arguments[1:]
	}
	directory := "/migrations"
	if len(arguments) > 0 {
		directory = arguments[0]
	}
	dsn, err := databaseDSN()
	if err != nil {
		return err
	}
	files, err := migrationFiles(directory)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
	defer cancel()
	connection, err := pgx.Connect(ctx, dsn)
	if err != nil {
		return errors.New("connect to claimed problem PostgreSQL database")
	}
	defer connection.Close(context.Background())
	if _, err := connection.Exec(ctx, `SELECT pg_advisory_lock(hashtext('ojos:problem-service:migration'))`); err != nil {
		return err
	}
	defer connection.Exec(context.Background(), `SELECT pg_advisory_unlock(hashtext('ojos:problem-service:migration'))`)
	if _, err := connection.Exec(ctx, `CREATE TABLE IF NOT EXISTS ojos_schema_migrations (id TEXT PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW())`); err != nil {
		return err
	}
	for _, path := range files {
		id := filepath.Base(path)
		var applied bool
		if err := connection.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM ojos_schema_migrations WHERE id=$1)`, id).Scan(&applied); err != nil {
			return err
		}
		if applied {
			continue
		}
		sql, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		if migrationRunsOutsideTransaction(id, sql) {
			if _, err := connection.Exec(ctx, string(sql)); err != nil {
				return fmt.Errorf("apply %s: %w", id, err)
			}
			if _, err := connection.Exec(ctx, `INSERT INTO ojos_schema_migrations(id) VALUES($1)`, id); err != nil {
				return err
			}
			continue
		}
		tx, err := connection.Begin(ctx)
		if err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, string(sql)); err != nil {
			_ = tx.Rollback(ctx)
			return fmt.Errorf("apply %s: %w", id, err)
		}
		if _, err := tx.Exec(ctx, `INSERT INTO ojos_schema_migrations(id) VALUES($1)`, id); err != nil {
			_ = tx.Rollback(ctx)
			return err
		}
		if err := tx.Commit(ctx); err != nil {
			return err
		}
	}
	return nil
}

func migrationRunsOutsideTransaction(id string, sql []byte) bool {
	// PostgreSQL forbids CREATE INDEX CONCURRENTLY in a transaction. The sole
	// historical exception is pinned by both immutable migration ID and content
	// digest so a modified or newly added file cannot silently escape the
	// default transaction boundary.
	const concurrentIndexID = "000006_problem_files_storage_path_index.up.sql"
	const concurrentIndexSHA256 = "1ce67c6828f6d034326186cb21250fbf2442abc98b5ec170067c529bc037b686"
	if id != concurrentIndexID {
		return false
	}
	digest := sha256.Sum256(sql)
	return fmt.Sprintf("%x", digest[:]) == concurrentIndexSHA256
}

func databaseDSN() (string, error) {
	// Match the runtime lookup exactly: the claim-specific path is authoritative
	// when both variables are materialized during the compatibility release.
	path := strings.TrimSpace(os.Getenv("OJOS_RESOURCE_PROBLEMS_OUTPUT_FILE"))
	if path == "" {
		path = strings.TrimSpace(os.Getenv("OJOS_RESOURCE_OUTPUT_FILE"))
	}
	if path == "" {
		path = "/run/ojos/resources/problems/dsn"
	}
	return resourceoutput.ReadPostgreSQLDSN(path)
}

func migrationFiles(directory string) ([]string, error) {
	files := make([]string, 0)
	err := filepath.WalkDir(directory, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.Type().IsRegular() && strings.HasSuffix(entry.Name(), ".up.sql") {
			files = append(files, path)
		}
		return nil
	})
	sort.Strings(files)
	if err != nil {
		return nil, err
	}
	if len(files) == 0 {
		return nil, errors.New("no migration files found")
	}
	return files, nil
}
