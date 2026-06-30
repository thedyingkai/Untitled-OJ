//go:build tools

package main

//go:generate go run github.com/zeromicro/go-zero/tools/goctl@v1.10.1 api go -api judgeapi.api -dir . -style gozero
