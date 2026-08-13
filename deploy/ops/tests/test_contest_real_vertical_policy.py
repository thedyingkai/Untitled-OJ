from __future__ import annotations

import importlib.util
import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
HARNESS_PATH = ROOT / "deploy" / "ops" / "contest-service-real-vertical-e2e.sh"
HARNESS = HARNESS_PATH.read_text(encoding="utf-8")
RUST_DRIVER_PATH = (
    ROOT
    / "services"
    / "orchestrator"
    / "backend"
    / "tests"
    / "contest_service_real_vertical_e2e.rs"
)
RUST_DRIVER = RUST_DRIVER_PATH.read_text(encoding="utf-8")
RUST_FIXTURES = (
    RUST_DRIVER_PATH.parent / "support" / "contest_real_vertical_fixtures.rs"
).read_text(encoding="utf-8")
PORT_ALLOCATOR_PATH = (
    ROOT
    / "deploy"
    / "ops"
    / "fixtures"
    / "contest-service-real-vertical"
    / "allocate_ports.py"
)
PORT_ALLOCATOR_SPEC = importlib.util.spec_from_file_location(
    "contest_vertical_port_allocator", PORT_ALLOCATOR_PATH
)
assert PORT_ALLOCATOR_SPEC is not None and PORT_ALLOCATOR_SPEC.loader is not None
PORT_ALLOCATOR = importlib.util.module_from_spec(PORT_ALLOCATOR_SPEC)
PORT_ALLOCATOR_SPEC.loader.exec_module(PORT_ALLOCATOR)


