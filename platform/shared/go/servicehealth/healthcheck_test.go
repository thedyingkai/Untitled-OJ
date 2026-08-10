package servicehealth

import (
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
)

func loopbackServer(t *testing.T, status int) *httptest.Server {
	t.Helper()
	server := httptest.NewUnstartedServer(http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) {
		writer.WriteHeader(status)
	}))
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	server.Listener = listener
	server.Start()
	t.Cleanup(server.Close)
	return server
}

func TestRunIfRequested(t *testing.T) {
	server := loopbackServer(t, http.StatusNoContent)
	handled, err := RunIfRequested([]string{"service", Command}, server.URL+"/health")
	if !handled || err != nil {
		t.Fatalf("expected successful handled probe, handled=%v err=%v", handled, err)
	}

	handled, err = RunIfRequested([]string{"service", "serve"}, server.URL)
	if handled || err != nil {
		t.Fatalf("ordinary command was intercepted, handled=%v err=%v", handled, err)
	}
}

func TestRunIfRequestedFailsClosed(t *testing.T) {
	server := loopbackServer(t, http.StatusServiceUnavailable)
	for name, target := range map[string]string{
		"unhealthy": server.URL + "/health",
		"remote":    "http://example.invalid/health",
		"redirect":  server.URL + "/redirect?query=forbidden",
	} {
		t.Run(name, func(t *testing.T) {
			handled, err := RunIfRequested([]string{"service", Command}, target)
			if !handled || err == nil {
				t.Fatalf("expected a handled failure, handled=%v err=%v", handled, err)
			}
		})
	}
}
