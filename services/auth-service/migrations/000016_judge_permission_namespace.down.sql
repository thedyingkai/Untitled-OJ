-- Expand-only rollback by design.
--
-- These permission codes can also be materialized by the signed Contribution
-- projection, and authorization relationships can be changed after rollout.
-- Without per-row migration provenance, deleting them here could erase state
-- that this migration did not create. Older Judge revisions ignore the new
-- namespace, so retaining these definitions and grants is safe during a rolling
-- rollback. A future contract migration may remove the legacy namespace only
-- after every supported consumer has stopped using it.
SELECT 1;
