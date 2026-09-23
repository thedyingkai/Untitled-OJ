package topologyprojection

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	shared "ojos-shared/topologyprojection"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

type Store struct {
	db *pgxpool.Pool
}

func NewStore(db *pgxpool.Pool) *Store {
	return &Store{db: db}
}

func (s *Store) Get(ctx context.Context, topologyID string) (*shared.Document, error) {
	if s == nil || s.db == nil {
		return nil, fmt.Errorf("Auth topology projection requires PostgreSQL")
	}
	var payload []byte
	err := s.db.QueryRow(ctx, `SELECT payload FROM auth_topology_projections WHERE topology_id = $1`, topologyID).Scan(&payload)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("load Auth topology projection: %w", err)
	}
	document, err := shared.DecodeDocument(payload)
	if err != nil {
		return nil, fmt.Errorf("decode Auth topology projection: %w", err)
	}
	return &document, nil
}

func (s *Store) Apply(ctx context.Context, request shared.Request) error {
	document := request.Document()
	if s == nil || s.db == nil {
		return fmt.Errorf("Auth topology projection requires PostgreSQL")
	}
	payload, err := json.Marshal(document)
	if err != nil {
		return fmt.Errorf("encode Auth topology projection: %w", err)
	}
	tx, err := s.db.BeginTx(ctx, pgx.TxOptions{IsoLevel: pgx.Serializable})
	if err != nil {
		return fmt.Errorf("begin Auth topology projection transaction: %w", err)
	}
	defer tx.Rollback(ctx) //nolint:errcheck
	var existingPayload []byte
	err = tx.QueryRow(ctx, `SELECT payload FROM auth_topology_projections WHERE topology_id = $1 FOR UPDATE`, request.TopologyID).
		Scan(&existingPayload)
	if err != nil && err != pgx.ErrNoRows {
		return fmt.Errorf("lock Auth topology projection: %w", err)
	}
	var current *shared.Document
	if err == nil {
		persisted, decodeErr := shared.DecodeDocument(existingPayload)
		if decodeErr != nil {
			return fmt.Errorf("decode locked Auth topology projection: %w", decodeErr)
		}
		current = &persisted
	}
	write, err := shared.PlanApply(current, request)
	if err != nil {
		return err
	}
	if !write {
		return nil
	}
	_, err = tx.Exec(ctx, `
		INSERT INTO auth_topology_projections(topology_id, revision_id, content_sha256, operation_id, payload)
		VALUES ($1, $2, $3, $4, $5::jsonb)
		ON CONFLICT(topology_id) DO UPDATE SET
			revision_id = excluded.revision_id,
			content_sha256 = excluded.content_sha256,
			operation_id = excluded.operation_id,
			payload = excluded.payload,
			updated_at = clock_timestamp()`,
		document.TopologyID, document.RevisionID, document.ContentSHA256, document.OperationID, payload)
	if err != nil {
		return fmt.Errorf("persist Auth topology projection: %w", err)
	}
	if _, err = tx.Exec(ctx, `DELETE FROM auth_topology_binding_grants WHERE topology_id = $1`, document.TopologyID); err != nil {
		return fmt.Errorf("replace Auth topology grants: %w", err)
	}
	for _, grant := range document.Grants {
		_, err = tx.Exec(ctx, `
			INSERT INTO auth_topology_binding_grants(
				binding_id, topology_id, consumer_deployment_id, requirement_name,
				consumer_service_id, consumer_node_id, credential_generation, api_id, permission_code
			) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)`,
			grant.BindingID, document.TopologyID, grant.ConsumerDeploymentID, grant.RequirementName,
			grant.ConsumerServiceID, grant.ConsumerNodeID, int64(grant.CredentialGeneration), grant.APIID, grant.Permission)
		if err != nil {
			return fmt.Errorf("persist Auth binding grant %s: %w", grant.BindingID, err)
		}
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("commit Auth topology projection: %w", err)
	}
	return nil
}

func (s *Store) Delete(ctx context.Context, topologyID string) error {
	if s == nil || s.db == nil {
		return fmt.Errorf("Auth topology projection requires PostgreSQL")
	}
	if _, err := s.db.Exec(ctx, `DELETE FROM auth_topology_projections WHERE topology_id = $1`, topologyID); err != nil {
		return fmt.Errorf("delete Auth topology projection: %w", err)
	}
	return nil
}

// AuthorizeWorkload checks the exact identity and generation projected by an
// applied ApiBinding. No service-wide wildcard or legacy allowed_apis value is
// consulted: unlinking the topology or rotating the binding takes effect on
// the next request even if the caller's JWT has not expired yet.
func (s *Store) AuthorizeWorkload(
	ctx context.Context,
	deploymentID string,
	serviceID string,
	nodeID string,
	credentialGeneration uint64,
	apiID string,
	permissionCode string,
) (bool, error) {
	deploymentID = strings.TrimSpace(deploymentID)
	serviceID = strings.TrimSpace(serviceID)
	nodeID = strings.TrimSpace(nodeID)
	apiID = strings.TrimSpace(apiID)
	permissionCode = strings.TrimSpace(permissionCode)
	if deploymentID == "" || serviceID == "" || nodeID == "" || credentialGeneration == 0 || apiID == "" || permissionCode == "" {
		return false, nil
	}
	if s == nil || s.db == nil {
		return false, fmt.Errorf("Auth topology projection requires PostgreSQL")
	}
	var allowed bool
	err := s.db.QueryRow(ctx, `
		SELECT EXISTS (
			SELECT 1
			FROM auth_topology_binding_grants
			WHERE consumer_deployment_id = $1
			  AND consumer_service_id = $2
			  AND consumer_node_id = $3
			  AND credential_generation = $4
			  AND api_id = $5
			  AND permission_code = $6
		)`, deploymentID, serviceID, nodeID, int64(credentialGeneration), apiID, permissionCode).Scan(&allowed)
	if err != nil {
		return false, fmt.Errorf("authorize workload from Auth topology projection: %w", err)
	}
	return allowed, nil
}
