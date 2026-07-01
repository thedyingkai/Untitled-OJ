package store

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"

	"ojos-user-service/internal/types"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

type ProfilePatch struct {
	DisplayName  string
	Bio          string
	AvatarObject string
	Preferences  map[string]string
}

type ProfileStore struct {
	root string
	db   *pgxpool.Pool
	now  func() time.Time
	mu   sync.Mutex
}

var safeUserID = regexp.MustCompile(`^[A-Za-z0-9_.-]{1,128}$`)

func NewProfileStore(root string, db *pgxpool.Pool) (*ProfileStore, error) {
	root = strings.TrimSpace(root)
	if root == "" {
		root = "/data/ojos/users"
	}
	if db == nil {
		if err := os.MkdirAll(root, 0o755); err != nil {
			return nil, err
		}
	}
	return &ProfileStore{
		root: root,
		db:   db,
		now:  time.Now,
	}, nil
}

func NewFileProfileStore(root string) (*ProfileStore, error) {
	return NewProfileStore(root, nil)
}

func NewPostgresProfileStore(db *pgxpool.Pool) (*ProfileStore, error) {
	if db == nil {
		return nil, errors.New("postgres pool is required")
	}
	return NewProfileStore("", db)
}

func (s *ProfileStore) GetOrCreateCtx(ctx context.Context, userID, displayName string) (types.ProfileResp, error) {
	if s.db != nil {
		return s.getOrCreatePostgres(ctx, userID, displayName)
	}
	return s.GetOrCreate(userID, displayName)
}

func (s *ProfileStore) UpdateCtx(ctx context.Context, userID string, patch ProfilePatch) (types.ProfileResp, error) {
	if s.db != nil {
		return s.updatePostgres(ctx, userID, patch)
	}
	return s.Update(userID, patch)
}

func (s *ProfileStore) getOrCreatePostgres(ctx context.Context, userID, displayName string) (types.ProfileResp, error) {
	if err := validateUserID(userID); err != nil {
		return types.ProfileResp{}, err
	}
	if strings.TrimSpace(displayName) == "" {
		displayName = userID
	}
	var profile types.ProfileResp
	err := s.db.QueryRow(ctx, `
INSERT INTO user_profiles(user_id, display_name, preferences, created_at, updated_at)
VALUES($1, $2, '{"theme":"system"}'::jsonb, NOW(), NOW())
ON CONFLICT(user_id) DO UPDATE
SET user_id = EXCLUDED.user_id
RETURNING
    user_id,
    display_name,
    bio,
    avatar_object,
    preferences,
    solved_problems,
    submissions,
    accepted,
    created_at::TEXT,
    updated_at::TEXT
`, userID, strings.TrimSpace(displayName)).Scan(
		&profile.UserId,
		&profile.DisplayName,
		&profile.Bio,
		&profile.AvatarObject,
		&profile.Preferences,
		&profile.Stats.SolvedProblems,
		&profile.Stats.Submissions,
		&profile.Stats.Accepted,
		&profile.CreatedAt,
		&profile.UpdatedAt,
	)
	if err != nil {
		return types.ProfileResp{}, err
	}
	if profile.Preferences == nil {
		profile.Preferences = map[string]string{}
	}
	return profile, nil
}

func (s *ProfileStore) updatePostgres(ctx context.Context, userID string, patch ProfilePatch) (types.ProfileResp, error) {
	if err := validateUserID(userID); err != nil {
		return types.ProfileResp{}, err
	}
	if patch.Preferences == nil {
		patch.Preferences = map[string]string{}
	}
	preferences, err := json.Marshal(patch.Preferences)
	if err != nil {
		return types.ProfileResp{}, err
	}
	var profile types.ProfileResp
	err = s.db.QueryRow(ctx, `
INSERT INTO user_profiles(user_id, display_name, preferences, created_at, updated_at)
VALUES($1, $1, '{"theme":"system"}'::jsonb, NOW(), NOW())
ON CONFLICT(user_id) DO UPDATE
SET
    display_name = CASE WHEN $2 <> '' THEN $2 ELSE user_profiles.display_name END,
    bio = CASE WHEN $3 <> '' THEN $3 ELSE user_profiles.bio END,
    avatar_object = CASE WHEN $4 <> '' THEN $4 ELSE user_profiles.avatar_object END,
    preferences = CASE WHEN $5::jsonb <> '{}'::jsonb THEN $5::jsonb ELSE user_profiles.preferences END,
    updated_at = NOW()
RETURNING
    user_id,
    display_name,
    bio,
    avatar_object,
    preferences,
    solved_problems,
    submissions,
    accepted,
    created_at::TEXT,
    updated_at::TEXT
`, userID, strings.TrimSpace(patch.DisplayName), strings.TrimSpace(patch.Bio), strings.TrimSpace(patch.AvatarObject), string(preferences)).Scan(
		&profile.UserId,
		&profile.DisplayName,
		&profile.Bio,
		&profile.AvatarObject,
		&profile.Preferences,
		&profile.Stats.SolvedProblems,
		&profile.Stats.Submissions,
		&profile.Stats.Accepted,
		&profile.CreatedAt,
		&profile.UpdatedAt,
	)
	if errors.Is(err, pgx.ErrNoRows) {
		return types.ProfileResp{}, os.ErrNotExist
	}
	if err != nil {
		return types.ProfileResp{}, err
	}
	if profile.Preferences == nil {
		profile.Preferences = map[string]string{}
	}
	return profile, nil
}