class ContestRealVerticalPolicyTests(unittest.TestCase):
    def test_ports_are_batch_allocated_outside_linux_ephemeral_range(self) -> None:
        for marker in (
            'port_allocation_file="$run_root/allocated-ports.txt"',
            'if ! python3 "$repo_root/deploy/ops/fixtures/contest-service-real-vertical/allocate_ports.py"',
            '5 >"$port_allocation_file"',
            'mapfile -t allocated_ports <"$port_allocation_file"',
            'rm -f -- "$port_allocation_file"',
            '< /proc/sys/net/ipv4/ip_local_port_range',
            '"$allocated_port" =~ ^[0-9]+$',
            '"$allocated_port" -lt 1024',
            '"$allocated_port" -gt 65535',
            '"$allocated_port" -ge "$ephemeral_port_low"',
            '"$allocated_port" -le "$ephemeral_port_high"',
            '"${#allocated_ports[@]}" -ne 5',
            'sort -u | wc -l)" -ne 5',
            'gateway_port="${allocated_ports[3]}"',
            'gateway_tls_port="${allocated_ports[4]}"',
        ):
            self.assertIn(marker, HARNESS)
        self.assertNotIn("free_port()", HARNESS)
        self.assertNotIn('sock.bind(("127.0.0.1", 0))', HARNESS)
        self.assertEqual(
            PORT_ALLOCATOR.EPHEMERAL_RANGE_PATH,
            "/proc/sys/net/ipv4/ip_local_port_range",
        )

    def test_batch_allocator_holds_distinct_ports_until_selection_completes(self) -> None:
        events: list[tuple[str, int]] = []
        active: set[int] = set()

        class FakeSocket:
            def __init__(self, *_args: object) -> None:
                self.port = 0

            def bind(self, address: tuple[str, int]) -> None:
                _, self.port = address
                if self.port == 1024 or self.port in active:
                    raise OSError("occupied")
                active.add(self.port)
                events.append(("bind", self.port))

            def listen(self, _backlog: int) -> None:
                events.append(("listen", self.port))

            def close(self) -> None:
                if self.port in active:
                    active.remove(self.port)
                    events.append(("close", self.port))

        ports = PORT_ALLOCATOR.allocate_ports(
            3,
            ephemeral_range=(1026, 65533),
            socket_factory=FakeSocket,
        )

        self.assertEqual(ports, [1025, 65534, 65535])
        self.assertTrue(all(not 1026 <= port <= 65533 for port in ports))
        successful_binds = [port for event, port in events if event == "bind"]
        final_listen = max(
            index for index, event in enumerate(events) if event[0] == "listen"
        )
        close_indices = [
            index for index, event in enumerate(events) if event[0] == "close"
        ]
        self.assertEqual(successful_binds, ports)
        self.assertEqual(len(close_indices), len(ports))
        self.assertTrue(all(index > final_listen for index in close_indices))
        self.assertEqual(active, set())

    def test_batch_allocator_rejects_invalid_injected_ephemeral_range(self) -> None:
        with self.assertRaisesRegex(ValueError, "invalid ephemeral port range"):
            PORT_ALLOCATOR.allocate_ports(1, ephemeral_range=(65535, 1024))

    def test_run_root_uses_trusted_tmp_and_stays_private_until_handoff(self) -> None:
        initialization_end = HARNESS.index('staged_repo_root="$run_root/staged-repo"')
        initialization = HARNESS[:initialization_end]
        for marker in (
            'run_parent="/tmp"',
            'run_parent_canonical="$(realpath -e -- "$run_parent"',
            '"$run_parent_canonical" != "$run_parent"',
            '! -d "$run_parent"',
            '-L "$run_parent"',
            '"$run_parent_contract" != "0:0:1777"',
            'mktemp -d -p "$run_parent" ojos-contest-real-vertical.XXXXXXXX',
            '"$(stat -c \'%u:%g:%a\' -- "$run_root")" != "$invoking_uid:$invoking_gid:700"',
        ):
            self.assertIn(marker, initialization)
        self.assertNotIn("RUNNER_TEMP", initialization)
        self.assertNotIn("${TMPDIR", initialization)

        private_check = HARNESS.index(
            '"$(stat -c \'%u:%g:%a\' -- "$run_root")" != "$invoking_uid:$invoking_gid:700"'
        )
        handoff = HARNESS.index('chmod 0755 "$run_root"')
        handoff_check = HARNESS.index(
            '"$(stat -c \'%u:%g:%a\' -- "$run_root")" != "$invoking_uid:$invoking_gid:755"'
        )
        self.assertLess(private_check, handoff)
        self.assertLess(handoff, handoff_check)

    def test_workload_output_preclean_is_runner_owned_and_path_constrained(self) -> None:
        self.assertNotIn('sudo -u "#$workload_uid"', HARNESS)
        mkdir = HARNESS.index(
            'mkdir -p "$output_root"'
        )
        preclean = HARNESS.index('rm -f -- "$evidence"', mkdir)
        chown = HARNESS.index(
            'sudo chown "$workload_uid:$workload_gid" "$output_root"', preclean
        )
        self.assertLess(mkdir, preclean)
        self.assertLess(preclean, chown)
        preclean_block = HARNESS[mkdir:chown]
        for marker in (
            '"$(dirname -- "$evidence")" != "$output_root"',
            '"$(basename -- "$evidence")" != "live-evidence.json"',
            "refusing unsafe contest vertical evidence preclean",
            'rm -f -- "$evidence"',
        ):
            self.assertIn(marker, preclean_block)
        self.assertNotIn("sudo rm", preclean_block)
        self.assertNotIn("setpriv", preclean_block)

    def test_real_test_keeps_exact_workload_identity_and_socket_group(self) -> None:
        home_marker = '  "HOME=$scratch_root/home"'
        execution_start = HARNESS.rindex(
            "sudo env -i", 0, HARNESS.index(home_marker)
        )
        execution_end = HARNESS.index(
            '      "$staged_test_binary" --nocapture --test-threads=1',
            execution_start,
        )
        execution = HARNESS[execution_start:execution_end]
        for marker in (
            '--reuid "$workload_uid" --regid "$workload_gid"',
            '--groups "$workload_groups"',
            "--bounding-set=-all --inh-caps=-all --ambient-caps=-all",
            "--no-new-privs",
        ):
            self.assertIn(marker, execution)
        self.assertIn("workload_uid=65532", HARNESS)
        self.assertIn("workload_gid=65532", HARNESS)
        self.assertNotIn("--umask", execution)
        self.assertIn(
            "/bin/bash -c 'umask 0022; exec \"$@\"' contest-vertical",
            execution,
        )
        self.assertNotIn('eval ', execution)
        self.assertNotIn(
            '      "$test_binary" --nocapture --test-threads=1', execution
        )

    def test_cargo_test_binary_is_identity_checked_and_staged_read_only(self) -> None:
        for marker in (
            'cargo metadata --no-deps --format-version=1',
            '"services/orchestrator/backend/tests/contest_service_real_vertical_e2e.rs"',
            'os.path.commonpath((target_root, resolved)) != target_root',
            'r"contest_service_real_vertical_e2e-[0-9a-f]{16,64}"',
            'if len(set(executables)) != 1',
            'staged_test_binary="$run_root/contest-service-real-vertical-e2e"',
            'install -m 0555 -- "$test_binary" "$staged_test_binary"',
            '"$(stat -c \'%a\' "$staged_test_binary")" != 555',
            'cmp -s -- "$test_binary" "$staged_test_binary"',
        ):
            self.assertIn(marker, HARNESS)
        self.assertNotIn("chmod -R", HARNESS)
        self.assertNotIn("chmod o+x", HARNESS)

    def test_contract_inputs_are_explicitly_staged_without_checkout_traversal(self) -> None:
        expected_inputs = (
            "services/contest-service/ojos.service.yaml",
            "services/contest-service/api/openapi.yaml",
            "services/contest-service/config.schema.json",
            "services/contest-service/events/contest-created-v1.schema.json",
            "services/contest-service/frontend/user/manifest.json",
            "services/contest-service/frontend/admin/manifest.json",
            "platform/schemas/orchestrator/actions.yaml",
            "platform/schemas/orchestrator/forms.yaml",
            "platform/schemas/orchestrator/plans.yaml",
            "platform/schemas/orchestrator/results.yaml",
            "platform/schemas/orchestrator/errors.yaml",
        )
        allowlist_start = HARNESS.index("staged_inputs=(")
        allowlist_end = HARNESS.index("\n)", allowlist_start)
        allowlist = HARNESS[allowlist_start:allowlist_end]
        for relative_input in expected_inputs:
            self.assertEqual(allowlist.count(f'  "{relative_input}"'), 1)
        self.assertEqual(allowlist.count('  "'), len(expected_inputs))

        staging_end = HARNESS.index("wait_http() {")
        staging = HARNESS[allowlist_start:staging_end]
        for marker in (
            'staged_repo_root="$(realpath -e -- "$staged_repo_root")"',
            '"$staged_repo_root" != "$run_root/staged-repo"',
            '"$resolved_source" != "$source_input"',
            'install -D -m 0444 -- "$resolved_source" "$staged_input"',
            'chmod 0555 -- "$staged_directory"',
            '"$(find "$staged_repo_root" -type f -print | wc -l)" -ne "${#staged_inputs[@]}"',
            '"$(stat -c \'%u:%g:%a\' -- "$staged_input")" != "$invoking_uid:$invoking_gid:444"',
            'cmp -s -- "$source_input" "$staged_input"',
            '"$(stat -c \'%u:%g:%a\' -- "$staged_repo_root")" != "$invoking_uid:$invoking_gid:555"',
        ):
            self.assertIn(marker, staging)
        self.assertNotIn("chmod -R", staging)
        for marker in (
            'scratch_root="$staged_repo_root/.runtime"',
            'sudo install -d -o "$workload_uid" -g "$workload_gid" -m 0700',
            '"$scratch_root" != "$staged_repo_root/.runtime"',
            '"$(sudo stat -c \'%u:%g:%a\' -- "$scratch_root")" != "$workload_uid:$workload_gid:700"',
        ):
            self.assertIn(marker, HARNESS)

        execution_start = HARNESS.index("sudo env -i")
        execution = HARNESS[execution_start:]
        self.assertIn(
            '"OJOS_CONTEST_E2E_STAGED_REPO_ROOT=$staged_repo_root"', execution
        )
        self.assertIn(
            '"OJOS_CONTEST_E2E_CONTRACT_SOURCE=$staged_contract_source"', execution
        )

    def test_redis_host_and_runtime_boundaries_use_the_isolated_workflow_port(self) -> None:
        for marker in (
            'redis_host_port="${OJOS_CONTEST_E2E_REDIS_HOST_PORT:-}"',
            '"$redis_host_port" -lt 1',
            '"$redis_host_port" -gt 65535',
            'redis_host_url="redis://127.0.0.1:${redis_host_port}/0"',
            'redis_runtime_url="redis://${bridge_gateway}:${redis_host_port}/0"',
            'redis-cli -u "$redis_host_url" ping | grep -qx PONG',
            'docker run --rm --network bridge redis:8.8.0',
            'redis-cli -u "$redis_runtime_url" ping | grep -qx PONG',
            '"OJOS_CONTEST_E2E_REDIS_HOST_URL=$redis_host_url"',
            '"OJOS_CONTEST_E2E_REDIS_RUNTIME_URL=$redis_runtime_url"',
        ):
            self.assertIn(marker, HARNESS)
        self.assertNotIn("OJOS_CONTEST_E2E_REDIS_URL", HARNESS)
        self.assertNotIn("redis://${bridge_gateway}:6379/0", HARNESS)

        for marker in (
            'required_redis_url("OJOS_CONTEST_E2E_REDIS_HOST_URL", true)',
            '"OJOS_CONTEST_E2E_REDIS_RUNTIME_URL"',
            '.env("REDIS_URL", &config.redis_host_url)',
            '&config.redis_host_url,',
            'config.redis_runtime_url.clone()',
        ):
            self.assertIn(marker, RUST_DRIVER)

    def test_rust_driver_uses_only_canonical_staged_repo_inputs(self) -> None:
        for forbidden in ('env!("CARGO_MANIFEST_DIR")', "workspace_root()"):
            self.assertNotIn(forbidden, RUST_DRIVER)
            self.assertNotIn(forbidden, RUST_FIXTURES)
        for marker in (
            'canonical_env_directory("OJOS_CONTEST_E2E_STAGED_REPO_ROOT")',
            'canonical_env_file("OJOS_CONTEST_E2E_CONTRACT_SOURCE")',
            'contract_source.strip_prefix(&staged_repo_root)?',
            '== Path::new("services/contest-service/ojos.service.yaml")',
            'scratch_root.strip_prefix(&staged_repo_root)? == Path::new(".runtime")',
            'metadata.is_dir() && !metadata.file_type().is_symlink()',
            'metadata.is_file() && !metadata.file_type().is_symlink()',
            'ensure!(canonical == supplied, "{name} must already be canonical")',
            '&config.staged_repo_root,',
            '&config.contract_source,',
        ):
            self.assertIn(marker, RUST_DRIVER)
        self.assertIn(
            'compile(source).context("compile staged checked-in contest service contract")',
            RUST_FIXTURES,
        )
        self.assertIn(".strip_prefix(repo_root)?", RUST_FIXTURES)

    def test_privileged_cleanup_remains_confined_to_validated_mktemp_root(self) -> None:
        cleanup_start = HARNESS.index("cleanup() {")
        cleanup_end = HARNESS.index("\n}\ntrap cleanup EXIT", cleanup_start)
        cleanup = HARNESS[cleanup_start:cleanup_end]
        for marker in (
            'cleanup_root="$(realpath -e -- "$run_root" 2>/dev/null || true)"',
            '"$cleanup_root" == "$run_root"',
            '"$run_parent" == /tmp',
            '"$(realpath -e -- "$run_parent" 2>/dev/null || true)" == "$run_parent"',
            '"$(stat -c \'%u:%g:%a\' -- "$run_parent" 2>/dev/null || true)" == "0:0:1777"',
            '"$(dirname -- "$cleanup_root")" == "$run_parent"',
            '^ojos-contest-real-vertical\\.[A-Za-z0-9]{8}$',
            'sudo rm -rf -- "$cleanup_root"',
            "refusing unsafe privileged contest vertical cleanup",
        ):
            self.assertIn(marker, cleanup)


if __name__ == "__main__":
    unittest.main()
