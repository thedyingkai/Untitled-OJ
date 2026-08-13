CREATE TABLE IF NOT EXISTS orchestrator_contribution_revisions (
    revision_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    release_digest TEXT NOT NULL,
    contract_digest TEXT NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    previous_revision_id TEXT REFERENCES orchestrator_contribution_revisions(revision_id),
    status TEXT NOT NULL CHECK (status IN ('STAGED', 'ACTIVE', 'RETIRED', 'ABORTED')),
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (scope_id, service_id, generation),
    UNIQUE (revision_id, scope_id, service_id),
    UNIQUE (revision_id, scope_id, service_id, generation),
    CHECK (payload->>'revision_id' = revision_id),
    CHECK (payload->>'scope_id' = scope_id),
    CHECK (payload->>'deployment_id' = deployment_id),
    CHECK (payload->>'service_id' = service_id),
    CHECK (payload->>'release_digest' = release_digest),
    CHECK (payload->>'contract_digest' = contract_digest),
    CHECK ((payload->>'generation')::BIGINT = generation),
    CHECK (payload->>'status' = status)
);
CREATE INDEX IF NOT EXISTS idx_orchestrator_contribution_revisions_scope
    ON orchestrator_contribution_revisions(scope_id, service_id, generation);
CREATE INDEX IF NOT EXISTS idx_orchestrator_contribution_revisions_deployment
    ON orchestrator_contribution_revisions(deployment_id, generation);

CREATE OR REPLACE FUNCTION orchestrator_reject_contribution_revision_identity_change()
RETURNS trigger LANGUAGE plpgsql AS $contribution_revision_immutable$
BEGIN
    IF OLD.revision_id IS DISTINCT FROM NEW.revision_id
       OR OLD.scope_id IS DISTINCT FROM NEW.scope_id
       OR OLD.deployment_id IS DISTINCT FROM NEW.deployment_id
       OR OLD.service_id IS DISTINCT FROM NEW.service_id
       OR OLD.release_digest IS DISTINCT FROM NEW.release_digest
       OR OLD.contract_digest IS DISTINCT FROM NEW.contract_digest
       OR OLD.generation IS DISTINCT FROM NEW.generation
       OR OLD.previous_revision_id IS DISTINCT FROM NEW.previous_revision_id
       OR (OLD.payload - 'status') IS DISTINCT FROM (NEW.payload - 'status') THEN
        RAISE EXCEPTION 'contribution revision immutable content cannot change';
    END IF;
    RETURN NEW;
END;
$contribution_revision_immutable$;
DROP TRIGGER IF EXISTS orchestrator_contribution_revision_immutable
    ON orchestrator_contribution_revisions;
CREATE TRIGGER orchestrator_contribution_revision_immutable
BEFORE UPDATE ON orchestrator_contribution_revisions
FOR EACH ROW EXECUTE FUNCTION orchestrator_reject_contribution_revision_identity_change();

CREATE TABLE IF NOT EXISTS orchestrator_contribution_heads (
    scope_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    active_revision_id TEXT NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    etag TEXT NOT NULL UNIQUE,
    payload JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (scope_id, service_id),
    FOREIGN KEY (active_revision_id, scope_id, service_id)
        REFERENCES orchestrator_contribution_revisions(
            revision_id, scope_id, service_id
        ),
    CHECK (payload->>'scope_id' = scope_id),
    CHECK (payload->>'service_id' = service_id),
    CHECK (payload->>'active_revision_id' = active_revision_id),
    CHECK ((payload->>'generation')::BIGINT = generation),
    CHECK (payload->>'etag' = etag)
);
CREATE INDEX IF NOT EXISTS idx_orchestrator_contribution_heads_revision
    ON orchestrator_contribution_heads(active_revision_id);

CREATE TABLE IF NOT EXISTS orchestrator_contribution_activations (
    activation_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    candidate_revision_id TEXT NOT NULL UNIQUE
        REFERENCES orchestrator_contribution_revisions(revision_id),
    previous_revision_id TEXT REFERENCES orchestrator_contribution_revisions(revision_id),
    expected_head_etag TEXT,
    state TEXT NOT NULL CHECK (state IN (
        'PREPARING', 'COMMITTING', 'COMPENSATING',
        'SUCCEEDED', 'ABORTED', 'NEEDS_ATTENTION'
    )),
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (payload->>'activation_id' = activation_id),
    CHECK (payload->>'scope_id' = scope_id),
    CHECK (payload->>'service_id' = service_id),
    CHECK (payload->>'candidate_revision_id' = candidate_revision_id),
    CHECK (payload->>'state' = state)
);
CREATE INDEX IF NOT EXISTS idx_orchestrator_contribution_activations_state
    ON orchestrator_contribution_activations(state, activation_id);

CREATE OR REPLACE FUNCTION orchestrator_reject_contribution_activation_identity_change()
RETURNS trigger LANGUAGE plpgsql AS $contribution_activation_immutable$
BEGIN
    IF OLD.activation_id IS DISTINCT FROM NEW.activation_id
       OR OLD.scope_id IS DISTINCT FROM NEW.scope_id
       OR OLD.service_id IS DISTINCT FROM NEW.service_id
       OR OLD.candidate_revision_id IS DISTINCT FROM NEW.candidate_revision_id
       OR OLD.previous_revision_id IS DISTINCT FROM NEW.previous_revision_id
       OR OLD.expected_head_etag IS DISTINCT FROM NEW.expected_head_etag
       OR (OLD.payload - 'state' - 'termination_intent') IS DISTINCT FROM
          (NEW.payload - 'state' - 'termination_intent') THEN
        RAISE EXCEPTION 'contribution activation identity cannot change';
    END IF;
    RETURN NEW;
