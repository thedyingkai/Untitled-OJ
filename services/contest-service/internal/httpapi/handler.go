package httpapi

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"strconv"
	"strings"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"ojos-contest-service/internal/contest"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
)

type ProblemReader interface {
	Probe(context.Context) error
}

type Handler struct {
	repository    contest.Repository
	problemReader ProblemReader
	permission    sharedperm.UserChecker
	logger        *slog.Logger
	requests      *prometheus.CounterVec
	duration      *prometheus.HistogramVec
	metrics       http.Handler
}

func New(repository contest.Repository, problemReader ProblemReader, permission sharedperm.UserChecker, logger *slog.Logger, registry *prometheus.Registry) (*Handler, error) {
	if repository == nil {
		return nil, errors.New("contest repository is required")
	}
	if logger == nil {
		logger = slog.New(slog.NewJSONHandler(io.Discard, nil))
	}
	if registry == nil {
		registry = prometheus.NewRegistry()
	}
	requests := prometheus.NewCounterVec(prometheus.CounterOpts{
		Namespace: "ojos", Subsystem: "contest_service", Name: "http_requests_total",
		Help: "Completed Contest Service HTTP requests.",
	}, []string{"operation", "method", "status"})
	duration := prometheus.NewHistogramVec(prometheus.HistogramOpts{
		Namespace: "ojos", Subsystem: "contest_service", Name: "http_request_duration_seconds",
		Help: "Contest Service request duration.", Buckets: prometheus.DefBuckets,
	}, []string{"operation", "method"})
	if err := registry.Register(requests); err != nil {
		return nil, err
	}
	if err := registry.Register(duration); err != nil {
		return nil, err
	}
	return &Handler{
		repository: repository, problemReader: problemReader, permission: permission, logger: logger,
		requests: requests, duration: duration, metrics: promhttp.HandlerFor(registry, promhttp.HandlerOpts{}),
	}, nil
}

func (handler *Handler) Routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", handler.observe("contestHealth", handler.health))
	mux.HandleFunc("GET /readyz", handler.observe("contestReady", handler.ready))
	mux.Handle("GET /metrics", handler.metrics)
	mux.HandleFunc("GET /contests", handler.authorize("listContests", handler.observe("listContests", handler.list)))
	mux.HandleFunc("POST /contests", handler.authorize("createContest", handler.observe("createContest", handler.create)))
	mux.HandleFunc("GET /contests/{contestId}", handler.authorize("getContest", handler.observe("getContest", handler.get)))
	mux.HandleFunc("PUT /contests/{contestId}", handler.authorize("updateContest", handler.observe("updateContest", handler.update)))
	mux.HandleFunc("DELETE /contests/{contestId}", handler.authorize("deleteContest", handler.observe("deleteContest", handler.delete)))
	mux.HandleFunc("GET /admin/contests", handler.authorize("adminListContests", handler.observe("adminListContests", handler.list)))
	return recovery(handler.logger, mux)
}

// authorize mirrors the operation-level permission compiled into the Gateway
// Contribution. Unmanaged tests and local development may omit a checker, but
// managed startup always supplies the Agent-backed checker.
func (handler *Handler) authorize(operation string, next http.HandlerFunc) http.HandlerFunc {
	return func(writer http.ResponseWriter, request *http.Request) {
		if handler.permission == nil {
			next(writer, request)
			return
		}
		permission := RequiredPermission(operation)
		if permission == "" {
			writeError(writer, http.StatusInternalServerError, "permission_contract_invalid", "operation permission is unavailable")
			return
		}
		user, err := authctx.FromHeaders(request.Header)
		if err != nil || user == nil || user.UserID <= 0 {
			writeError(writer, http.StatusUnauthorized, "unauthorized", "authenticated user context is required")
			return
		}
		if err := handler.permission.RequireUserPermission(request.Context(), user.UserID, permission, sharedperm.SystemScope()); err != nil {
			if errors.Is(err, sharedperm.ErrForbidden) {
				writeError(writer, http.StatusForbidden, "forbidden", "permission denied")
			} else {
				writeError(writer, http.StatusServiceUnavailable, "permission_unavailable", "permission service is unavailable")
			}
			return
		}
		next(writer, request.WithContext(authctx.NewContext(request.Context(), user)))
	}
}

func (handler *Handler) observe(operation string, next http.HandlerFunc) http.HandlerFunc {
	return func(writer http.ResponseWriter, request *http.Request) {
		started := time.Now()
		statusWriter := &statusRecorder{ResponseWriter: writer}
		next(statusWriter, request)
		if statusWriter.status == 0 {
			statusWriter.status = http.StatusOK
		}
		status := strconv.Itoa(statusWriter.status)
		handler.requests.WithLabelValues(operation, request.Method, status).Inc()
		handler.duration.WithLabelValues(operation, request.Method).Observe(time.Since(started).Seconds())
		handler.logger.InfoContext(request.Context(), "http request", "operation", operation, "method", request.Method, "status", statusWriter.status)
	}
}

func (handler *Handler) health(writer http.ResponseWriter, request *http.Request) {
	writeJSON(writer, http.StatusOK, map[string]string{"status": "ok"})
}

