// ojos-object-store prepares and provisions the optional self-hosted S3 service.
package main

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"syscall"
	"time"

	"ojos-storage-service/internal/objectstoredeploy"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "object-store:", err)
		os.Exit(1)
	}
}

func run() error {
	if len(os.Args) < 2 {
		return fmt.Errorf("usage: ojos-object-store render <path> | provision | ready | serve server [flags]")
	}
	switch os.Args[1] {
	case "serve":
		ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
		defer cancel()
		return objectstoredeploy.RunProvider(ctx, os.Args[2:])
	case "render":
		if len(os.Args) != 3 {
			return fmt.Errorf("render requires a private output path")
		}
		return objectstoredeploy.RenderSeaweedConfig(os.Args[2])
	case "provision", "ready":
		if len(os.Args) != 2 {
			return fmt.Errorf("unexpected arguments")
		}
		timeout := 2 * time.Minute
		if os.Args[1] == "ready" {
			timeout = 4 * time.Second
		}
		ctx, cancel := context.WithTimeout(context.Background(), timeout)
		defer cancel()
		if os.Args[1] == "ready" {
			return objectstoredeploy.Ready(ctx)
		}
		return objectstoredeploy.Provision(ctx)
	default:
		return fmt.Errorf("unknown command %q", os.Args[1])
	}
}
