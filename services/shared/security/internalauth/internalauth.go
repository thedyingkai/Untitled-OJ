package internalauth

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
)

const (
	HeaderKeyID     = "X-OJOS-Internal-Key-Id"
	HeaderTimestamp = "X-OJOS-Internal-Timestamp"
	HeaderNonce     = "X-OJOS-Internal-Nonce"
	HeaderBodyHash  = "X-OJOS-Internal-Body-SHA256"
	HeaderSignature = "X-OJOS-Internal-Signature"

	HeaderAuthVerified = "X-Auth-Verified"
	HeaderUserID       = "X-User-Id"
	HeaderUsername     = "X-Username"
	HeaderRoles        = "X-Roles"

	signatureVersion = "v1"

	defaultRotationInterval = 6 * time.Hour
	defaultVerifyGrace      = 10 * time.Minute
	defaultRotateBefore     = 2 * time.Minute
	defaultTimestampSkew    = 60 * time.Second
	defaultNonceTTL         = 120 * time.Second

	advisoryLockID int64 = 781206051337
)

var (
	ErrMissingInternalAuth = errors.New("missing internal auth")
	ErrInvalidSignature    = errors.New("invalid internal signature")
	ErrInvalidTimestamp    = errors.New("invalid internal timestamp")
	ErrTimestampSkew       = errors.New("internal auth timestamp skew exceeded")
	ErrReplay              = errors.New("internal auth nonce replay")
	ErrKeyNotFound         = errors.New("internal auth key not found")
	ErrInvalidBodyHash     = errors.New("invalid internal body hash")
)

type Config struct {
	Enabled bool

	RotationInterval time.Duration
	VerifyGrace      time.Duration
	RotateBefore     time.Duration
	TimestampSkew    time.Duration
	NonceTTL         time.Duration
}

func (c Config) normalized() Config {
	if c.RotationInterval <= 0 {
		c.RotationInterval = defaultRotationInterval
	}
	if c.VerifyGrace <= 0 {
		c.VerifyGrace = defaultVerifyGrace
	}
	if c.RotateBefore <= 0 {
		c.RotateBefore = defaultRotateBefore
	}
	if c.TimestampSkew <= 0 {
		c.TimestampSkew = defaultTimestampSkew
	}
	if c.NonceTTL <= 0 {
		c.NonceTTL = defaultNonceTTL
	}
	return c
}

type Key struct {
	KeyID       string
	Secret      []byte
	NotBefore   time.Time
	NotAfter    time.Time
	VerifyUntil time.Time
}

type KeyManager struct {
	db  *pgxpool.Pool
	cfg Config

	mu          sync.Mutex
	cachedKey   *Key
	cacheExpire time.Time
}

func NewKeyManager(db *pgxpool.Pool, cfg Config) *KeyManager {
	return &KeyManager{
		db:  db,
		cfg: cfg.normalized(),
	}
}

func (m *KeyManager) CurrentSigningKey(ctx context.Context) (*Key, error) {
	now := time.Now().UTC()

	m.mu.Lock()
	if m.cachedKey != nil &&
		now.Before(m.cacheExpire) &&
		now.After(m.cachedKey.NotBefore) &&
		now.Before(m.cachedKey.NotAfter.Add(-m.cfg.RotateBefore)) {
		key := cloneKey(m.cachedKey)
		m.mu.Unlock()
		return key, nil
	}
	m.mu.Unlock()

	key, err := m.loadCurrentSigningKey(ctx)
	if err == nil && now.Before(key.NotAfter.Add(-m.cfg.RotateBefore)) {
		m.setCachedKey(key)
		return key, nil
	}

	key, err = m.ensureFreshSigningKey(ctx)
	if err != nil {
		return nil, err
	}

	m.setCachedKey(key)
	return key, nil
}

func (m *KeyManager) VerifyKey(ctx context.Context, keyID string) (*Key, error) {
	keyID = strings.TrimSpace(keyID)
	if keyID == "" {
		return nil, ErrKeyNotFound
	}

	var key Key
	err := m.db.QueryRow(
		ctx,
		`
SELECT
    key_id,
    secret,
    not_before,
    not_after,
    verify_until
FROM internal_auth_keys
WHERE key_id = $1
  AND not_before <= NOW()
  AND verify_until > NOW()
`,
		keyID,
	).Scan(
		&key.KeyID,
		&key.Secret,
		&key.NotBefore,
		&key.NotAfter,
		&key.VerifyUntil,
	)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, ErrKeyNotFound
	}
	if err != nil {
		return nil, err
	}

	return &key, nil
}

