import json
import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
CAPACITY = ROOT / "deploy" / "capacity"


class CapacityAnsibleAssetTests(unittest.TestCase):
    def test_inventory_declares_the_exact_host_shape(self):
        inventory = (CAPACITY / "inventory.example.yml").read_text(encoding="utf-8")
        self.assertEqual(inventory.count("capacity-worker-"), 10)
        self.assertEqual(inventory.count("capacity_worker_ordinal:"), 10)
        addresses = re.findall(r"ansible_host:\s*([^, }\r\n]+)", inventory)
        self.assertEqual(len(addresses), 13)
        self.assertEqual(len({address.strip().lower() for address in addresses}), 13)
        for group in (
            "orchestrator_database:",
            "orchestrator_control_plane:",
            "orchestrator_soak_runner:",
            "orchestrator_workers:",
        ):
            self.assertIn(group, inventory)

    def test_worker_template_has_ten_isolated_engine_socket_and_ledger_pairs(self):
        template = (CAPACITY / "templates" / "worker-compose.yml.j2").read_text(
            encoding="utf-8"
        )
        self.assertIn("range(0, 10)", template)
        self.assertIn("/var/lib/docker", template)
        self.assertIn("-socket:/var/run", template)
        self.assertIn("execution-ledger.sqlite3", template)
        self.assertIn("--registry-credentials", template)
        self.assertIn(
            "/etc/ojos/capacity/registry-credentials.json:/run/secrets/registry-credentials.json:ro",
            template,
        )
        self.assertIn("user: \"65532:65532\"", template)
        self.assertIn('group_add:\n      - "10004"', template)
        internal_root = (
            "/var/lib/ojos/capacity/agent-internal/{{ '%02d' | format(engine) }}:"
            "/var/lib/ojos-agent"
        )
        export_root = (
            "/var/lib/ojos/capacity/workload-exports/{{ '%02d' | format(engine) }}:"
            "/var/lib/ojos-workload-export"
        )
        self.assertEqual(template.count(internal_root), 1)
        self.assertEqual(template.count(export_root), 2)
        self.assertIn(f"{export_root}:ro", template)
        self.assertNotIn(f"{internal_root}:ro", template)
        self.assertIn("--postgres-resource-provider", template)
        self.assertIn("--resource-secret-dir", template)
        engine_block = template[
            template.index("  engine-") : template.index("  agent-")
        ]
        for forbidden in (
            "/var/lib/ojos-agent",
            "agent-internal",
            "registry-credentials",
            "agent-resource-provider",
            "control-plane-ca",
        ):
            self.assertNotIn(forbidden, engine_block)
        self.assertNotIn("cap_add:", template)
        self.assertNotIn("/var/run/docker.sock:/var/run/docker.sock", template)
        self.assertIn("20000 + engine * 20 + service_index", template)
        self.assertIn('0.0.0.0:', template)
        self.assertIn('range(0, 20)', template)
        self.assertEqual(template.count("\n    ports:\n"), 1)

        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        compose_validation = playbook[playbook.index(
            "- name: Reject invalid or duplicate-key worker Compose configuration"
        ):playbook.index("- name: Pull immutable worker images")]
        self.assertIn("- config", compose_validation)
        self.assertIn("- --quiet", compose_validation)

    def test_capacity_configures_strict_agent_postgres_resource_provider(self):
        variables = (CAPACITY / "group_vars" / "all.example.yml").read_text(
            encoding="utf-8"
        )
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        compose = (CAPACITY / "templates" / "worker-compose.yml.j2").read_text(
            encoding="utf-8"
        )
        for variable in (
            "capacity_agent_postgres_provider_file",
            "capacity_agent_postgres_admin_url_file",
            "capacity_agent_postgres_ca_file",
        ):
            self.assertIn(variable, variables)
            self.assertIn(variable, playbook)
        gate = playbook[
            playbook.index("- name: Require one fail-closed verify-full Agent ResourceClaim provider") :
            playbook.index("- name: Read the verified immutable image provenance record")
        ]
        for exact in (
            "schema_version == 1",
            "provider_id == 'postgresql-capacity'",
            "tls_mode == 'verify-full'",
            "admin_url_file == '/run/agent-resource-provider/admin.url'",
            "ca_file == '/run/agent-resource-provider/postgres-ca.crt'",
            "sslmode=require",
        ):
            self.assertIn(exact, gate)
        self.assertIn("--postgres-resource-provider", compose)
        self.assertIn("/run/agent-resource-provider/provider.json", compose)
        self.assertIn("--resource-secret-dir", compose)
        self.assertIn("/var/lib/ojos-agent/resource-provider-secrets", compose)

    def test_node_plan_platform_uses_authenticated_runtime_facts(self):
        template = (CAPACITY / "templates" / "nodes.json.j2").read_text(
            encoding="utf-8"
        )
        labels_match = re.search(r'"labels":\s*(\{.*?\n      \})', template, re.DOTALL)
        self.assertIsNotNone(labels_match)
        labels = json.loads(labels_match.group(1))
        self.assertEqual(
            {key: labels[key] for key in ("runtime", "os", "arch")},
            {"runtime": "docker", "os": "linux", "arch": "x86_64"},
        )
        self.assertEqual(
            labels["providers"]["postgresql"],
            {"enabled": True, "provider_id": "postgresql-capacity"},
        )

        store_api = (
            ROOT / "services" / "orchestrator" / "backend" / "src" / "store_v1_api.rs"
        ).read_text(encoding="utf-8")
        self.assertIn(
            "let facts = node_runtime_facts(storage, &node.node_id)?;", store_api
        )
        self.assertIn('facts.docker.engine != "docker"', store_api)
        self.assertIn("facts.docker.os_type.trim()", store_api)
        self.assertIn("facts.docker.architecture.trim()", store_api)
        self.assertIn('"STORE_NODE_RUNTIME_FACTS_REQUIRED"', store_api)
        self.assertIn('"STORE_NODE_RUNTIME_FACTS_STALE"', store_api)
        self.assertIn('"STORE_TARGET_PLATFORM_INVALID"', store_api)
        self.assertNotIn('node.labels.get("runtime")', store_api)
        self.assertNotIn('node.labels.get("os")', store_api)
        self.assertNotIn('node.labels.get("arch")', store_api)

        catalog_generator = (
            ROOT
            / "services"
            / "orchestrator"
            / "manager"
            / "examples"
            / "generate_capacity_catalog.rs"
        ).read_text(encoding="utf-8")
        self.assertIn('TargetPlatform::new("linux", "x86_64")', catalog_generator)

    def test_enrollment_code_is_uid_readable_isolated_and_removed_before_agent_start(
        self,
    ):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        worker_start = playbook.index(
            "- name: Configure 10 isolated Engines and Agents on every worker"
        )
        worker_end = playbook.index(
            "- name: Configure the dedicated self-hosted soak runner", worker_start
        )
        worker_play = playbook[worker_start:worker_end]
        compose = (CAPACITY / "templates" / "worker-compose.yml.j2").read_text(
            encoding="utf-8"
        )
        enrollment = (CAPACITY / "tasks" / "enroll-capacity-agent.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn('user: "65532:65532"', compose)
        self.assertIn('group_add:\n      - "10004"', compose)
        self.assertNotIn("enrollment-code", compose)
        self.assertIn(
            "agent-internal/{{ '%02d' | format(engine) }}:/var/lib/ojos-agent", compose
        )
        self.assertNotIn("/var/lib/ojos/capacity/agents:/var/lib/ojos-agent", compose)
        self.assertNotIn("/etc/ojos/capacity/enrollment", worker_play)
        self.assertIn(
            'agent-internal/{{ \'%02d\' | format(item) }}/bootstrap"', worker_play
        )
        self.assertIn('owner: "65532"', worker_play)
        self.assertIn('group: "65532"', worker_play)
        self.assertIn("Create independent Agent-internal state directories", worker_play)
        self.assertNotIn("recurse: true", worker_play)
        self.assertIn("Stop existing Agents before workload identity migration", worker_play)
        self.assertIn("existing_worker_compose.stat.exists", worker_play)
        self.assertIn("Fail closed instead of copying secrets out of the legacy combined layout", worker_play)
        self.assertIn("Create independent workload export directories", worker_play)
        self.assertLess(
            worker_play.index("- name: Stop existing Agents before workload identity migration"),
            worker_play.index("- name: Create independent Agent-internal state directories"),
        )
        self.assertIn('mode: "0700"', worker_play)
        self.assertIn(
            "ansible.builtin.include_tasks: tasks/enroll-capacity-agent.yml",
            worker_play,
        )
        self.assertIn("loop_var: capacity_engine_ordinal", worker_play)

        detect_code = enrollment.index(
            "- name: Detect whether the controller issued a code for this Node"
        )
        require_state = enrollment.index(
            "- name: Require either an installed identity or its controller enrollment code"
        )
        stage = enrollment.index("- name: Stage the code only in this Agent state directory")
        redeem = enrollment.index(
            "- name: Redeem this code into the matching Agent mTLS identity", stage
        )
        parse_result = enrollment.index(
            "- name: Parse the enrollment command result before consuming its controller code",
            redeem,
        )
        validate_result = enrollment.index(
            "- name: Prove the enrollment result is bound to this exact Agent",
            parse_result,
        )
        cleanup = enrollment.index(
            "- name: Remove this Agent staged code immediately after enrollment",
            validate_result,
        )
        controller_cleanup = enrollment.index(
            "- name: Remove this Agent controller code immediately after enrollment",
            cleanup,
        )
        fallback_cleanup = enrollment.index(
            "- name: Erase this Agent staged code after any interrupted enrollment",
            controller_cleanup,
        )
        start_agents = worker_play.index(
            "- name: Recreate each enrolled Agent with the current registry credentials"
        )
        staged = enrollment[stage:redeem]
        redemption = enrollment[redeem:cleanup]
        self.assertIn('owner: "65532"', staged)
        self.assertIn('group: "65532"', staged)
        self.assertIn('mode: "0600"', staged)
        self.assertIn(
            "bootstrap/enrollment-code:/run/secrets/enrollment-code:ro",
            redemption,
        )
        self.assertIn("when: controller_enrollment_code.stat.exists", staged)
        self.assertIn("when: controller_enrollment_code.stat.exists", redemption)
        result_gate = enrollment[parse_result:cleanup]
        self.assertIn("enroll_agent.stdout | from_json", result_gate)
        self.assertIn("status in ['ENROLLED', 'RECOVERED']", result_gate)
        self.assertIn("capacity_enrollment_result.node_id ==", result_gate)
        self.assertIn("capacity_enrollment_result.spiffe_id ==", result_gate)
        self.assertIn("serial_hex is match('^[0-9a-f]{1,128}$')", result_gate)
        self.assertIn(
            "identity_dir == '/var/lib/ojos-agent/identity'", result_gate
        )
        self.assertGreaterEqual(result_gate.count("no_log: true"), 2)
        self.assertNotIn(
            "when: not identity_files.results[capacity_engine_ordinal].stat.exists",
            enrollment,
        )
        self.assertIn("delegate_to: localhost", enrollment[detect_code:require_state])
        self.assertIn(
            "identity_files.results[capacity_engine_ordinal].stat.exists or "
            "controller_enrollment_code.stat.exists",
            enrollment[require_state:stage],
        )
        for coupled_reference in (
            "agent-internal/{{ '%02d' | format(capacity_engine_ordinal) }}",
            '"agent-{{ \'%02d\' | format(capacity_engine_ordinal) }}"',
        ):
            self.assertIn(coupled_reference, redemption)
        self.assertIn("  always:", enrollment[controller_cleanup:fallback_cleanup])
        self.assertIn(
            "delegate_to: localhost",
            enrollment[controller_cleanup:fallback_cleanup],
        )
        self.assertIn(
            "when: controller_enrollment_code.stat.exists",
            enrollment[controller_cleanup:fallback_cleanup],
        )
        self.assertLess(detect_code, require_state)
        self.assertLess(require_state, stage)
        self.assertLess(stage, redeem)
        self.assertLess(redeem, parse_result)
        self.assertLess(parse_result, validate_result)
        self.assertLess(validate_result, cleanup)
        self.assertLess(cleanup, controller_cleanup)
        self.assertLess(controller_cleanup, fallback_cleanup)
        self.assertLess(
            worker_play.index("tasks/enroll-capacity-agent.yml"), start_agents
        )

        # `from_json` fails the block for malformed stdout and the assert fails
        # for a successful command with a wrong binding. Both happen before any
        # deletion; the `always` block erases only the staged worker copy.
        always_block = enrollment[enrollment.index("  always:") :]
        self.assertNotIn("capacity_fixture_output_dir", always_block)

    def test_exit_zero_invalid_enrollment_stdout_preserves_controller_code(self):
        enrollment = (CAPACITY / "tasks" / "enroll-capacity-agent.yml").read_text(
            encoding="utf-8"
        )
        parse = enrollment.index("enroll_agent.stdout | from_json")
        validate = enrollment.index(
            "- name: Prove the enrollment result is bound to this exact Agent", parse
        )
        controller_delete = enrollment.index(
            "- name: Remove this Agent controller code immediately after enrollment",
            validate,
        )
        gate = enrollment[validate:controller_delete]
        for required_binding in (
            "status in ['ENROLLED', 'RECOVERED']",
            "capacity_enrollment_result.node_id ==",
            "capacity_enrollment_result.spiffe_id ==",
            "serial_hex is match('^[0-9a-f]{1,128}$')",
            "not_after_ms > capacity_enrollment_result.renew_after_ms",
            "identity_dir == '/var/lib/ojos-agent/identity'",
        ):
            self.assertIn(required_binding, gate)
        self.assertLess(parse, validate)
        self.assertLess(validate, controller_delete)
        self.assertNotIn(
            "capacity_fixture_output_dir", enrollment[enrollment.index("  always:") :]
        )

    def test_playbook_uses_only_public_v1_seed_and_digest_images(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        environment_tool = (
            CAPACITY / "orchestrator-capacity-environment.py"
        ).read_text(encoding="utf-8")
        self.assertIn("orchestrator-capacity-environment.py", playbook)
        self.assertIn("@sha256:[0-9a-f]{64}", playbook)
        self.assertIn("org.opencontainers.image.revision", playbook)
        self.assertNotIn("psql", environment_tool.lower())
        self.assertNotIn("sqlite3", environment_tool.lower())
        self.assertNotIn("/api/v0", environment_tool)
        self.assertIn("/api/v1/store/releases:install", environment_tool)
        self.assertIn("/api/v1/topologies", environment_tool)

    def test_signed_catalog_tree_is_not_used_as_the_runtime_evidence_workspace(self):
        variables = (CAPACITY / "group_vars" / "all.example.yml").read_text(
            encoding="utf-8"
        )
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        self.assertIn(
            'capacity_catalog_output_dir: "{{ capacity_fixture_output_dir }}/catalog"',
            variables,
        )
        generator = playbook[playbook.index(
            "- name: Generate and self-verify the signed Catalog v2 fixture"
        ):playbook.index("- name: Validate the four generated fixture documents exist")]
        self.assertIn('"{{ capacity_catalog_output_dir }}"', generator)
        node_plan = playbook[playbook.index(
            "- name: Render the deterministic 100-Node plan"
        ):playbook.index("- name: Prepare Linux x64 hosts and Docker Compose")]
        self.assertIn('dest: "{{ capacity_fixture_output_dir }}/nodes.json"', node_plan)
        self.assertNotIn("capacity_catalog_output_dir", node_plan)
        self.assertIn(
            'src: "{{ capacity_catalog_output_dir }}/"',
            playbook,
        )
        catalog_override = (
            CAPACITY / "templates" / "catalog-override.env.j2"
        ).read_text(encoding="utf-8")
        self.assertIn(
            "capacity_catalog_output_dir ~ '/trust.json'", catalog_override
        )
        self.assertIn(
            "capacity_catalog_output_dir ~ '/catalog-source.json'", catalog_override
        )
        self.assertNotIn("capacity_fixture_output_dir", catalog_override)

    def test_playbook_rejects_duplicate_actual_inventory_addresses(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        self.assertIn("Collect the fixed 13 capacity inventory hosts", playbook)
        self.assertIn("hostvars[item].ansible_host is defined", playbook)
        self.assertIn("map('extract', hostvars, 'ansible_host')", playbook)
        self.assertIn("capacity_inventory_addresses | unique | list | length == 13", playbook)

    def test_playbook_binds_provisioning_to_the_exact_clean_candidate_tree(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        first_play = playbook[:playbook.index(
            "- name: Prepare Linux x64 hosts and Docker Compose"
        )]
        self.assertIn("argv: [git, rev-parse, HEAD]", first_play)
        self.assertIn(
            "argv: [git, status, --porcelain=v1, --untracked-files=all]",
            first_play,
        )
        self.assertIn(
            "capacity_repository_head.stdout == capacity_candidate_sha", first_play
        )
        self.assertIn("capacity_repository_status.stdout == ''", first_play)

    def test_control_plane_waits_for_real_verify_full_postgres_before_migration(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        compose = (CAPACITY / "templates" / "control-plane-compose.yml.j2").read_text(
            encoding="utf-8"
        )
        self.assertIn("database-ready:", compose)
        self.assertIn(
            "PGDATABASE: ${ORCHESTRATOR_MIGRATION_DATABASE_URL", compose
        )
        self.assertIn("PGCONNECT_TIMEOUT: \"3\"", compose)
        self.assertIn("entrypoint: [psql]", compose)
        self.assertIn(
            "orchestrator-postgres-ca.crt:/run/secrets/orchestrator-postgres-ca.crt:ro",
            compose,
        )
        ready = playbook.index(
            "- name: Prove the exact verify-full PostgreSQL URL and CA are ready"
        )
        start = playbook.index(
            "- name: Start the single-active candidate control plane", ready
        )
        self.assertLess(ready, start)
        ready_task = playbook[ready:start]
        self.assertIn("- database-ready", ready_task)
        self.assertIn("retries: 20", ready_task)
        self.assertIn("delay: 3", ready_task)
        self.assertIn(
            "capacity_runtime_database_url == capacity_migration_database_url",
            playbook,
        )
        self.assertIn("sslmode=verify-full", playbook)

    def test_playbook_rejects_aliases_that_reach_the_same_physical_host(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        start = playbook.index("- name: Prepare Linux x64 hosts and Docker Compose")
        end = playbook.index(
            "- name: Configure the dedicated TLS PostgreSQL host", start
        )
        observed_separation = playbook[start:end]
        self.assertIn("gather_facts: true", observed_separation)
        self.assertIn("ansible_facts.machine_id", observed_separation)
        self.assertIn("ansible_facts.default_ipv4.address", observed_separation)
        self.assertIn("capacity_observed_machine_ids | length == 13", observed_separation)
        self.assertIn(
            "capacity_observed_machine_ids | unique | list | length == 13",
            observed_separation,
        )
        self.assertIn(
            "capacity_observed_default_ipv4_addresses | unique | list | length == 13",
            observed_separation,
        )
        self.assertGreaterEqual(observed_separation.count("run_once: true"), 2)

    def test_playbook_verifies_fixture_identity_in_all_100_engines(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        start = playbook.index(
            "- name: Collect real candidate-bound workload evidence from all 100 Engines"
        )
        end = playbook.index(
            "- name: Verify and record the real production-capacity state", start
        )
        fixture_verification = playbook[start:end]
        self.assertIn("hosts: orchestrator_workers", fixture_verification)
        self.assertIn("orchestrator-capacity-engine-evidence.py", fixture_verification)
        self.assertIn("- collect", fixture_verification)
        self.assertIn("--worker-ordinal", fixture_verification)
        self.assertIn("capacity_candidate_sha", fixture_verification)
        self.assertIn("capacity_fixture_image", fixture_verification)
        self.assertIn("container_count == 200", fixture_verification)
        self.assertIn("engine_count == 10", fixture_verification)
        self.assertIn("ansible.builtin.fetch", fixture_verification)

    def test_runner_reruns_preserve_uptime_until_configuration_changes(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        start = playbook.index("- name: Configure the dedicated self-hosted soak runner")
        end = playbook.index("- name: Seed the real production-capacity state", start)
        runner_play = playbook[start:end]
        runner_tasks, runner_handlers = runner_play.split("  handlers:\n", 1)

        self.assertIn("- --disableupdate", runner_tasks)
        self.assertGreaterEqual(
            runner_tasks.count(
                "notify: Restart dedicated soak runner after configuration changes"
            ),
            5,
        )
        self.assertIn("ansible.builtin.meta: flush_handlers", runner_tasks)
        self.assertIn(
            'argv: [systemctl, is-active, --quiet, "{{ capacity_runner_service_name }}"]',
            runner_tasks,
        )
        self.assertIn(
            "when: capacity_runner_service_active.rc != 0", runner_tasks
        )
        self.assertNotIn("runner_service_status.rc", runner_tasks)
        self.assertNotIn("Start or restart the runner service", runner_tasks)
        self.assertNotIn(
            'argv: ["{{ capacity_github_runner_dir }}/svc.sh", stop]',
            runner_tasks,
        )

        service_install_start = runner_tasks.index(
            "- name: Install the runner service"
        )
        service_install_end = runner_tasks.index(
            "- name: Preserve a restart request for every actual runner mutation",
            service_install_start,
        )
        service_install = runner_tasks[service_install_start:service_install_end]
        self.assertIn(
            'path: "{{ capacity_github_runner_dir }}/.service"', runner_tasks
        )
        self.assertIn("register: runner_service_installation", runner_tasks)
        self.assertIn(
            "when: not runner_service_installation.stat.exists", service_install
        )
        self.assertNotIn("runner_registration", service_install)
        self.assertIn(
            "notify: Restart dedicated soak runner after configuration changes",
            service_install,
        )

        self.assertIn(
            'argv: ["{{ capacity_github_runner_dir }}/svc.sh", stop]',
            runner_handlers,
        )
        self.assertIn(
            'argv: ["{{ capacity_github_runner_dir }}/svc.sh", start]',
            runner_handlers,
        )
        self.assertNotIn("svc.sh\", status", runner_handlers)

    def test_runner_recovers_when_service_install_finished_before_start(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        start = playbook.index("- name: Configure the dedicated self-hosted soak runner")
        end = playbook.index("- name: Seed the real production-capacity state", start)
        runner_play = playbook[start:end]

        self.assertIn(") != capacity_runner_desired_fingerprint", runner_play)
        self.assertIn(
            "or not runner_service_installation.stat.exists", runner_play
        )
        self.assertIn(
            "changed_when: capacity_runner_restart_required | bool", runner_play
        )
        self.assertIn(
            "when: capacity_runner_service_active.rc != 0", runner_play
        )

        install = runner_play.index("- name: Install the runner service")
        flush = runner_play.index("ansible.builtin.meta: flush_handlers", install)
        active_confirmation = runner_play.index(
            "- name: Require the dedicated runner service to be active", flush
        )
        applied_commit = runner_play.index(
            "- name: Commit the applied fingerprint only after active confirmation",
            active_confirmation,
        )
        self.assertLess(install, flush)
        self.assertLess(flush, active_confirmation)
        self.assertLess(active_confirmation, applied_commit)

    def test_runner_recovers_when_copy_finished_before_handler(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        start = playbook.index("- name: Configure the dedicated self-hosted soak runner")
        end = playbook.index("- name: Seed the real production-capacity state", start)
        runner_play = playbook[start:end]

        self.assertIn("capacity_runner_desired_gate_environment.stat.checksum", runner_play)
        self.assertIn(
            "capacity_runner_desired_helper_fingerprint_components", runner_play
        )
        for repository_owned_input in (
            "orchestrator-capacity-live-evidence.py",
            "orchestrator-capacity-engine-evidence.py",
            "orchestrator-capacity-environment.py",
            "live-evidence.yml",
            "environment-observer.json.j2",
        ):
            self.assertIn(repository_owned_input, runner_play)
        self.assertIn("capacity_fixture_output_dir ~ '/nodes.json'", runner_play)
        self.assertIn(
            "capacity_catalog_output_dir ~ '/capacity-fixture.json'", runner_play
        )
        self.assertIn(
            "lookup('file', capacity_control_plane_ca_file, rstrip=False)",
            runner_play,
        )
        self.assertIn(".ojos-capacity-config-applied.sha256", runner_play)
        invalidate = runner_play.index(
            "- name: Invalidate stale applied state before mutating runner configuration"
        )
        helper_copy = runner_play.index(
            "- name: Install protected token and restart helpers", invalidate
        )
        environment_copy = runner_play.index(
            "- name: Install protected gate environment", helper_copy
        )
        flush = runner_play.index(
            "ansible.builtin.meta: flush_handlers", environment_copy
        )
        active_confirmation = runner_play.index(
            "- name: Require the dedicated runner service to be active", flush
        )
        applied_commit = runner_play.index(
            "- name: Commit the applied fingerprint only after active confirmation",
            active_confirmation,
        )
        self.assertLess(invalidate, helper_copy)
        self.assertLess(helper_copy, environment_copy)
        self.assertLess(environment_copy, flush)
        self.assertLess(flush, active_confirmation)
        self.assertLess(active_confirmation, applied_commit)

    def test_runner_rejects_a_non_exact_deployed_helper_manifest(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        start = playbook.index("- name: Configure the dedicated self-hosted soak runner")
        end = playbook.index("- name: Seed the real production-capacity state", start)
        runner_play = playbook[start:end]

        self.assertIn(
            'paths: "{{ capacity_github_runner_dir }}/protected"', runner_play
        )
        self.assertIn("checksum_algorithm: sha256", runner_play)
        self.assertIn(
            "capacity_runner_deployed_helper_fingerprint_components", runner_play
        )
        self.assertIn(
            "== capacity_runner_desired_helper_fingerprint_components", runner_play
        )
        self.assertIn(
            "capacity_runner_deployed_helper_links.matched | int == 0", runner_play
        )
        self.assertIn(
            "when: not capacity_runner_deployed_helpers_match | bool", runner_play
        )

        helper_copy = runner_play.index(
            "- name: Install protected token and restart helpers"
        )
        deployed_inventory = runner_play.index(
            "- name: Inventory deployed protected runner helpers after copying",
            helper_copy,
        )
        invalidate = runner_play.index(
            "- name: Invalidate applied state when deployed helpers are not exact",
            deployed_inventory,
        )
        fail_closed = runner_play.index(
            "- name: Fail closed on a stale missing or mismatched deployed helper",
            invalidate,
        )
        flush = runner_play.index("ansible.builtin.meta: flush_handlers", fail_closed)
        applied_commit = runner_play.index(
            "- name: Commit the applied fingerprint only after active confirmation",
            flush,
        )
        self.assertLess(helper_copy, deployed_inventory)
        self.assertLess(deployed_inventory, invalidate)
        self.assertLess(invalidate, fail_closed)
        self.assertLess(fail_closed, flush)
        self.assertLess(flush, applied_commit)

        def manifests_match(desired, deployed, deployed_link_count=0):
            return deployed_link_count == 0 and deployed == desired

        desired = ("refresh-token:sha-a", "nested/restart:sha-b")
        self.assertTrue(manifests_match(desired, desired))
        self.assertFalse(manifests_match(desired, desired + ("stale:sha-c",)))
        self.assertFalse(manifests_match(desired, desired[:1]))
        self.assertFalse(
            manifests_match(desired, ("refresh-token:wrong", desired[1]))
        )
        self.assertFalse(manifests_match(desired, desired, deployed_link_count=1))

    def test_existing_runner_identity_changes_fail_closed(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        start = playbook.index("- name: Configure the dedicated self-hosted soak runner")
        end = playbook.index("- name: Seed the real production-capacity state", start)
        runner_play = playbook[start:end]

        for identity_material in (
            "capacity_github_runner_archive_url",
            "capacity_github_runner_archive_sha256",
            "capacity_github_runner_expected_version",
            "capacity_github_repository_url",
            "runner=ojos-orchestrator-soak",
            "labels=self-hosted,linux,x64,orchestrator-soak",
            "work=_work",
            "disableupdate=true",
            "capacity_github_runner_user",
            "capacity_github_runner_dir",
        ):
            self.assertIn(identity_material, runner_play)
        self.assertIn(
            "Fail closed when an existing runner does not match the pinned identity",
            runner_play,
        )
        self.assertIn(
            "capacity_runner_applied_identity_fingerprint == capacity_runner_desired_identity_fingerprint",
            runner_play,
        )
        self.assertIn(
            "capacity_runner_pending_identity_fingerprint == capacity_runner_desired_identity_fingerprint",
            runner_play,
        )

        journal = runner_play.index(
            "- name: Journal the pinned identity before first registration"
        )
        registration = runner_play.index(
            "- name: Register exact dedicated soak labels", journal
        )
        active_confirmation = runner_play.index(
            "- name: Require the dedicated runner service to be active", registration
        )
        applied_identity = runner_play.index(
            "- name: Commit the pinned runner identity only after active confirmation",
            active_confirmation,
        )
        clear_pending = runner_play.index(
            "- name: Clear the pending runner identity journal after commit",
            applied_identity,
        )
        self.assertLess(journal, registration)
        self.assertLess(registration, active_confirmation)
        self.assertLess(active_confirmation, applied_identity)
        self.assertLess(applied_identity, clear_pending)

        def identity_is_proven(registration_exists, applied, pending, desired):
            return (
                not registration_exists
                or applied == desired
                or (applied == "" and pending == desired)
            )

        self.assertTrue(identity_is_proven(True, "", "desired", "desired"))
        self.assertTrue(identity_is_proven(False, "", "", "desired"))
        self.assertFalse(identity_is_proven(True, "old", "", "desired"))
        self.assertFalse(identity_is_proven(True, "", "old", "desired"))

    def test_runner_toolchain_and_registration_token_are_pinned_and_just_in_time(self):
        variables = (CAPACITY / "group_vars" / "all.example.yml").read_text(
            encoding="utf-8"
        )
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        runner_start = playbook.index(
            "- name: Configure the dedicated self-hosted soak runner"
        )
        runner_end = playbook.index(
            "- name: Seed the real production-capacity state", runner_start
        )
        runner = playbook[runner_start:runner_end]

        self.assertIn('capacity_github_runner_expected_version: "2.336.0"', variables)
        self.assertIn('capacity_github_cli_version: "2.97.0"', variables)
        self.assertIn("capacity_github_cli_archive_sha256:", variables)
        self.assertIn('capacity_jq_version: "1.7.1"', variables)
        self.assertIn("capacity_jq_binary_sha256:", variables)
        self.assertIn("Install the checksum-pinned jq binary", runner)
        self.assertIn("argv: [/usr/local/bin/jq, --version]", runner)
        self.assertIn("capacity_jq_version_output.stdout | trim == 'jq-' ~ capacity_jq_version", runner)
        self.assertNotIn("creates: \"/opt/ojos/github-cli-", runner)
        self.assertNotIn("capacity_github_runner_registration_token:", variables)
        self.assertIn(
            "capacity_github_runner_registration_token_argv_json:", variables
        )
        for required in (
            "gh, attestation, verify, --help",
            "--cert-oidc-issuer",
            "--signer-workflow",
            "--source-ref",
            "--source-digest",
            "--deny-self-hosted-runners",
            "Runner.Listener",
            "capacity_github_runner_expected_version",
        ):
            self.assertIn(required, runner)

        acquire = runner.index(
            "Acquire a just-in-time GitHub runner registration token without a shell"
        )
        register = runner.index("Register exact dedicated soak labels", acquire)
        erase = runner.index(
            "Erase all just-in-time registration token facts after use", register
        )
        self.assertLess(acquire, register)
        self.assertLess(register, erase)
        registration = runner[register:erase]
        self.assertIn("capacity_runner_registration_token_payload.token", registration)
        self.assertNotIn("capacity_github_runner_registration_token }}", registration)
        acquisition = runner[acquire:register]
        self.assertIn("timeout: 30", acquisition)
        self.assertIn("immediately after token acquisition", acquisition)
        self.assertIn("capacity_runner_registration_token_result.stderr == ''", acquisition)
        self.assertIn("expires_at | type_debug == 'int'", acquisition)
        self.assertIn("capacity_runner_registration_token_result: {}", runner[erase:])

    def test_runner_environment_and_selected_helpers_are_exact(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        runner_start = playbook.index(
            "- name: Configure the dedicated self-hosted soak runner"
        )
        runner_end = playbook.index(
            "- name: Seed the real production-capacity state", runner_start
        )
        runner = playbook[runner_start:runner_end]

        self.assertIn(
            "Require exactly eight unique protected runner environment bindings",
            runner,
        )
        runner_parse = runner.index(
            "Parse selected protected helper argv in the runner host context"
        )
        deployed_helper_check = runner.index(
            "Inspect selected deployed token and restart helpers"
        )
        self.assertLess(runner_parse, deployed_helper_check)
        self.assertIn(
            "capacity_runner_actual_gate_environment_lines == capacity_runner_expected_gate_environment_lines",
            runner,
        )
        self.assertIn("Inspect selected deployed token and restart helpers", runner)
        self.assertIn("item.stat.executable", playbook[:runner_end])
        self.assertIn("capacity_runner_required_helper_sources", playbook[:runner_end])

    def test_private_candidate_registry_auth_reaches_host_and_engine_pulls(self):
        variables = (CAPACITY / "group_vars" / "all.example.yml").read_text(
            encoding="utf-8"
        )
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        runtime = (
            ROOT / "services" / "orchestrator" / "runtime" / "src" / "lib.rs"
        ).read_text(encoding="utf-8")
        agent = (
            ROOT / "services" / "orchestrator" / "agent" / "src" / "main.rs"
        ).read_text(encoding="utf-8")
        live_gate = (
            ROOT / "deploy" / "ops" / "orchestrator-docker-agent-e2e.sh"
        ).read_text(encoding="utf-8")

        for name in (
            "capacity_registry_server",
            "capacity_registry_username",
            "capacity_registry_password_file",
        ):
            self.assertIn(name, variables)
            self.assertIn(name, playbook)
        self.assertIn("Materialize authenticated candidate-registry access", playbook)
        self.assertIn("/etc/ojos/capacity/docker-auth", playbook)
        self.assertIn("Materialize strict read-only Agent registry credentials", playbook)
        self.assertIn(
            "Recreate each enrolled Agent with the current registry credentials",
            playbook,
        )
        self.assertIn("--force-recreate", playbook)
        recreate = playbook.index(
            "Recreate each enrolled Agent with the current registry credentials"
        )
        recreate_end = playbook.index("\n      changed_when: true", recreate)
        recreate_task = playbook[recreate:recreate_end]
        self.assertIn("--no-deps", recreate_task)
        self.assertNotIn("when:", recreate_task)
        self.assertNotIn("capacity_agent_registry_credentials.changed", playbook)
        self.assertIn("with_registry_credentials_file", runtime)
        self.assertIn("self.credentials_for(image)", runtime)
        self.assertIn('long = "registry-credentials"', agent)
        self.assertIn("must reject anonymous pulls", live_gate)
        self.assertIn("OJOS_DOCKER_E2E_REGISTRY_CREDENTIALS", live_gate)
        self.assertIn("registry_credentials_path=", live_gate)
        self.assertIn("cygpath -w", live_gate)
        self.assertIn("must be absent before the authenticated pull", live_gate)

    def test_worker_ipv4_contract_fails_before_any_remote_mutation(self):
        playbook = (CAPACITY / "site.yml").read_text(encoding="utf-8")
        ipv4 = playbook.index(
            "Require worker addresses to be literal IPv4 endpoints used by Node plans"
        )
        remote = playbook.index("- name: Prepare Linux x64 hosts and Docker Compose")
        self.assertLess(ipv4, remote)
        self.assertIn("advertised Node host_ip", playbook[ipv4:remote])

    def test_capacity_runbooks_match_the_production_workflow_contract(self):
        readme = (CAPACITY / "README.md").read_text(encoding="utf-8")
        variables = (CAPACITY / "group_vars" / "all.example.yml").read_text(
            encoding="utf-8"
        )
        operations = (
            ROOT / "docs" / "orchestrator" / "operations-v1.md"
        ).read_text(encoding="utf-8")
        bootstrap = readme[
            readme.index("## Candidate-image bootstrap") : readme.index("## Run")
        ]
        bootstrap_script = bootstrap.split("```bash", 1)[1].split("```", 1)[0].lstrip()

        self.assertIn("`capacity_image_provenance_record_file`", readme)
        self.assertNotIn("`capacity_provenance_commit_file`", readme)
        self.assertIn(
            "Do not configure `ORCHESTRATOR_GATE_ENVIRONMENT_ARGV_JSON` as a GitHub",
            readme,
        )
        self.assertIn("dedicated runner service\nenvironment", readme)
        self.assertIn(
            '-f candidate_image_run_id="$CANDIDATE_IMAGE_RUN_ID"', readme
        )
        self.assertIn(
            '-f candidate_image_run_id="$CANDIDATE_IMAGE_RUN_ID"', operations
        )
        self.assertIn("唯一受支持入口是下面的首次 workflow dispatch", operations)
        self.assertIn("orchestrator-candidate-image-provenance", readme)
        self.assertIn("GHCR Container packages are private by default", readme)
        self.assertTrue(bootstrap_script.startswith("set -Eeuo pipefail\n"))
        strict = bootstrap_script.index("set -Eeuo pipefail")
        for action in (
            'CANDIDATE_SHA="$(git rev-parse HEAD)"',
            'docker --config "$DOCKER_CONFIG" login',
            'run_json="$(gh api',
            'gh run download "$CANDIDATE_IMAGE_RUN_ID"',
            "verify-orchestrator-image-provenance.py",
            'gh attestation verify "oci://$reference"',
        ):
            self.assertLess(strict, bootstrap_script.index(action), action)

        password_path = re.search(
            r'^capacity_registry_password_file:\s*"([^"]+)"$',
            variables,
            re.MULTILINE,
        )
        self.assertIsNotNone(password_path)
        self.assertIn(
            f"export capacity_registry_password_file={password_path.group(1)}",
            bootstrap_script,
        )
        self.assertIn('docker_config="$work/docker-config"', bootstrap)
        self.assertIn('install -d -m 0700 "$docker_config"', bootstrap)
        self.assertIn('export DOCKER_CONFIG="$docker_config"', bootstrap)
        self.assertIn(
            'docker --config "$DOCKER_CONFIG" login "$capacity_registry_server"',
            bootstrap,
        )
        self.assertIn('--username "$capacity_registry_username"', bootstrap)
        self.assertIn(
            '--password-stdin <"$capacity_registry_password_file"', bootstrap
        )
        self.assertNotIn('$(cat "$capacity_registry_password_file")', bootstrap)
        login = bootstrap_script.index("docker --config")
        attestation = bootstrap_script.index(
            'gh attestation verify "oci://$reference"'
        )
        self.assertLess(login, attestation)
        attestation_context = bootstrap_script.index(
            'DOCKER_CONFIG="$docker_config" \\', login
        )
        self.assertLess(attestation_context, attestation)
        self.assertIn("trap cleanup_candidate_bootstrap EXIT", bootstrap)
        self.assertIn(
            "capacity_runner_gate_env_base_file=/protected/orchestrator-capacity/runner-base.env",
            bootstrap_script,
        )
        self.assertIn(
            "capacity_candidate_vars_file=/protected/orchestrator-capacity/candidate-images.vars.json",
            bootstrap_script,
        )
        self.assertIn(
            "capacity_runner_candidate_bindings_file=/protected/orchestrator-capacity/candidate-images.runner-bindings.env",
            bootstrap_script,
        )
        for binding in (
            "ORCHESTRATOR_GATE_CONTROL_PLANE_IMAGE",
            "ORCHESTRATOR_GATE_AGENT_IMAGE",
            "ORCHESTRATOR_GATE_FIXTURE_IMAGE",
            "ORCHESTRATOR_GATE_IMAGE_WORKFLOW_RUN_ID",
            "ORCHESTRATOR_GATE_IMAGE_PROVENANCE_RECORD_SHA256",
        ):
            self.assertIn(binding, bootstrap_script)

        persist = bootstrap_script.index(
            'install -m 0600 "$work/candidate-image-provenance.json"'
        )
        publish_provenance = bootstrap_script.index(
            'mv -f -- "$persist_stage/candidate-image-provenance.json"'
        )
        publish_bindings = bootstrap_script.index(
            'mv -f -- "$persist_stage/candidate-images.runner-bindings.env"'
        )
        publish_runner = bootstrap_script.index(
            'mv -f -- "$persist_stage/runner.env"'
        )
        publish_vars = bootstrap_script.index(
            'mv -f -- "$persist_stage/candidate-images.vars.json"'
        )
        cleanup = bootstrap_script.index(
            "\ncleanup_candidate_bootstrap\n", publish_vars
        )
        self.assertLess(attestation, persist)
        self.assertLess(persist, publish_provenance)
        self.assertLess(publish_provenance, publish_bindings)
        self.assertLess(publish_bindings, publish_runner)
        self.assertLess(publish_runner, publish_vars)
        self.assertLess(publish_vars, cleanup)
        cleanup_definition_start = bootstrap_script.index(
            "cleanup_candidate_bootstrap() {"
        )
        cleanup_definition_end = bootstrap_script.index(
            "trap cleanup_candidate_bootstrap EXIT", cleanup_definition_start
        )
        cleanup_definition = bootstrap_script[
            cleanup_definition_start:cleanup_definition_end
        ]
        self.assertIn(
            'export DOCKER_CONFIG="$previous_docker_config"',
            cleanup_definition,
        )
        self.assertIn("unset DOCKER_CONFIG", cleanup_definition)
        self.assertLess(cleanup, bootstrap_script.index("trap - EXIT", cleanup))
        self.assertNotIn(
            "$capacity_registry_password_file",
            bootstrap_script[persist:cleanup],
        )
        self.assertIn(
            "--extra-vars @/protected/orchestrator-capacity/candidate-images.vars.json",
            readme,
        )
        self.assertIn("82-second", readme)
        self.assertIn("288 rounds + final = 292", readme)

    def test_no_example_contains_a_real_secret(self):
        variables = (CAPACITY / "group_vars" / "all.example.yml").read_text(
            encoding="utf-8"
        )
        for forbidden in ("BEGIN PRIVATE KEY", "ghp_", "github_pat_", "eyJhbGci"):
            self.assertNotIn(forbidden, variables)


if __name__ == "__main__":
    unittest.main()