END;
$contribution_activation_immutable$;
DROP TRIGGER IF EXISTS orchestrator_contribution_activation_identity_immutable
    ON orchestrator_contribution_activations;
CREATE TRIGGER orchestrator_contribution_activation_identity_immutable
BEFORE UPDATE ON orchestrator_contribution_activations
FOR EACH ROW EXECUTE FUNCTION orchestrator_reject_contribution_activation_identity_change();

CREATE TABLE IF NOT EXISTS orchestrator_contribution_projection_receipts (
    activation_id TEXT NOT NULL
        REFERENCES orchestrator_contribution_activations(activation_id),
    target TEXT NOT NULL CHECK (target IN (
        'API_REGISTRY', 'AUTH', 'GATEWAY', 'USER_SHELL', 'ADMIN_SHELL'
    )),
    candidate_revision_id TEXT NOT NULL
        REFERENCES orchestrator_contribution_revisions(revision_id),
    previous_revision_id TEXT REFERENCES orchestrator_contribution_revisions(revision_id),
    candidate_generation BIGINT NOT NULL CHECK (candidate_generation > 0),
    observed_generation BIGINT CHECK (observed_generation IS NULL OR observed_generation > 0),
    state TEXT NOT NULL CHECK (state IN (
        'PENDING', 'STAGED', 'ACTIVE', 'RESTORED', 'FAILED', 'UNKNOWN'
    )),
    payload JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (activation_id, target),
    CHECK (payload->>'activation_id' = activation_id),
    CHECK (payload->>'target' = target),
    CHECK (payload->>'candidate_revision_id' = candidate_revision_id),
    CHECK ((payload->>'candidate_generation')::BIGINT = candidate_generation),
    CHECK (payload->>'state' = state)
);
CREATE INDEX IF NOT EXISTS idx_orchestrator_contribution_receipts_state
    ON orchestrator_contribution_projection_receipts(state, activation_id, target);

CREATE OR REPLACE FUNCTION orchestrator_reject_contribution_receipt_identity_change()
RETURNS trigger LANGUAGE plpgsql AS $contribution_receipt_immutable$
BEGIN
    IF OLD.activation_id IS DISTINCT FROM NEW.activation_id
       OR OLD.target IS DISTINCT FROM NEW.target
       OR OLD.candidate_revision_id IS DISTINCT FROM NEW.candidate_revision_id
       OR OLD.previous_revision_id IS DISTINCT FROM NEW.previous_revision_id
       OR OLD.candidate_generation IS DISTINCT FROM NEW.candidate_generation
       OR (OLD.payload - 'state' - 'observed_generation' - 'staged_digest' - 'active_digest' - 'last_error')
          IS DISTINCT FROM
          (NEW.payload - 'state' - 'observed_generation' - 'staged_digest' - 'active_digest' - 'last_error') THEN
        RAISE EXCEPTION 'projection receipt identity cannot change';
    END IF;
    RETURN NEW;
END;
$contribution_receipt_immutable$;
DROP TRIGGER IF EXISTS orchestrator_contribution_receipt_identity_immutable
    ON orchestrator_contribution_projection_receipts;
CREATE TRIGGER orchestrator_contribution_receipt_identity_immutable
BEFORE UPDATE ON orchestrator_contribution_projection_receipts
FOR EACH ROW EXECUTE FUNCTION orchestrator_reject_contribution_receipt_identity_change();

CREATE TABLE IF NOT EXISTS orchestrator_permission_assignments_v1 (
    assignment_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    permission_key TEXT NOT NULL,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('USER', 'ROLE', 'SERVICE')),
    subject_id TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (scope_id, permission_key, subject_kind, subject_id),
    CHECK (payload->>'assignment_id' = assignment_id),
    CHECK (payload->>'scope_id' = scope_id),
    CHECK (payload->>'permission_key' = permission_key),
    CHECK (payload->>'subject_kind' = subject_kind),
    CHECK (payload->>'subject_id' = subject_id)
);
CREATE INDEX IF NOT EXISTS idx_orchestrator_permission_assignments_scope
    ON orchestrator_permission_assignments_v1(
        scope_id, permission_key, subject_kind, subject_id
    );

CREATE OR REPLACE FUNCTION orchestrator_reject_permission_assignment_update()
RETURNS trigger LANGUAGE plpgsql AS $permission_assignment_immutable$
BEGIN
    RAISE EXCEPTION 'permission assignments are immutable; delete and recreate explicitly';
END;
$permission_assignment_immutable$;
DROP TRIGGER IF EXISTS orchestrator_permission_assignment_immutable
    ON orchestrator_permission_assignments_v1;
CREATE TRIGGER orchestrator_permission_assignment_immutable
BEFORE UPDATE ON orchestrator_permission_assignments_v1
FOR EACH ROW EXECUTE FUNCTION orchestrator_reject_permission_assignment_update();
