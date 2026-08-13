package contest

import (
	"context"
	"errors"
	"testing"
	"time"

	generated "ojos.local/gen/contest_service"
)

func validCreate() CreateInput {
	start := time.Date(2027, 1, 1, 10, 0, 0, 0, time.UTC)
	return CreateInput{Slug: "winter-cup", Title: "Winter Cup", StartsAt: start, EndsAt: start.Add(2 * time.Hour)}
}

func TestMemoryRepositoryLifecycleAndOptimisticConcurrency(t *testing.T) {
	repository := NewMemoryRepository()
	ctx := context.Background()
	created, err := repository.Create(ctx, validCreate())
	if err != nil || created.ID != 1 || created.Version != 1 {
		t.Fatalf("create = %#v, %v", created, err)
	}
	if _, err := repository.Create(ctx, validCreate()); !errors.Is(err, ErrConflict) {
		t.Fatalf("duplicate create error = %v", err)
	}
	update := UpdateInput{
		Title: "Winter Cup Finals", StartsAt: created.StartsAt,
		EndsAt: created.EndsAt, Version: created.Version,
	}
	updated, err := repository.Update(ctx, created.ID, update)
	if err != nil || updated.Version != 2 || updated.Title != update.Title {
		t.Fatalf("update = %#v, %v", updated, err)
	}
	if _, err := repository.Update(ctx, created.ID, update); !errors.Is(err, ErrConflict) {
		t.Fatalf("stale update error = %v", err)
	}
	items, err := repository.List(ctx)
	if err != nil || len(items) != 1 || items[0].ID != created.ID {
		t.Fatalf("list = %#v, %v", items, err)
	}
	if err := repository.Delete(ctx, created.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := repository.Get(ctx, created.ID); !errors.Is(err, ErrNotFound) {
		t.Fatalf("get deleted error = %v", err)
	}
}

func TestCreateValidationRejectsBadWindowAndSlug(t *testing.T) {
	input := validCreate()
	input.Slug = "../escape"
	if err := input.NormalizeAndValidate(); !errors.Is(err, ErrInvalid) {
		t.Fatalf("bad slug error = %v", err)
	}
	input = validCreate()
	input.EndsAt = input.StartsAt
	if err := input.NormalizeAndValidate(); !errors.Is(err, ErrInvalid) {
		t.Fatalf("bad window error = %v", err)
	}
}

func TestContestCreatedCodecRejectsWrongSchema(t *testing.T) {
	if ContestCreatedEventType != generated.ContestServiceContestCreatedV1Type || generated.ContestServiceContestCreatedV1SchemaDigest == "" {
		t.Fatal("runtime event identity drifted from generated contract")
	}
	input := validCreate()
	payload := CreatedV1{ContestID: 7, Slug: input.Slug, Title: input.Title, StartsAt: input.StartsAt, EndsAt: input.EndsAt}
	event, err := contestCreatedCodec.NewEvent(context.Background(), "urn:test", "contest/7", 1, payload)
	if err != nil {
		t.Fatal(err)
	}
	envelope := event.Envelope()
	if decoded, err := contestCreatedCodec.Decode(envelope); err != nil || decoded.ContestID != 7 {
		t.Fatalf("decode = %#v, %v", decoded, err)
	}
	envelope.DataSchema = "urn:wrong"
	if _, err := contestCreatedCodec.Decode(envelope); err == nil {
		t.Fatal("wrong schema accepted")
	}
}
