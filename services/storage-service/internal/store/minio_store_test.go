package store

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"ojos-shared/storagecontract"
)

type fakeS3Object struct {
	body        []byte
	contentType string
	sha256      string
	updatedAt   string
}

type fakeS3Server struct {
	mu                 sync.Mutex
	buckets            map[string]struct{}
	objects            map[string]fakeS3Object
	objectHeadFailures map[string]fakeS3Failure
}

type fakeS3Failure struct {
	status int
	code   string
}

func newFakeS3Server() *fakeS3Server {
	return &fakeS3Server{
		buckets:            make(map[string]struct{}),
		objects:            make(map[string]fakeS3Object),
		objectHeadFailures: make(map[string]fakeS3Failure),
	}
}

func (s *fakeS3Server) failObjectHead(objectKey string, failure fakeS3Failure) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.objectHeadFailures[objectKey] = failure
}

func (s *fakeS3Server) removeBucket(bucket string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.buckets, bucket)
}

func (s *fakeS3Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	bucket, key := splitS3Path(r.URL.Path)
	if bucket == "" {
		http.NotFound(w, r)
		return
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	if key == "" {
		switch r.Method {
		case http.MethodHead:
			if _, ok := s.buckets[bucket]; !ok {
				http.NotFound(w, r)
				return
			}
			w.WriteHeader(http.StatusOK)
		case http.MethodGet:
			if _, ok := s.buckets[bucket]; !ok {
				http.NotFound(w, r)
				return
			}
			w.WriteHeader(http.StatusOK)
		case http.MethodPut:
			s.buckets[bucket] = struct{}{}
			w.WriteHeader(http.StatusOK)
		default:
			http.Error(w, "unsupported bucket method", http.StatusMethodNotAllowed)
		}
		return
	}

	objectKey := bucket + "/" + key
	switch r.Method {
	case http.MethodPut:
		if _, ok := s.buckets[bucket]; !ok {
			http.NotFound(w, r)
			return
		}
		if r.Header.Get("If-None-Match") == "*" {
			if _, exists := s.objects[objectKey]; exists {
				http.Error(w, "precondition failed", http.StatusPreconditionFailed)
				return
			}
		}
		body, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		if strings.EqualFold(r.Header.Get("Content-Encoding"), "aws-chunked") ||
			bytes.Contains(body, []byte(";chunk-signature=")) {
			var decodeErr error
			body, decodeErr = decodeAWSChunkedBody(body)
			if decodeErr != nil {
				http.Error(w, decodeErr.Error(), http.StatusBadRequest)
				return
			}
		}
		s.objects[objectKey] = fakeS3Object{
			body:        body,
			contentType: r.Header.Get("Content-Type"),
			sha256:      r.Header.Get("X-Amz-Meta-Ojos-Sha256"),
			updatedAt:   r.Header.Get("X-Amz-Meta-Ojos-Updated-At"),
		}
		w.WriteHeader(http.StatusOK)
	case http.MethodHead:
		if _, ok := s.buckets[bucket]; !ok {
			writeFakeS3Error(w, http.StatusNotFound, "NoSuchBucket")
			return
		}
		if failure, ok := s.objectHeadFailures[objectKey]; ok {
			writeFakeS3Error(w, failure.status, failure.code)
			return
		}
		object, ok := s.objects[objectKey]
		if !ok {
			writeFakeS3Error(w, http.StatusNotFound, "NoSuchKey")
			return
		}
		writeFakeS3ObjectHeaders(w, object)
		w.WriteHeader(http.StatusOK)
	case http.MethodGet:
		object, ok := s.objects[objectKey]
		if !ok {
			http.NotFound(w, r)
			return
		}
		writeFakeS3ObjectHeaders(w, object)
		_, _ = w.Write(object.body)
	case http.MethodDelete:
		delete(s.objects, objectKey)
		w.WriteHeader(http.StatusNoContent)
	default:
		http.Error(w, "unsupported object method", http.StatusMethodNotAllowed)
	}
}

func TestMinIOObjectStoreLifecycleWithFakeS3(t *testing.T) {
	fake := newFakeS3Server()
	server := httptest.NewServer(fake)
	defer server.Close()
	payload := bytes.Repeat([]byte("judge-package-payload\n"), 64*1024)

	objectStore, err := NewMinIOObjectStore(MinIOOptions{
		Endpoint:  strings.TrimPrefix(server.URL, "http://"),
		AccessKey: "minio",
		SecretKey: "secret",
		UseSSL:    false,
	}, []string{"submissions", "judge-artifacts"})
	if err != nil {
		t.Fatalf("new minio object store: %v", err)
	}
	if objectStore.Backend() != "minio" {
		t.Fatalf("unexpected backend %q", objectStore.Backend())
	}

	meta, err := objectStore.Put(context.Background(), "submissions", "42/main.cpp", PutOptions{ContentType: "text/x-c++src"}, bytes.NewReader(payload))
	if err != nil {
		t.Fatalf("put object: %v", err)
	}
	if meta.Bucket != "submissions" || meta.Key != "42/main.cpp" || meta.SizeBytes != int64(len(payload)) {
		t.Fatalf("unexpected metadata: %#v", meta)
	}

	stored, err := objectStore.Metadata("submissions", "42/main.cpp")
	if err != nil {
		t.Fatalf("metadata: %v", err)
	}
	if stored.SHA256 != meta.SHA256 || stored.ContentType != "text/x-c++src" {
		t.Fatalf("metadata mismatch: got %#v want sha %q", stored, meta.SHA256)
	}
	_, err = objectStore.Put(context.Background(), "submissions", "42/main.cpp", PutOptions{IfAbsent: true}, bytes.NewBufferString("replacement"))
	if !errors.Is(err, ErrPreconditionFailed) {
		t.Fatalf("expected MinIO precondition failure, got %v", err)
	}

	req := httptest.NewRequest(http.MethodGet, "/objects/submissions/42/main.cpp", nil)
	rec := httptest.NewRecorder()
	if err := objectStore.Serve(rec, req, "submissions", "42/main.cpp"); err != nil {
		t.Fatalf("serve get: %v", err)
	}
	if !bytes.Equal(rec.Body.Bytes(), payload) {
		t.Fatalf("unexpected body size %d", rec.Body.Len())
	}
	if rec.Header().Get("X-OJOS-Object-Sha256") != meta.SHA256 {
		t.Fatalf("missing sha header")
	}
	if got := rec.Header().Get("Content-Length"); got != strconv.FormatInt(meta.SizeBytes, 10) {
		t.Fatalf("MinIO GET Content-Length = %q, want %d", got, meta.SizeBytes)
	}

	headReq := httptest.NewRequest(http.MethodHead, "/objects/submissions/42/main.cpp", nil)
	headRec := httptest.NewRecorder()
	if err := objectStore.Serve(headRec, headReq, "submissions", "42/main.cpp"); err != nil {
		t.Fatalf("serve head: %v", err)
	}
	if headRec.Body.Len() != 0 {
		t.Fatalf("head should not write body")
	}
	if got := headRec.Header().Get("Content-Length"); got != strconv.FormatInt(meta.SizeBytes, 10) {
		t.Fatalf("MinIO HEAD Content-Length = %q, want %d", got, meta.SizeBytes)
	}
	if got := headRec.Header().Get(storagecontract.ResultHeader); got != storagecontract.ResultPresent {
		t.Fatalf("MinIO HEAD result = %q, want %q", got, storagecontract.ResultPresent)
	}

	cancelledCtx, cancel := context.WithCancel(context.Background())
	cancel()
	cancelledReq := httptest.NewRequest(http.MethodGet, "/objects/submissions/42/main.cpp", nil).WithContext(cancelledCtx)
	if err := objectStore.Serve(httptest.NewRecorder(), cancelledReq, "submissions", "42/main.cpp"); err == nil {
		t.Fatalf("serve should propagate request cancellation")
	}

	if err := objectStore.DeleteIfMatches(context.Background(), "submissions", "42/main.cpp", strings.Repeat("0", 64), meta.SizeBytes); !errors.Is(err, ErrPreconditionFailed) {
		t.Fatalf("mismatched MinIO conditional delete must fail closed: %v", err)
	}
	if _, err := objectStore.Metadata("submissions", "42/main.cpp"); err != nil {
		t.Fatalf("mismatched MinIO delete removed object: %v", err)
	}
	if err := objectStore.DeleteIfMatches(context.Background(), "submissions", "42/main.cpp", meta.SHA256, meta.SizeBytes); err != nil {
		t.Fatalf("conditional delete: %v", err)
	}
	if _, err := objectStore.Metadata("submissions", "42/main.cpp"); !errors.Is(err, ErrObjectNotFound) {
		t.Fatalf("deleted object metadata should be not found, got %v", err)
	}
	missingHead := httptest.NewRequest(http.MethodHead, "/objects/submissions/42/main.cpp", nil)
	missingHeadRec := httptest.NewRecorder()
	if err := objectStore.Serve(missingHeadRec, missingHead, "submissions", "42/main.cpp"); !errors.Is(err, ErrObjectNotFound) {
		t.Fatalf("missing MinIO HEAD should be not found, got %v", err)
	}
	if got := missingHeadRec.Header().Get(storagecontract.ResultHeader); got != storagecontract.ResultObjectNotFound {
		t.Fatalf("missing MinIO HEAD result = %q, want %q", got, storagecontract.ResultObjectNotFound)
	}

	fake.failObjectHead("submissions/backend-error", fakeS3Failure{
		status: http.StatusForbidden,
		code:   "AccessDenied",
	})
	backendFailureHead := httptest.NewRequest(http.MethodHead, "/objects/submissions/backend-error", nil)
	err = objectStore.Serve(httptest.NewRecorder(), backendFailureHead, "submissions", "backend-error")
	if err == nil || errors.Is(err, ErrObjectNotFound) {
		t.Fatalf("non-missing MinIO HEAD error must be preserved, got %v", err)
	}

	emptyMeta, err := objectStore.Put(context.Background(), "submissions", "empty.in", PutOptions{}, bytes.NewReader(nil))
	if err != nil {
		t.Fatalf("put empty MinIO object: %v", err)
	}
	if emptyMeta.SizeBytes != 0 {
		t.Fatalf("empty MinIO object size = %d, want 0", emptyMeta.SizeBytes)
	}
	emptyHead := httptest.NewRequest(http.MethodHead, "/objects/submissions/empty.in", nil)
	emptyHeadRec := httptest.NewRecorder()
	if err := objectStore.Serve(emptyHeadRec, emptyHead, "submissions", "empty.in"); err != nil {
		t.Fatalf("head empty MinIO object: %v", err)
	}
	if got := emptyHeadRec.Header().Get("Content-Length"); got != "0" {
		t.Fatalf("empty MinIO object HEAD Content-Length = %q, want 0", got)
	}
	if err := objectStore.DeleteIfMatches(context.Background(), "submissions", "empty.in", emptyMeta.SHA256, 0); err != nil {
		t.Fatalf("conditional delete empty MinIO object: %v", err)
	}
	if _, err := objectStore.Metadata("submissions", "empty.in"); !errors.Is(err, ErrObjectNotFound) {
		t.Fatalf("empty MinIO object still exists after conditional delete: %v", err)
	}

	fake.removeBucket("submissions")
	missingBucketHead := httptest.NewRequest(http.MethodHead, "/objects/submissions/backend-error", nil)
	err = objectStore.Serve(httptest.NewRecorder(), missingBucketHead, "submissions", "backend-error")
	if err == nil || errors.Is(err, ErrObjectNotFound) {
		t.Fatalf("missing MinIO bucket must not be reported as a missing object, got %v", err)
	}
}

func splitS3Path(rawPath string) (string, string) {
	parts := strings.SplitN(strings.TrimPrefix(rawPath, "/"), "/", 2)
	if len(parts) == 0 {
		return "", ""
	}
	if len(parts) == 1 {
		return parts[0], ""
	}
	return parts[0], parts[1]
}

func writeFakeS3ObjectHeaders(w http.ResponseWriter, object fakeS3Object) {
	w.Header().Set("Content-Length", strconv.Itoa(len(object.body)))
	w.Header().Set("Content-Type", object.contentType)
	w.Header().Set("Last-Modified", time.Now().UTC().Format(http.TimeFormat))
	w.Header().Set("X-Amz-Meta-Ojos-Sha256", object.sha256)
	w.Header().Set("X-Amz-Meta-Ojos-Updated-At", object.updatedAt)
}

func writeFakeS3Error(w http.ResponseWriter, status int, code string) {
	w.Header().Set("Content-Type", "application/xml")
	w.WriteHeader(status)
	_, _ = io.WriteString(w, "<Error><Code>"+code+"</Code><Message>fake S3 error</Message></Error>")
}

func decodeAWSChunkedBody(data []byte) ([]byte, error) {
	var decoded bytes.Buffer
	for len(data) > 0 {
		lineEnd := bytes.Index(data, []byte("\r\n"))
		if lineEnd < 0 {
			return nil, io.ErrUnexpectedEOF
		}
		sizeText := string(data[:lineEnd])
		if semicolon := strings.IndexByte(sizeText, ';'); semicolon >= 0 {
			sizeText = sizeText[:semicolon]
		}
		size, err := strconv.ParseInt(sizeText, 16, 64)
		if err != nil {
			return nil, err
		}
		data = data[lineEnd+2:]
		if size == 0 {
			return decoded.Bytes(), nil
		}
		chunkSize := int(size)
		if len(data) < chunkSize+2 {
			return nil, io.ErrUnexpectedEOF
		}
		decoded.Write(data[:chunkSize])
		data = data[chunkSize+2:]
	}
	return decoded.Bytes(), nil
}
