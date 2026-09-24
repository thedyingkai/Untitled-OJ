// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package main

import (
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"time"

	"ojos-auth-service/internal/app"
	"ojos-auth-service/internal/config"

	"github.com/zeromicro/go-zero/core/conf"
)

var configFile = flag.String("f", "etc/auth.yaml", "the config file")

func main() {
	if len(os.Args) > 1 && os.Args[1] == "readycheck" {
		if err := readycheck(); err != nil {
			log.Print(err)
			os.Exit(1)
		}
		return
	}
	flag.Parse()

	var c config.Config
	if err := conf.Load(*configFile, &c); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	if err := app.Run(c); err != nil {
		fmt.Fprintln(os.Stderr, "start auth-service:", err)
		os.Exit(1)
	}
}

func readycheck() error {
	client := &http.Client{
		Timeout:       2 * time.Second,
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error { return http.ErrUseLastResponse },
	}
	request, err := http.NewRequest(http.MethodGet, "http://127.0.0.1:8081/readyz", nil)
	if err != nil {
		return err
	}
	response, err := client.Do(request)
	if err != nil {
		return fmt.Errorf("auth-service readiness probe failed: %w", err)
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return fmt.Errorf("auth-service readiness probe returned %s", response.Status)
	}
	return nil
}
