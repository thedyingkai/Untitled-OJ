package store

import (
	"bytes"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

type fakeS3Object struct {
	body        []byte
	contentType string
	sha256      string
	updatedAt   string
}

type fakeS3Server struct {
	mu      sync.Mutex
	buckets map[string]struct{}
	objects map[string]fakeS3Object
}

func newFakeS3Server() *fakeS3Server {
	return &fakeS3Server{
		buckets: make(map[string]struct{}),
		objects: make(map[string]fakeS3Object),
	}
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
		object, ok := s.objects[objectKey]
		if !ok {
			http.NotFound(w, r)
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

	meta, err := objectStore.Put("submissions", "42/main.cpp", "text/x-c++src", bytes.NewBufferString("int main(){}"))
	if err != nil {
		t.Fatalf("put object: %v", err)
	}
	if meta.Bucket != "submissions" || meta.Key != "42/main.cpp" || meta.SizeBytes != int64(len("int main(){}")) {
		t.Fatalf("unexpected metadata: %#v", meta)
	}

	stored, err := objectStore.Metadata("submissions", "42/main.cpp")
	if err != nil {
		t.Fatalf("metadata: %v", err)
	}
	if stored.SHA256 != meta.SHA256 || stored.ContentType != "text/x-c++src" {
		t.Fatalf("metadata mismatch: got %#v want sha %q", stored, meta.SHA256)
	}

	req := httptest.NewRequest(http.MethodGet, "/objects/submissions/42/main.cpp", nil)
	rec := httptest.NewRecorder()
	if err := objectStore.Serve(rec, req, "submissions", "42/main.cpp"); err != nil {
		t.Fatalf("serve get: %v", err)
	}
	if rec.Body.String() != "int main(){}" {
		t.Fatalf("unexpected body %q", rec.Body.String())
	}
	if rec.Header().Get("X-OJOS-Object-Sha256") != meta.SHA256 {
		t.Fatalf("missing sha header")
	}

	headReq := httptest.NewRequest(http.MethodHead, "/objects/submissions/42/main.cpp", nil)
	headRec := httptest.NewRecorder()
	if err := objectStore.Serve(headRec, headReq, "submissions", "42/main.cpp"); err != nil {
		t.Fatalf("serve head: %v", err)
	}
	if headRec.Body.Len() != 0 {
		t.Fatalf("head should not write body")
	}

	if err := objectStore.Delete("submissions", "42/main.cpp"); err != nil {
		t.Fatalf("delete: %v", err)
	}
	if _, err := objectStore.Metadata("submissions", "42/main.cpp"); err == nil {
		t.Fatalf("deleted object metadata should fail")
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