func (m *KeyManager) setCachedKey(key *Key) {
	m.mu.Lock()
	defer m.mu.Unlock()

	m.cachedKey = cloneKey(key)
	m.cacheExpire = time.Now().UTC().Add(30 * time.Second)
}

func (m *KeyManager) loadCurrentSigningKey(ctx context.Context) (*Key, error) {
	var key Key
	err := m.db.QueryRow(
		ctx,
		`
SELECT
    key_id,
    secret,
    not_before,
    not_after,
    verify_until
FROM internal_auth_keys
WHERE not_before <= NOW()
  AND not_after > NOW()
ORDER BY not_after DESC
LIMIT 1
`,
	).Scan(
		&key.KeyID,
		&key.Secret,
		&key.NotBefore,
		&key.NotAfter,
		&key.VerifyUntil,
	)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, ErrKeyNotFound
	}
	if err != nil {
		return nil, err
	}

	return &key, nil
}

func (m *KeyManager) ensureFreshSigningKey(ctx context.Context) (*Key, error) {
	locked := false

	if err := m.db.QueryRow(
		ctx,
		`SELECT pg_try_advisory_lock($1)`,
		advisoryLockID,
	).Scan(&locked); err != nil {
		return nil, err
	}

	if !locked {
		time.Sleep(150 * time.Millisecond)

		key, err := m.loadCurrentSigningKey(ctx)
		if err == nil {
			return key, nil
		}
		return nil, err
	}

	defer func() {
		_, _ = m.db.Exec(context.Background(), `SELECT pg_advisory_unlock($1)`, advisoryLockID)
	}()

	now := time.Now().UTC()

	if key, err := m.loadCurrentSigningKey(ctx); err == nil {
		if now.Before(key.NotAfter.Add(-m.cfg.RotateBefore)) {
			return key, nil
		}
	}

	secret := make([]byte, 32)
	if _, err := rand.Read(secret); err != nil {
		return nil, err
	}

	suffix := make([]byte, 6)
	if _, err := rand.Read(suffix); err != nil {
		return nil, err
	}

	keyID := fmt.Sprintf(
		"v%s_%s",
		now.Format("20060102150405"),
		hex.EncodeToString(suffix),
	)

	notBefore := now.Add(-5 * time.Second)
	notAfter := now.Add(m.cfg.RotationInterval)
	verifyUntil := notAfter.Add(m.cfg.VerifyGrace)

	var key Key
	err := m.db.QueryRow(
		ctx,
		`
INSERT INTO internal_auth_keys(
    key_id,
    secret,
    not_before,
    not_after,
    verify_until,
    created_at
)
VALUES($1, $2, $3, $4, $5, NOW())
RETURNING
    key_id,
    secret,
    not_before,
    not_after,
    verify_until
`,
		keyID,
		secret,
		notBefore,
		notAfter,
		verifyUntil,
	).Scan(
		&key.KeyID,
		&key.Secret,
		&key.NotBefore,
		&key.NotAfter,
		&key.VerifyUntil,
	)
	if err != nil {
		return nil, err
	}

	return &key, nil
}

func cloneKey(key *Key) *Key {
	if key == nil {
		return nil
	}

	cp := *key
	cp.Secret = append([]byte(nil), key.Secret...)
	return &cp
}

type Signer struct {
	keys *KeyManager
}

func NewSigner(keys *KeyManager) *Signer {
	return &Signer{keys: keys}
}

func (s *Signer) SignRequest(ctx context.Context, req *http.Request) error {
	key, err := s.keys.CurrentSigningKey(ctx)
	if err != nil {
		return err
	}

	body, err := readAndRestoreBody(req)
	if err != nil {
		return err
	}

	bodyHash := sha256Hex(body)

	nonceBytes := make([]byte, 16)
	if _, err := rand.Read(nonceBytes); err != nil {
		return err
	}

	timestamp := strconv.FormatInt(time.Now().UTC().Unix(), 10)
	nonce := hex.EncodeToString(nonceBytes)

	req.Header.Set(HeaderKeyID, key.KeyID)
	req.Header.Set(HeaderTimestamp, timestamp)
	req.Header.Set(HeaderNonce, nonce)
	req.Header.Set(HeaderBodyHash, bodyHash)

	canonical := canonicalString(req, timestamp, nonce, bodyHash)
	signature := signCanonical(key.Secret, canonical)

	req.Header.Set(HeaderSignature, signatureVersion+"="+signature)
	return nil
}

type NonceStore interface {
	Use(ctx context.Context, nonce string, ttl time.Duration) error
}

type RedisNonceStore struct {
	Client *redis.Client
	Prefix string
}

