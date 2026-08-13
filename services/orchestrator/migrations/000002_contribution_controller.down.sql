DROP TRIGGER IF EXISTS orchestrator_permission_assignment_immutable
    ON orchestrator_permission_assignments_v1;
DROP FUNCTION IF EXISTS orchestrator_reject_permission_assignment_update();
DROP TABLE IF EXISTS orchestrator_permission_assignments_v1;

DROP TRIGGER IF EXISTS orchestrator_contribution_receipt_identity_immutable
    ON orchestrator_contribution_projection_receipts;
DROP FUNCTION IF EXISTS orchestrator_reject_contribution_receipt_identity_change();
DROP TABLE IF EXISTS orchestrator_contribution_projection_receipts;

DROP TRIGGER IF EXISTS orchestrator_contribution_activation_identity_immutable
    ON orchestrator_contribution_activations;
DROP FUNCTION IF EXISTS orchestrator_reject_contribution_activation_identity_change();
DROP TABLE IF EXISTS orchestrator_contribution_activations;

DROP TABLE IF EXISTS orchestrator_contribution_heads;

DROP TRIGGER IF EXISTS orchestrator_contribution_revision_immutable
    ON orchestrator_contribution_revisions;
DROP FUNCTION IF EXISTS orchestrator_reject_contribution_revision_identity_change();
DROP TABLE IF EXISTS orchestrator_contribution_revisions;
