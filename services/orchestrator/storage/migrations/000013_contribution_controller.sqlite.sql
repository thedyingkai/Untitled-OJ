CREATE TABLE orchestrator_contribution_revisions (
    revision_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    release_digest TEXT NOT NULL,
    contract_digest TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    previous_revision_id TEXT,
    status TEXT NOT NULL CHECK (status IN ('STAGED', 'ACTIVE', 'RETIRED', 'ABORTED')),
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (scope_id, service_id, generation),
    UNIQUE (revision_id, scope_id, service_id),
    UNIQUE (revision_id, scope_id, service_id, generation),
    FOREIGN KEY (previous_revision_id)
        REFERENCES orchestrator_contribution_revisions(revision_id),
    CHECK (json_extract(payload, '$.revision_id') = revision_id),
    CHECK (json_extract(payload, '$.scope_id') = scope_id),
    CHECK (json_extract(payload, '$.deployment_id') = deployment_id),
    CHECK (json_extract(payload, '$.service_id') = service_id),
    CHECK (json_extract(payload, '$.release_digest') = release_digest),
    CHECK (json_extract(payload, '$.contract_digest') = contract_digest),
    CHECK (json_extract(payload, '$.generation') = generation),
    CHECK (json_extract(payload, '$.status') = status)
);
CREATE INDEX idx_orchestrator_contribution_revisions_scope
    ON orchestrator_contribution_revisions(scope_id, service_id, generation);
CREATE INDEX idx_orchestrator_contribution_revisions_deployment
    ON orchestrator_contribution_revisions(deployment_id, generation);

CREATE TRIGGER orchestrator_contribution_revision_immutable
BEFORE UPDATE ON orchestrator_contribution_revisions
WHEN OLD.revision_id <> NEW.revision_id
  OR OLD.scope_id <> NEW.scope_id
  OR OLD.deployment_id <> NEW.deployment_id
  OR OLD.service_id <> NEW.service_id
  OR OLD.release_digest <> NEW.release_digest
  OR OLD.contract_digest <> NEW.contract_digest
  OR OLD.generation <> NEW.generation
  OR OLD.previous_revision_id IS NOT NEW.previous_revision_id
  OR json_remove(OLD.payload, '$.status') <> json_remove(NEW.payload, '$.status')
BEGIN
    SELECT RAISE(ABORT, 'contribution revision immutable content cannot change');
END;

CREATE TABLE orchestrator_contribution_heads (
    scope_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    active_revision_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    etag TEXT NOT NULL UNIQUE,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (scope_id, service_id),
    FOREIGN KEY (active_revision_id, scope_id, service_id)
        REFERENCES orchestrator_contribution_revisions(
            revision_id, scope_id, service_id
        ),
    CHECK (json_extract(payload, '$.scope_id') = scope_id),
    CHECK (json_extract(payload, '$.service_id') = service_id),
    CHECK (json_extract(payload, '$.active_revision_id') = active_revision_id),
    CHECK (json_extract(payload, '$.generation') = generation),
    CHECK (json_extract(payload, '$.etag') = etag)
);
CREATE INDEX idx_orchestrator_contribution_heads_revision
    ON orchestrator_contribution_heads(active_revision_id);

CREATE TABLE orchestrator_contribution_activations (
    activation_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    candidate_revision_id TEXT NOT NULL UNIQUE,
    previous_revision_id TEXT,
    expected_head_etag TEXT,
    state TEXT NOT NULL CHECK (state IN (
        'PREPARING', 'COMMITTING', 'COMPENSATING',
        'SUCCEEDED', 'ABORTED', 'NEEDS_ATTENTION'
    )),
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (candidate_revision_id)
        REFERENCES orchestrator_contribution_revisions(revision_id),
    FOREIGN KEY (previous_revision_id)
        REFERENCES orchestrator_contribution_revisions(revision_id),
    CHECK (json_extract(payload, '$.activation_id') = activation_id),
    CHECK (json_extract(payload, '$.scope_id') = scope_id),
    CHECK (json_extract(payload, '$.service_id') = service_id),
    CHECK (json_extract(payload, '$.candidate_revision_id') = candidate_revision_id),
    CHECK (json_extract(payload, '$.state') = state)
);
CREATE INDEX idx_orchestrator_contribution_activations_state
    ON orchestrator_contribution_activations(state, activation_id);

