// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package main

import (
	"flag"
	"fmt"
	"log"
	"os"

	"ojos-gateway/internal/app"
	"ojos-gateway/internal/config"
	"ojos-shared/servicehealth"

	"github.com/zeromicro/go-zero/core/conf"
)

var configFile = flag.String("f", "etc/gateway.yaml", "the config file")

func main() {
	if handled, err := servicehealth.RunIfRequested(os.Args, "http://127.0.0.1:8080/readyz"); handled {
		if err != nil {
			log.Fatal(err)
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
		fmt.Fprintln(os.Stderr, "start gateway:", err)
		os.Exit(1)
	}
}
