package contest

import (
	"context"
	"sort"
	"sync"
	"time"
)

type MemoryRepository struct {
	mu     sync.RWMutex
	nextID int64
	items  map[int64]Contest
}

func NewMemoryRepository() *MemoryRepository {
	return &MemoryRepository{nextID: 1, items: make(map[int64]Contest)}
}

func (repository *MemoryRepository) Ping(context.Context) error { return nil }

func (repository *MemoryRepository) List(context.Context) ([]Contest, error) {
	repository.mu.RLock()
	defer repository.mu.RUnlock()
	items := make([]Contest, 0, len(repository.items))
	for _, item := range repository.items {
		items = append(items, item)
	}
	sort.Slice(items, func(i, j int) bool {
		if items[i].StartsAt.Equal(items[j].StartsAt) {
			return items[i].ID < items[j].ID
		}
		return items[i].StartsAt.Before(items[j].StartsAt)
	})
	return items, nil
}

func (repository *MemoryRepository) Get(_ context.Context, id int64) (Contest, error) {
	repository.mu.RLock()
	defer repository.mu.RUnlock()
	item, ok := repository.items[id]
	if !ok {
		return Contest{}, ErrNotFound
	}
	return item, nil
}

func (repository *MemoryRepository) Create(_ context.Context, input CreateInput) (Contest, error) {
	if err := input.NormalizeAndValidate(); err != nil {
		return Contest{}, err
	}
	repository.mu.Lock()
	defer repository.mu.Unlock()
	for _, current := range repository.items {
		if current.Slug == input.Slug {
			return Contest{}, ErrConflict
		}
	}
	now := time.Now().UTC()
	item := Contest{
		ID: repository.nextID, Slug: input.Slug, Title: input.Title,
		Description: input.Description, StartsAt: input.StartsAt.UTC(), EndsAt: input.EndsAt.UTC(),
		Version: 1, CreatedAt: now, UpdatedAt: now,
	}
	repository.nextID++
	repository.items[item.ID] = item
	return item, nil
}

func (repository *MemoryRepository) Update(_ context.Context, id int64, input UpdateInput) (Contest, error) {
	if err := input.NormalizeAndValidate(); err != nil {
		return Contest{}, err
	}
	repository.mu.Lock()
	defer repository.mu.Unlock()
	item, ok := repository.items[id]
	if !ok {
		return Contest{}, ErrNotFound
	}
	if item.Version != input.Version {
		return Contest{}, ErrConflict
	}
	item.Title = input.Title
	item.Description = input.Description
	item.StartsAt = input.StartsAt.UTC()
	item.EndsAt = input.EndsAt.UTC()
	item.Version++
	item.UpdatedAt = time.Now().UTC()
	repository.items[id] = item
	return item, nil
}

func (repository *MemoryRepository) Delete(_ context.Context, id int64) error {
	repository.mu.Lock()
	defer repository.mu.Unlock()
	if _, ok := repository.items[id]; !ok {
		return ErrNotFound
	}
	delete(repository.items, id)
	return nil
}
