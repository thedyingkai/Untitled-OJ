-- Serialize permission namespace ownership with Contribution reconciliation
-- and administrative permission writes for the duration of this migration.
LOCK TABLE permissions IN SHARE ROW EXCLUSIVE MODE;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM permissions
        WHERE code IN (
            'judge.submission.view.own',
            'judge.submission.view.all',
            'judge.submission.manage'
        )
          AND service_code <> 'judge-api'
    ) THEN
        RAISE EXCEPTION 'judge submission permission namespace is owned by another service';
    END IF;
END
$$;

INSERT INTO permissions(code, service_code, name, description)
VALUES
    ('judge.submission.view.own', 'judge-api', 'View Own Submission', 'View own submissions'),
    ('judge.submission.view.all', 'judge-api', 'View All Submissions', 'View all submissions'),
    ('judge.submission.manage', 'judge-api', 'Manage Submissions', 'Cancel and rejudge submissions')
ON CONFLICT (code) DO UPDATE SET
    name = EXCLUDED.name,
    description = EXCLUDED.description
WHERE permissions.service_code = EXCLUDED.service_code;

INSERT INTO role_permissions(role_id, permission_code)
SELECT role_id, 'judge.submission.view.own'
FROM role_permissions
WHERE permission_code IN ('submission.view.own', 'submission.view.all')
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT role_id, 'judge.submission.view.all'
FROM role_permissions
WHERE permission_code = 'submission.view.all'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT rejudge.role_id, 'judge.submission.manage'
FROM role_permissions rejudge
JOIN role_permissions delete_submission
  ON delete_submission.role_id = rejudge.role_id
 AND delete_submission.permission_code = 'submission.delete'
WHERE rejudge.permission_code = 'submission.rejudge'
ON CONFLICT DO NOTHING;

INSERT INTO permission_assignments(
    principal_type,
    principal_id,
    permission_code,
    scope_type,
    scope_id,
    effect,
    granted_by_type,
    granted_by_id,
    reason,
    expires_at,
    created_at
)
SELECT
    principal_type,
    principal_id,
    CASE permission_code
        WHEN 'submission.view.own' THEN 'judge.submission.view.own'
        WHEN 'submission.view.all' THEN 'judge.submission.view.all'
    END,
    scope_type,
    scope_id,
    effect,
    granted_by_type,
    granted_by_id,
    reason,
    expires_at,
    created_at
FROM permission_assignments
WHERE permission_code IN ('submission.view.own', 'submission.view.all')
  AND (expires_at IS NULL OR expires_at > clock_timestamp())
ON CONFLICT (principal_type, principal_id, permission_code, scope_type, scope_id)
DO UPDATE SET
    effect = CASE
        WHEN permission_assignments.expires_at IS NOT NULL
         AND permission_assignments.expires_at <= clock_timestamp() THEN EXCLUDED.effect
        WHEN permission_assignments.effect = 'deny' OR EXCLUDED.effect = 'deny' THEN 'deny'
        ELSE 'allow'
    END,
    expires_at = CASE
        WHEN permission_assignments.expires_at IS NOT NULL
         AND permission_assignments.expires_at <= clock_timestamp() THEN EXCLUDED.expires_at
        WHEN permission_assignments.effect = 'deny' AND permission_assignments.expires_at IS NULL THEN NULL
        WHEN EXCLUDED.effect = 'deny' AND EXCLUDED.expires_at IS NULL THEN NULL
        WHEN permission_assignments.effect = 'deny' AND EXCLUDED.effect = 'deny'
            THEN GREATEST(permission_assignments.expires_at, EXCLUDED.expires_at)
        WHEN EXCLUDED.effect = 'deny' THEN EXCLUDED.expires_at
        WHEN permission_assignments.effect = 'deny' THEN permission_assignments.expires_at
        WHEN permission_assignments.expires_at IS NULL OR EXCLUDED.expires_at IS NULL THEN NULL
        ELSE GREATEST(permission_assignments.expires_at, EXCLUDED.expires_at)
    END;

