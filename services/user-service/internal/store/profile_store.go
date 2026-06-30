package store

import (
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
)

type ProfilePatch struct {
	DisplayName  string
	Bio          string
	AvatarObject string
	Preferences  map[string]string
}

type ProfileStore struct {
	root string
	now  func() time.Time
	mu   sync.Mutex
}

var safeUserID = regexp.MustCompile(`^[A-Za-z0-9_.-]{1,128}$`)

func NewProfileStore(root string) (*ProfileStore, error) {
	root = strings.TrimSpace(root)
	if root == "" {
		root = "/data/ojos/users"
	}
	if err := os.MkdirAll(root, 0o755); err != nil {
		return nil, err
	}
	return &ProfileStore{
		root: root,
		now:  time.Now,
	}, nil
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