func (s *ProfileStore) GetOrCreate(userID, displayName string) (types.ProfileResp, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := validateUserID(userID); err != nil {
		return types.ProfileResp{}, err
	}
	profile, err := s.read(userID)
	if err == nil {
		return profile, nil
	}
	if !errors.Is(err, os.ErrNotExist) {
		return types.ProfileResp{}, err
	}
	now := s.now().UTC().Format(time.RFC3339)
	if strings.TrimSpace(displayName) == "" {
		displayName = userID
	}
	profile = types.ProfileResp{
		UserId:      userID,
		DisplayName: strings.TrimSpace(displayName),
		Preferences: map[string]string{
			"theme": "system",
		},
		Stats:     types.UserStats{},
		CreatedAt: now,
		UpdatedAt: now,
	}
	return profile, s.write(profile)
}

func (s *ProfileStore) Update(userID string, patch ProfilePatch) (types.ProfileResp, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := validateUserID(userID); err != nil {
		return types.ProfileResp{}, err
	}
	profile, err := s.read(userID)
	if errors.Is(err, os.ErrNotExist) {
		now := s.now().UTC().Format(time.RFC3339)
		profile = types.ProfileResp{
			UserId:      userID,
			DisplayName: userID,
			Preferences: map[string]string{
				"theme": "system",
			},
			CreatedAt: now,
			UpdatedAt: now,
		}
	} else if err != nil {
		return types.ProfileResp{}, err
	}

	if value := strings.TrimSpace(patch.DisplayName); value != "" {
		profile.DisplayName = value
	}
	if patch.Bio != "" {
		profile.Bio = strings.TrimSpace(patch.Bio)
	}
	if patch.AvatarObject != "" {
		profile.AvatarObject = strings.TrimSpace(patch.AvatarObject)
	}
	if patch.Preferences != nil {
		profile.Preferences = patch.Preferences
	}
	if profile.Preferences == nil {
		profile.Preferences = map[string]string{}
	}
	profile.UpdatedAt = s.now().UTC().Format(time.RFC3339)
	return profile, s.write(profile)
}

func (s *ProfileStore) read(userID string) (types.ProfileResp, error) {
	var profile types.ProfileResp
	data, err := os.ReadFile(s.path(userID))
	if err != nil {
		return profile, err
	}
	if err := json.Unmarshal(data, &profile); err != nil {
		return profile, err
	}
	if profile.Preferences == nil {
		profile.Preferences = map[string]string{}
	}
	return profile, nil
}

func (s *ProfileStore) write(profile types.ProfileResp) error {
	if err := os.MkdirAll(s.root, 0o755); err != nil {
		return err
	}
	data, err := json.MarshalIndent(profile, "", "  ")
	if err != nil {
		return err
	}
	tmp := filepath.Join(s.root, "."+stableName(profile.UserId)+".tmp")
	if err := os.WriteFile(tmp, data, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, s.path(profile.UserId))
}

func (s *ProfileStore) path(userID string) string {
	return filepath.Join(s.root, stableName(userID)+".json")
}

func validateUserID(userID string) error {
	if !safeUserID.MatchString(userID) {
		return fmt.Errorf("invalid user id")
	}
	return nil
}

func stableName(value string) string {
	sum := sha256.Sum256([]byte(value))
	return hex.EncodeToString(sum[:])
}