func (s RedisNonceStore) Use(ctx context.Context, nonce string, ttl time.Duration) error {
	if s.Client == nil {
		return errors.New("redis nonce store client is nil")
	}

	prefix := s.Prefix
	if prefix == "" {
		prefix = "ojos:internal-auth:nonce:"
	}

	ok, err := s.Client.SetNX(ctx, prefix+nonce, "1", ttl).Result()
	if err != nil {
		return err
	}
	if !ok {
		return ErrReplay
	}

	return nil
}

type Verifier struct {
	keys  *KeyManager
	nonce NonceStore
	cfg   Config
}

func NewVerifier(keys *KeyManager, nonce NonceStore, cfg Config) *Verifier {
	return &Verifier{
		keys:  keys,
		nonce: nonce,
		cfg:   cfg.normalized(),
	}
}

func (v *Verifier) VerifyRequest(ctx context.Context, req *http.Request) error {
	keyID := strings.TrimSpace(req.Header.Get(HeaderKeyID))
	timestampText := strings.TrimSpace(req.Header.Get(HeaderTimestamp))
	nonce := strings.TrimSpace(req.Header.Get(HeaderNonce))
	bodyHash := strings.TrimSpace(req.Header.Get(HeaderBodyHash))
	signatureHeader := strings.TrimSpace(req.Header.Get(HeaderSignature))

	if keyID == "" || timestampText == "" || nonce == "" || bodyHash == "" || signatureHeader == "" {
		return ErrMissingInternalAuth
	}

	timestamp, err := strconv.ParseInt(timestampText, 10, 64)
	if err != nil || timestamp <= 0 {
		return ErrInvalidTimestamp
	}

	now := time.Now().UTC()
	requestTime := time.Unix(timestamp, 0).UTC()
	if requestTime.Before(now.Add(-v.cfg.TimestampSkew)) || requestTime.After(now.Add(v.cfg.TimestampSkew)) {
		return ErrTimestampSkew
	}

	body, err := readAndRestoreBody(req)
	if err != nil {
		return err
	}

	actualBodyHash := sha256Hex(body)
	if !hmac.Equal([]byte(strings.ToLower(bodyHash)), []byte(actualBodyHash)) {
		return ErrInvalidBodyHash
	}

	key, err := v.keys.VerifyKey(ctx, keyID)
	if err != nil {
		return err
	}

	canonical := canonicalString(req, timestampText, nonce, bodyHash)
	expected := signatureVersion + "=" + signCanonical(key.Secret, canonical)

	if !hmac.Equal([]byte(signatureHeader), []byte(expected)) {
		return ErrInvalidSignature
	}

	if v.nonce != nil {
		if err := v.nonce.Use(ctx, nonce, v.cfg.NonceTTL); err != nil {
			return err
		}
	}

	return nil
}

func canonicalString(req *http.Request, timestamp string, nonce string, bodyHash string) string {
	method := strings.ToUpper(strings.TrimSpace(req.Method))
	pathWithQuery := req.URL.RequestURI()

	lines := []string{
		signatureVersion,
		method,
		pathWithQuery,
		timestamp,
		nonce,
		strings.ToLower(strings.TrimSpace(bodyHash)),
		strings.TrimSpace(req.Header.Get(HeaderAuthVerified)),
		strings.TrimSpace(req.Header.Get(HeaderUserID)),
		strings.TrimSpace(req.Header.Get(HeaderUsername)),
		strings.TrimSpace(req.Header.Get(HeaderRoles)),
	}

	return strings.Join(lines, "\n")
}

func signCanonical(secret []byte, canonical string) string {
	mac := hmac.New(sha256.New, secret)
	mac.Write([]byte(canonical))
	sum := mac.Sum(nil)
	return base64.RawURLEncoding.EncodeToString(sum)
}

func readAndRestoreBody(req *http.Request) ([]byte, error) {
	if req.Body == nil {
		return nil, nil
	}

	body, err := io.ReadAll(req.Body)
	if err != nil {
		return nil, err
	}

	req.Body = io.NopCloser(bytes.NewReader(body))
	req.ContentLength = int64(len(body))
	return body, nil
}

func sha256Hex(body []byte) string {
	sum := sha256.Sum256(body)
	return hex.EncodeToString(sum[:])
}

func ClearInternalAuthHeaders(header http.Header) {
	header.Del(HeaderKeyID)
	header.Del(HeaderTimestamp)
	header.Del(HeaderNonce)
	header.Del(HeaderBodyHash)
	header.Del(HeaderSignature)
}

func ClearTrustedAuthHeaders(header http.Header) {
	header.Del(HeaderAuthVerified)
	header.Del(HeaderUserID)
	header.Del(HeaderUsername)
	header.Del(HeaderRoles)
}
