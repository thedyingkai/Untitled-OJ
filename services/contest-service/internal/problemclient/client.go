package problemclient

import (
	"context"
	"fmt"
	"net/http"

	"ojos-shared/servicecontext"
)

const BindingName = "problem.problem.read"

type Client struct {
	provider *servicecontext.ContextProvider
}

func New(provider *servicecontext.ContextProvider) *Client { return &Client{provider: provider} }

func (client *Client) Probe(ctx context.Context) error {
	if client == nil || client.provider == nil {
		return &servicecontext.BindingUnavailable{Name: BindingName}
	}
	httpClient, err := client.provider.Client(ctx)
	if err != nil {
		return err
	}
	// The binding base path already identifies the required API. The Gateway
	// rewrites that virtual root to the provider's declared /problems path, so
	// appending /problems here would address /problems/problems upstream.
	response, err := client.provider.Do(ctx, httpClient, BindingName, http.MethodGet, "", nil)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return fmt.Errorf("problem API probe returned %s", response.Status)
	}
	return nil
}