func (handler *Handler) ready(writer http.ResponseWriter, request *http.Request) {
	ctx, cancel := context.WithTimeout(request.Context(), time.Second)
	defer cancel()
	if err := handler.repository.Ping(ctx); err != nil {
		writeError(writer, http.StatusServiceUnavailable, "database_unavailable", "database is unavailable")
		return
	}
	if handler.problemReader != nil {
		if err := handler.problemReader.Probe(ctx); err != nil {
			writeError(writer, http.StatusServiceUnavailable, "problem_api_unavailable", "required Problem API is unavailable")
			return
		}
	}
	writeJSON(writer, http.StatusOK, map[string]string{"status": "ok"})
}

func (handler *Handler) list(writer http.ResponseWriter, request *http.Request) {
	items, err := handler.repository.List(request.Context())
	if err != nil {
		writeRepositoryError(writer, err)
		return
	}
	writeJSON(writer, http.StatusOK, map[string]any{"items": items})
}

func (handler *Handler) create(writer http.ResponseWriter, request *http.Request) {
	var input contest.CreateInput
	if err := decodeJSON(request, &input); err != nil {
		writeError(writer, http.StatusBadRequest, "invalid_request", "request body is invalid")
		return
	}
	item, err := handler.repository.Create(request.Context(), input)
	if err != nil {
		writeRepositoryError(writer, err)
		return
	}
	writeJSON(writer, http.StatusCreated, item)
}

func (handler *Handler) get(writer http.ResponseWriter, request *http.Request) {
	id, ok := contestID(writer, request)
	if !ok {
		return
	}
	item, err := handler.repository.Get(request.Context(), id)
	if err != nil {
		writeRepositoryError(writer, err)
		return
	}
	writeJSON(writer, http.StatusOK, item)
}

func (handler *Handler) update(writer http.ResponseWriter, request *http.Request) {
	id, ok := contestID(writer, request)
	if !ok {
		return
	}
	var input contest.UpdateInput
	if err := decodeJSON(request, &input); err != nil {
		writeError(writer, http.StatusBadRequest, "invalid_request", "request body is invalid")
		return
	}
	item, err := handler.repository.Update(request.Context(), id, input)
	if err != nil {
		writeRepositoryError(writer, err)
		return
	}
	writeJSON(writer, http.StatusOK, item)
}

func (handler *Handler) delete(writer http.ResponseWriter, request *http.Request) {
	id, ok := contestID(writer, request)
	if !ok {
		return
	}
	if err := handler.repository.Delete(request.Context(), id); err != nil {
		writeRepositoryError(writer, err)
		return
	}
	writer.WriteHeader(http.StatusNoContent)
}

func contestID(writer http.ResponseWriter, request *http.Request) (int64, bool) {
	id, err := strconv.ParseInt(request.PathValue("contestId"), 10, 64)
	if err != nil || id < 1 {
		writeError(writer, http.StatusBadRequest, "invalid_contest_id", "contestId must be a positive integer")
		return 0, false
	}
	return id, true
}

func decodeJSON(request *http.Request, target any) error {
	defer request.Body.Close()
	decoder := json.NewDecoder(io.LimitReader(request.Body, 1<<20))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(target); err != nil {
		return err
	}
	var extra any
	if err := decoder.Decode(&extra); !errors.Is(err, io.EOF) {
		return errors.New("request body contains trailing data")
	}
	return nil
}

func writeRepositoryError(writer http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, contest.ErrInvalid):
		writeError(writer, http.StatusBadRequest, "invalid_contest", "contest is invalid")
	case errors.Is(err, contest.ErrNotFound):
		writeError(writer, http.StatusNotFound, "contest_not_found", "contest was not found")
	case errors.Is(err, contest.ErrConflict):
		writeError(writer, http.StatusConflict, "contest_conflict", "contest conflicts with current state")
	default:
		writeError(writer, http.StatusInternalServerError, "internal_error", "internal server error")
	}
}

func writeError(writer http.ResponseWriter, status int, code, message string) {
	writeJSON(writer, status, map[string]string{"code": code, "message": message})
}

func writeJSON(writer http.ResponseWriter, status int, value any) {
	writer.Header().Set("Content-Type", "application/json; charset=utf-8")
	writer.WriteHeader(status)
	_ = json.NewEncoder(writer).Encode(value)
}

func recovery(logger *slog.Logger, next http.Handler) http.Handler {
	return http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		defer func() {
			if recovered := recover(); recovered != nil {
				logger.ErrorContext(request.Context(), "panic recovered", "error", recovered)
				writeError(writer, http.StatusInternalServerError, "internal_error", "internal server error")
			}
		}()
		next.ServeHTTP(writer, request)
	})
}

type statusRecorder struct {
	http.ResponseWriter
	status int
}

func (writer *statusRecorder) WriteHeader(status int) {
	if writer.status != 0 {
		return
	}
	writer.status = status
	writer.ResponseWriter.WriteHeader(status)
}

func (writer *statusRecorder) Write(bytes []byte) (int, error) {
	if writer.status == 0 {
		writer.WriteHeader(http.StatusOK)
	}
	return writer.ResponseWriter.Write(bytes)
}

func RequiredPermission(operation string) string {
	switch strings.TrimSpace(operation) {
	case "listContests", "getContest":
		return "contest-service.contest.read"
	case "createContest", "updateContest", "deleteContest", "adminListContests":
		return "contest-service.contest.manage"
	default:
		return ""
	}
}
