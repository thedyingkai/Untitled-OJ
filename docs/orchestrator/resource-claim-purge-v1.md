# ResourceClaim purge v1

`POST /api/v1/resources/{claimId}:purge` is the only destructive path for a
retained database. It is Admin-only, requires `Idempotency-Key`, and creates a
durable `resource.purge` Operation with a non-retry-safe `resource_purge` Agent
Job.

The request repeats the exact claim digest and generation and supplies
`PURGE {claim_id} {claim_digest} GENERATION {generation}` as `confirmation`.
The authenticated principal is injected as the audit actor; an actor field in
the body is rejected. The Agent refuses purge unless the claim is `RETAINED`
and no deployment binding remains. PostgreSQL credentials and generated DSNs
never enter this request, Job payload, Operation result, or audit log.
