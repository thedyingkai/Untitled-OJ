# Orchestrator Database

OJOS uses separate databases. `ORCHESTRATOR_DATABASE_URL` points to the Orchestrator DB; `OJ_DATABASE_URL` points to the OJ business DB. Business services must not write the Orchestrator DB directly, and the Orchestrator must not write OJ business tables.

Formal Orchestrator tables:

```text
service_releases
host_services
services
service_endpoints
service_links
service_routes
service_migration_records
orchestrator_operations
orchestrator_operation_logs
orchestrator_operation_locks
topology_snapshots
log_sources
diagnostic_reports
```

Endpoint runtime identity is `ip:port:service-name`. `service_id` is kept as a compatibility field and must match the embedded service-name. Link source and target endpoint fields use the same `ip:port:service-name` identity. A value such as `judge-worker[*]` is a derived query over running endpoints by `service_name`, not a formal Orchestrator table.

`deploy/orchestrator-migrations/000001_orchestrator_schema.up.sql` is the formal schema initializer. It must not recreate retired machine, device, installer, service installation, or runtime-manager tables.

`service_routes` stores route declarations. `service_migration_records` tracks `service_name + migration_version`. `log_sources` stores LogView metadata with separate `endpoint` and `service_id` fields.
