package servicehealth

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"time"
)

const Command = "healthcheck"

// RunIfRequested implements the common exec-form container health command.
// It is deliberately HTTP-only and loopback-only: a service proves its own
// listener without depending on curl, a shell, DNS, or another component.
func RunIfRequested(args []string, target string) (bool, error) {
	if len(args) != 2 || args[1] != Command {
		return false, nil
	}
	parsed, err := url.Parse(target)
	if err != nil || parsed.Scheme != "http" || parsed.User != nil || parsed.RawQuery != "" || parsed.Fragment != "" {
		return true, errors.New("healthcheck target must be a plain loopback HTTP URL")
	}
	if parsed.Hostname() != "127.0.0.1" && parsed.Hostname() != "localhost" && parsed.Hostname() != "::1" {
		return true, errors.New("healthcheck target must use a loopback host")
	}
	client := &http.Client{
		Timeout: 3 * time.Second,
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
	request, err := http.NewRequest(http.MethodGet, parsed.String(), nil)
	if err != nil {
		return true, fmt.Errorf("create healthcheck request: %w", err)
	}
	response, err := client.Do(request)
	if err != nil {
		return true, fmt.Errorf("healthcheck request failed: %w", err)
	}
	defer response.Body.Close()
	_, _ = io.Copy(io.Discard, io.LimitReader(response.Body, 4*1024))
	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		return true, fmt.Errorf("healthcheck returned %s", response.Status)
	}
	return true, nil
}
