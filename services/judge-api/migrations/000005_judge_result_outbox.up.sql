ALTER TABLE judge_tasks
    ADD COLUMN IF NOT EXISTS result_payload_sha256 TEXT NOT NULL DEFAULT '';

CREATE TABLE IF NOT EXISTS judge_task_report_receipts (
    task_id TEXT NOT NULL,
    lease_version INT NOT NULL,
    worker_id TEXT NOT NULL,
    report_kind TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    response_status TEXT NOT NULL,
    event_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(task_id, lease_version),
    CONSTRAINT chk_judge_task_report_receipt_lease CHECK (lease_version > 0),
    CONSTRAINT chk_judge_task_report_receipt_kind CHECK (report_kind IN ('result', 'fail')),
    CONSTRAINT chk_judge_task_report_receipt_digest CHECK (payload_sha256 ~ '^[a-f0-9]{64}$')
);

CREATE TABLE IF NOT EXISTS judge_result_outbox (
    sequence BIGSERIAL PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    task_id TEXT NOT NULL,
    lease_version INT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    payload JSONB NOT NULL,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    attempt_count INT NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_until TIMESTAMPTZ,
    published_at TIMESTAMPTZ,
    last_error TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_judge_result_outbox_lease_version CHECK (lease_version > 0),
    CONSTRAINT chk_judge_result_outbox_digest CHECK (payload_sha256 ~ '^[a-f0-9]{64}$'),
    CONSTRAINT fk_judge_result_outbox_receipt
        FOREIGN KEY(task_id, lease_version)
        REFERENCES judge_task_report_receipts(task_id, lease_version),
    UNIQUE(task_id, lease_version)
);

CREATE INDEX IF NOT EXISTS idx_judge_result_outbox_pending
    ON judge_result_outbox(available_at, sequence)
    WHERE published_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_judge_result_outbox_lease
    ON judge_result_outbox(lease_until)
    WHERE published_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_judge_task_report_receipts_created
    ON judge_task_report_receipts(created_at);
