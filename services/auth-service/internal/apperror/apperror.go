package apperror

import "net/http"

type Error struct {
	status int
	code   int
	msg    string
}

func (e Error) Error() string {
	return e.msg
}

func (e Error) HTTPStatus() int {
	return e.status
}

func (e Error) ErrorCode() int {
	return e.code
}

func (e Error) PublicMessage() string {
	return e.msg
}

func BadRequest(code int, msg string) error {
	return Error{status: http.StatusBadRequest, code: code, msg: msg}
}

func Unauthorized(code int, msg string) error {
	return Error{status: http.StatusUnauthorized, code: code, msg: msg}
}

func Forbidden(code int, msg string) error {
	return Error{status: http.StatusForbidden, code: code, msg: msg}
}

func NotFound(code int, msg string) error {
	return Error{status: http.StatusNotFound, code: code, msg: msg}
}

func Conflict(code int, msg string) error {
	return Error{status: http.StatusConflict, code: code, msg: msg}
}

func Internal(code int, msg string) error {
	return Error{status: http.StatusInternalServerError, code: code, msg: msg}
}

const (
	CodeInvalidRequest        = 40010
	CodeInvalidListQuery      = 40011
	CodeInvalidPrincipal      = 40012
	CodeInvalidScope          = 40013
	CodeInvalidCredential     = 40014
	CodeUnauthorized          = 40110
	CodeAdminRequired         = 40310
	CodePermissionDenied      = 40311
	CodeResourceNotFound      = 40410
	CodeConflict              = 40910
	CodeRepositoryUnavailable = 50310
)
