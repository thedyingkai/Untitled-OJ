CREATE TABLE IF NOT EXISTS auth_topology_projections (
    topology_id TEXT PRIMARY KEY,
    revision_id TEXT NOT NULL,
    content_sha256 TEXT NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
    operation_id TEXT NOT NULL,
    payload JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE IF NOT EXISTS auth_topology_binding_grants (
    binding_id TEXT PRIMARY KEY,
    topology_id TEXT NOT NULL REFERENCES auth_topology_projections(topology_id) ON DELETE CASCADE,
    consumer_deployment_id TEXT NOT NULL,
    requirement_name TEXT NOT NULL,
    consumer_service_id TEXT NOT NULL,
    consumer_node_id TEXT NOT NULL,
    credential_generation BIGINT NOT NULL CHECK (credential_generation > 0),
    api_id TEXT NOT NULL,
    permission_code TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (consumer_deployment_id, requirement_name)
);

CREATE INDEX IF NOT EXISTS idx_auth_topology_binding_grants_topology
    ON auth_topology_binding_grants(topology_id, binding_id);

CREATE INDEX IF NOT EXISTS idx_auth_topology_binding_grants_consumer
    ON auth_topology_binding_grants(consumer_deployment_id, credential_generation);
