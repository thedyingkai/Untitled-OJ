package objectstoredeploy

import (
	"context"
	"errors"
	"net/http"
	"net/http/httputil"
	"net/url"
	"time"
)

// ServeS3 exposes only the S3 HTTP listener from a shared container network
// namespace. SeaweedFS binds its HTTP and gRPC listeners to the same address;
// keeping it entirely on loopback prevents its internal IAM cache API from
// becoming reachable by other workloads. It never adds or substitutes credentials.
func ServeS3(ctx context.Context) error {
	target := &url.URL{Scheme: "http", Host: "127.0.0.1:8334"}
	return serveS3(ctx, ":8333", target)
}

func serveS3(ctx context.Context, address string, target *url.URL) error {
	proxy := &httputil.ReverseProxy{
		Rewrite: func(r *httputil.ProxyRequest) {
			r.SetURL(target)
			// S3 signs Host. Preserve the client-signed authority while Go removes
			// untrusted forwarding headers before this rewrite runs.
			r.Out.Host = r.In.Host
		},
		FlushInterval: -1,
	}
	server := &http.Server{
		Addr: address, Handler: proxy,
		ReadHeaderTimeout: 10 * time.Second, IdleTimeout: 90 * time.Second,
	}
	stopped := make(chan struct{})
	shutdownDone := make(chan struct{})
	go func() {
		defer close(shutdownDone)
		select {
		case <-ctx.Done():
			shutdown, cancel := context.WithTimeout(context.Background(), 20*time.Second)
			defer cancel()
			if err := server.Shutdown(shutdown); err != nil {
				_ = server.Close()
			}
		case <-stopped:
		}
	}()
	err := server.ListenAndServe()
	close(stopped)
	// ListenAndServe stops accepting before Shutdown finishes active responses.
	// Keep the process alive until those responses drain or the grace expires.
	<-shutdownDone
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}