CREATE TRIGGER orchestrator_contribution_activation_identity_immutable
BEFORE UPDATE ON orchestrator_contribution_activations
WHEN OLD.activation_id <> NEW.activation_id
  OR OLD.scope_id <> NEW.scope_id
  OR OLD.service_id <> NEW.service_id
  OR OLD.candidate_revision_id <> NEW.candidate_revision_id
  OR OLD.previous_revision_id IS NOT NEW.previous_revision_id
  OR OLD.expected_head_etag IS NOT NEW.expected_head_etag
  OR json_remove(json_remove(OLD.payload, '$.state'), '$.termination_intent') <>
     json_remove(json_remove(NEW.payload, '$.state'), '$.termination_intent')
BEGIN
    SELECT RAISE(ABORT, 'contribution activation identity cannot change');
END;

CREATE TABLE orchestrator_contribution_projection_receipts (
    activation_id TEXT NOT NULL,
    target TEXT NOT NULL CHECK (target IN (
        'API_REGISTRY', 'AUTH', 'GATEWAY', 'USER_SHELL', 'ADMIN_SHELL'
    )),
    candidate_revision_id TEXT NOT NULL,
    previous_revision_id TEXT,
    candidate_generation INTEGER NOT NULL CHECK (candidate_generation > 0),
    observed_generation INTEGER CHECK (observed_generation IS NULL OR observed_generation > 0),
    state TEXT NOT NULL CHECK (state IN (
        'PENDING', 'STAGED', 'ACTIVE', 'RESTORED', 'FAILED', 'UNKNOWN'
    )),
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (activation_id, target),
    FOREIGN KEY (activation_id)
        REFERENCES orchestrator_contribution_activations(activation_id),
    FOREIGN KEY (candidate_revision_id)
        REFERENCES orchestrator_contribution_revisions(revision_id),
    FOREIGN KEY (previous_revision_id)
        REFERENCES orchestrator_contribution_revisions(revision_id),
    CHECK (json_extract(payload, '$.activation_id') = activation_id),
    CHECK (json_extract(payload, '$.target') = target),
    CHECK (json_extract(payload, '$.candidate_revision_id') = candidate_revision_id),
    CHECK (json_extract(payload, '$.candidate_generation') = candidate_generation),
    CHECK (json_extract(payload, '$.state') = state)
);
CREATE INDEX idx_orchestrator_contribution_receipts_state
    ON orchestrator_contribution_projection_receipts(state, activation_id, target);

CREATE TRIGGER orchestrator_contribution_receipt_identity_immutable
BEFORE UPDATE ON orchestrator_contribution_projection_receipts
WHEN OLD.activation_id <> NEW.activation_id
  OR OLD.target <> NEW.target
  OR OLD.candidate_revision_id <> NEW.candidate_revision_id
  OR OLD.previous_revision_id IS NOT NEW.previous_revision_id
  OR OLD.candidate_generation <> NEW.candidate_generation
  OR json_remove(
       json_remove(
         json_remove(
           json_remove(
             json_remove(OLD.payload, '$.state'),
             '$.observed_generation'
           ),
           '$.staged_digest'
         ),
         '$.active_digest'
       ),
       '$.last_error'
     ) <>
     json_remove(
       json_remove(
         json_remove(
           json_remove(
             json_remove(NEW.payload, '$.state'),
             '$.observed_generation'
           ),
           '$.staged_digest'
         ),
         '$.active_digest'
       ),
       '$.last_error'
     )
BEGIN
    SELECT RAISE(ABORT, 'projection receipt identity cannot change');
END;

CREATE TABLE orchestrator_permission_assignments_v1 (
    assignment_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    permission_key TEXT NOT NULL,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('USER', 'ROLE', 'SERVICE')),
    subject_id TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (scope_id, permission_key, subject_kind, subject_id),
    CHECK (json_extract(payload, '$.assignment_id') = assignment_id),
    CHECK (json_extract(payload, '$.scope_id') = scope_id),
    CHECK (json_extract(payload, '$.permission_key') = permission_key),
    CHECK (json_extract(payload, '$.subject_kind') = subject_kind),
    CHECK (json_extract(payload, '$.subject_id') = subject_id)
);
CREATE INDEX idx_orchestrator_permission_assignments_scope
    ON orchestrator_permission_assignments_v1(scope_id, permission_key, subject_kind, subject_id);

CREATE TRIGGER orchestrator_permission_assignment_immutable
BEFORE UPDATE ON orchestrator_permission_assignments_v1
BEGIN
    SELECT RAISE(ABORT, 'permission assignments are immutable; delete and recreate explicitly');
END;
