package contest

import (
	"errors"
	"regexp"
	"strings"
	"time"
)

var slugPattern = regexp.MustCompile(`^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$`)

var (
	ErrInvalid  = errors.New("invalid contest")
	ErrNotFound = errors.New("contest not found")
	ErrConflict = errors.New("contest conflict")
)

type Contest struct {
	ID          int64     `json:"id"`
	Slug        string    `json:"slug"`
	Title       string    `json:"title"`
	Description string    `json:"description"`
	StartsAt    time.Time `json:"startsAt"`
	EndsAt      time.Time `json:"endsAt"`
	Version     int64     `json:"version"`
	CreatedAt   time.Time `json:"createdAt"`
	UpdatedAt   time.Time `json:"updatedAt"`
}

type CreateInput struct {
	Slug        string    `json:"slug"`
	Title       string    `json:"title"`
	Description string    `json:"description"`
	StartsAt    time.Time `json:"startsAt"`
	EndsAt      time.Time `json:"endsAt"`
}

type UpdateInput struct {
	Title       string    `json:"title"`
	Description string    `json:"description"`
	StartsAt    time.Time `json:"startsAt"`
	EndsAt      time.Time `json:"endsAt"`
	Version     int64     `json:"version"`
}

func (input *CreateInput) NormalizeAndValidate() error {
	input.Slug = strings.TrimSpace(input.Slug)
	input.Title = strings.TrimSpace(input.Title)
	input.Description = strings.TrimSpace(input.Description)
	if !slugPattern.MatchString(input.Slug) || input.Title == "" || len(input.Title) > 200 || len(input.Description) > 10_000 {
		return ErrInvalid
	}
	if input.StartsAt.IsZero() || input.EndsAt.IsZero() || !input.EndsAt.After(input.StartsAt) {
		return ErrInvalid
	}
	return nil
}

func (input *UpdateInput) NormalizeAndValidate() error {
	input.Title = strings.TrimSpace(input.Title)
	input.Description = strings.TrimSpace(input.Description)
	if input.Title == "" || len(input.Title) > 200 || len(input.Description) > 10_000 || input.Version < 1 {
		return ErrInvalid
	}
	if input.StartsAt.IsZero() || input.EndsAt.IsZero() || !input.EndsAt.After(input.StartsAt) {
		return ErrInvalid
	}
	return nil
}

type CreatedV1 struct {
	ContestID int64     `json:"contestId"`
	Slug      string    `json:"slug"`
	Title     string    `json:"title"`
	StartsAt  time.Time `json:"startsAt"`
	EndsAt    time.Time `json:"endsAt"`
}