-- The current Judge authorization path checks view.own before escalating to
-- view.all. Preserve legacy direct view.all allows by adding the prerequisite
-- view.own allow at the same scope. Do not copy a view.all deny to view.own:
-- that would incorrectly revoke access to a principal's own submissions.
INSERT INTO permission_assignments(
    principal_type,
    principal_id,
    permission_code,
    scope_type,
    scope_id,
    effect,
    granted_by_type,
    granted_by_id,
    reason,
    expires_at,
    created_at
)
SELECT
    principal_type,
    principal_id,
    'judge.submission.view.own',
    scope_type,
    scope_id,
    'allow',
    granted_by_type,
    granted_by_id,
    reason,
    expires_at,
    created_at
FROM permission_assignments
WHERE permission_code = 'submission.view.all'
  AND effect = 'allow'
  AND (expires_at IS NULL OR expires_at > clock_timestamp())
ON CONFLICT (principal_type, principal_id, permission_code, scope_type, scope_id)
DO UPDATE SET
    effect = CASE
        WHEN permission_assignments.expires_at IS NOT NULL
         AND permission_assignments.expires_at <= clock_timestamp() THEN EXCLUDED.effect
        WHEN permission_assignments.effect = 'deny' THEN 'deny'
        ELSE 'allow'
    END,
    expires_at = CASE
        WHEN permission_assignments.expires_at IS NOT NULL
         AND permission_assignments.expires_at <= clock_timestamp() THEN EXCLUDED.expires_at
        WHEN permission_assignments.effect = 'deny' THEN permission_assignments.expires_at
        WHEN permission_assignments.expires_at IS NULL OR EXCLUDED.expires_at IS NULL THEN NULL
        ELSE GREATEST(permission_assignments.expires_at, EXCLUDED.expires_at)
    END;

-- The current manage permission combines cancellation and rejudge authority.
-- A legacy direct deny for either constituent action must therefore deny the
-- combined permission at the same scope. Direct allows are intentionally not
-- inferred: two old permissions are not an unambiguous grant of the broader
-- current contract. DISTINCT ON prevents a principal denied both legacy
-- actions from targeting the same current assignment twice in one statement.
INSERT INTO permission_assignments(
    principal_type,
    principal_id,
    permission_code,
    scope_type,
    scope_id,
    effect,
    granted_by_type,
    granted_by_id,
    reason,
    expires_at,
    created_at
)
SELECT DISTINCT ON (principal_type, principal_id, scope_type, scope_id)
    principal_type,
    principal_id,
    'judge.submission.manage',
    scope_type,
    scope_id,
    'deny',
    granted_by_type,
    granted_by_id,
    reason,
    expires_at,
    created_at
FROM permission_assignments
WHERE permission_code IN ('submission.rejudge', 'submission.delete')
  AND effect = 'deny'
  AND (expires_at IS NULL OR expires_at > clock_timestamp())
ORDER BY
    principal_type,
    principal_id,
    scope_type,
    scope_id,
    (expires_at IS NULL) DESC,
    expires_at DESC,
    permission_code
ON CONFLICT (principal_type, principal_id, permission_code, scope_type, scope_id)
DO UPDATE SET
    effect = 'deny',
    expires_at = CASE
        WHEN permission_assignments.expires_at IS NOT NULL
         AND permission_assignments.expires_at <= clock_timestamp() THEN EXCLUDED.expires_at
        WHEN permission_assignments.effect = 'deny' AND permission_assignments.expires_at IS NULL THEN NULL
        WHEN EXCLUDED.expires_at IS NULL THEN NULL
        WHEN permission_assignments.effect = 'deny'
            THEN GREATEST(permission_assignments.expires_at, EXCLUDED.expires_at)
        ELSE EXCLUDED.expires_at
    END;
