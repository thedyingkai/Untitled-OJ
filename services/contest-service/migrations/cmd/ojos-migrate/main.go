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
	"ojos-contest-service/internal/config"
)

func main() {
	if err := run(os.Args[1:]); err != nil {
		log.Printf("migration failed: %v", err)
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
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	connection, err := pgx.Connect(ctx, dsn)
	if err != nil {
		return errors.New("connect to claimed PostgreSQL database")
	}
	defer connection.Close(context.Background())
	if _, err := connection.Exec(ctx, `SELECT pg_advisory_lock(hashtext('ojos:contest-service:migration'))`); err != nil {
		return err
	}
	defer connection.Exec(context.Background(), `SELECT pg_advisory_unlock(hashtext('ojos:contest-service:migration'))`)
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

func databaseDSN() (string, error) {
	path := strings.TrimSpace(os.Getenv("OJOS_RESOURCE_CONTESTS_OUTPUT_FILE"))
	if path == "" {
		path = strings.TrimSpace(os.Getenv("OJOS_RESOURCE_OUTPUT_FILE"))
	}
	if path == "" {
		path = "/run/ojos/resources/contests/dsn"
	}
	return config.ReadDatabaseDSN(path)
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
