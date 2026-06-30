# Operation Model

Operation is the audit unit for orchestration changes and observations.

Operations target formal objects such as service releases, services, endpoints, links, topology, log views, and diagnostic reports. Deployment templates are not operation targets.

The state machine is:

```text
PLANNED
AWAITING_CONFIRMATION
RUNNING
SUCCEEDED
FAILED
ROLLED_BACK
CANCELLED
EXPIRED
```

Planning persists `operation_id`, `action`, `target_type`, `target_id`, `plan`, `status`, `created_at`, and `updated_at`. Confirming writes `AWAITING_CONFIRMATION`. Applying obtains an operation lock, enters `RUNNING`, writes step logs, then writes result or error state. Rollback marks the original operation as `ROLLED_BACK` and writes rollback logs.

Executors only support fixed actions. Arbitrary shell, arbitrary script paths, user-provided command strings, and remote root shells are outside the model.

Current fixed drivers are:

```text
LocalProcessDriver
DockerComposeDriver
ExternalEndpointDriver
```

GUI, TUI, and daemon use the same dispatcher and store-backed operation state machine. They cannot bypass core to mutate state or run actions.
