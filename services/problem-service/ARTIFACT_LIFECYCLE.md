# Problem artifact lifecycle

Problem Service owns two different immutable object lifecycles:

- Authoring-file objects use keys of the form
  `problem-<id>-objects-sha256-<digest>`. Every upload has a durable intent.
  Replacing a `problem_files` row, deleting selected rows, or deleting a Problem
  writes the removed object identity back to the same ledger in the business
  transaction. The collector may delete it only after its retention window and
  a final database-reference check.
- Package ZIPs use `package-sha256-<digest>.zip`. Failed uploads without a
  committed revision remain collectible through their upload intent. Once a
  package revision commits, its intent is resolved and the ZIP is retained.

Committed package ZIP retention is deliberate. Judge submissions copy and pin
the package revision and may still need it after the Problem is updated or
deleted. Problem Service must not enqueue committed package revisions for
deletion until a future cross-service retention protocol can prove that no
submission holds them.

Online create and update paths must resolve an exact `PENDING` upload intent in
the same transaction that persists the matching reference. Missing intents fail
closed. Only the explicitly named legacy/backfill resolver may tolerate a row
that predates the ledger.
