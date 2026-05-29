package response

import (
	"encoding/json"
	"net/http"
)

type Response struct {
	Code int         `json:"code"`
	Msg  string      `json:"msg"`
	Data interface{} `json:"data,omitempty"`
}

func JSON(w http.ResponseWriter, code int, msg string, data interface{}) {
	w.Header().Set("Content-Type", "application/json")

	w.WriteHeader(http.StatusOK)

	_ = json.NewEncoder(w).Encode(Response{
		Code: code,
		Msg:  msg,
		Data: data,
	})
}

func Success(w http.ResponseWriter, data interface{}) {
	JSON(w, 0, "success", data)
}

func Error(w http.ResponseWriter, code int, msg string) {
	JSON(w, code, msg, nil)
}
