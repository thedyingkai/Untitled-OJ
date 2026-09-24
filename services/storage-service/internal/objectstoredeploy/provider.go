package objectstoredeploy

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"syscall"
	"time"
)

// RunProvider owns the fixed SeaweedFS process and its S3-only edge together.
// A separate container sharing a provider network namespace would retain a dead
// namespace after the provider restarts. One owner makes restart and shutdown
// atomic without exposing SeaweedFS's internal gRPC listeners.
func RunProvider(ctx context.Context, arguments []string) error {
	if len(arguments) == 0 || arguments[0] != "server" {
		return errors.New("serve requires the fixed SeaweedFS server command")
	}
	if err := RenderSeaweedConfig("/tmp/ojos-s3.json"); err != nil {
		return err
	}
	child := exec.Command("/usr/bin/weed", append([]string{"-logtostderr=true"}, arguments...)...)
	child.Stdout = os.Stdout
	child.Stderr = os.Stderr
	if err := child.Start(); err != nil {
		return fmt.Errorf("start object provider: %w", err)
	}
	childDone := make(chan error, 1)
	go func() { childDone <- child.Wait() }()
	edgeCtx, cancel := context.WithCancel(context.Background())
	defer cancel()
	edgeDone := make(chan error, 1)
	go func() { edgeDone <- ServeS3(edgeCtx) }()
	childStopped, edgeStopped := false, false
	var result error
	select {
	case <-ctx.Done():
	case err := <-childDone:
		childStopped = true
		result = errors.New("object provider exited unexpectedly")
		if err != nil {
			result = fmt.Errorf("object provider exited: %w", err)
		}
	case err := <-edgeDone:
		edgeStopped = true
		result = errors.New("S3 edge exited unexpectedly")
		if err != nil {
			result = fmt.Errorf("S3 edge exited: %w", err)
		}
	}
	// Drain the edge while its upstream is still alive, then stop and reap the
	// child. Docker's 45-second grace exceeds the 20+15-second internal bounds.
	cancel()
	if !edgeStopped {
		if err := <-edgeDone; err != nil {
			result = errors.Join(result, err)
		}
	}
	if !childStopped {
		_ = child.Process.Signal(syscall.SIGTERM)
		timer := time.NewTimer(15 * time.Second)
		defer timer.Stop()
		select {
		case err := <-childDone:
			if err != nil {
				result = errors.Join(result, fmt.Errorf("stop object provider: %w", err))
			}
		case <-timer.C:
			_ = child.Process.Kill()
			<-childDone
			result = errors.Join(result, errors.New("object provider exceeded shutdown grace"))
		}
	}
	return result
}
