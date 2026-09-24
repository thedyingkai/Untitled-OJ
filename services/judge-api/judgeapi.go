// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package main

import (
	"flag"
	"fmt"
	"os"

	"ojos-judge-api/internal/app"
	"ojos-judge-api/internal/config"
	"ojos-shared/servicehealth"

	"github.com/zeromicro/go-zero/core/conf"
)

var configFile = flag.String("f", "etc/judgeapi.yaml", "the config file")

func main() {
	if handled, err := servicehealth.RunIfRequested(os.Args, "http://127.0.0.1:8082/readyz"); handled {
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
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
		fmt.Fprintln(os.Stderr, "start judge-api:", err)
		os.Exit(1)
	}
}
