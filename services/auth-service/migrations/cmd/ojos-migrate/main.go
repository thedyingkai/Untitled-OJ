package main

import (
	"context"
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

const defaultResourceOutput = "/run/ojos/resources/auth/dsn"

func main() {
	if err := run(os.Args[1:]); err != nil {
		log.Printf("auth-service migration failed: %v", err)
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
		return errors.New("connect to claimed Auth PostgreSQL database")
	}
	defer connection.Close(context.Background())
	if _, err := connection.Exec(ctx, `SELECT pg_advisory_lock(hashtext('ojos:auth-service:migration'))`); err != nil {
		return errors.New("acquire auth-service migration lock")
	}
	defer connection.Exec(context.Background(), `SELECT pg_advisory_unlock(hashtext('ojos:auth-service:migration'))`)
	if _, err := connection.Exec(ctx, `CREATE TABLE IF NOT EXISTS ojos_schema_migrations (id TEXT PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW())`); err != nil {
		return errors.New("prepare migration ledger")
	}
	for _, path := range files {
		id := filepath.Base(path)
		var applied bool
		if err := connection.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM ojos_schema_migrations WHERE id=$1)`, id).Scan(&applied); err != nil {
			return errors.New("read migration ledger")
		}
		if applied {
			continue
		}
		sql, err := os.ReadFile(path)
		if err != nil {
			return fmt.Errorf("read migration %s: %w", id, err)
		}
		tx, err := connection.Begin(ctx)
		if err != nil {
			return errors.New("begin migration transaction")
		}
		if _, err := tx.Exec(ctx, string(sql)); err != nil {
			_ = tx.Rollback(ctx)
			return fmt.Errorf("apply migration %s: %w", id, err)
		}
		if _, err := tx.Exec(ctx, `INSERT INTO ojos_schema_migrations(id) VALUES($1)`, id); err != nil {
			_ = tx.Rollback(ctx)
			return errors.New("record migration ledger")
		}
		if err := tx.Commit(ctx); err != nil {
			return errors.New("commit migration transaction")
		}
	}
	return nil
}

func databaseDSN() (string, error) {
	path := strings.TrimSpace(os.Getenv("OJOS_RESOURCE_AUTH_OUTPUT_FILE"))
	if path == "" {
		path = strings.TrimSpace(os.Getenv("OJOS_RESOURCE_OUTPUT_FILE"))
	}
	if path == "" {
		path = defaultResourceOutput
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
