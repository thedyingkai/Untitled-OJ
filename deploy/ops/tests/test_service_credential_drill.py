import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
DRILL = ROOT / "deploy" / "ops" / "service-credential-drill.sh"


class ServiceCredentialDrillContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.drill = DRILL.read_text(encoding="utf-8")

    def test_final_readiness_follows_initdb_temporary_server_shutdown(self) -> None:
        helper = self.drill.split("wait_for_final_postgres() {", 1)[1].split(
            "\n}", 1
        )[0]
        marker = "PostgreSQL init process complete; ready for start up."
        self.assertIn(marker, self.drill)
        marker_gate = helper.index('grep -F "$postgres_init_complete"')
        pid_one_gate = helper.index("final_postgres_is_pid_one", marker_gate)
        final_readiness = helper.index(
            'pg_isready -U "$pg_user" -d "$pg_db"', pid_one_gate
        )
        self.assertLess(marker_gate, pid_one_gate)
        self.assertLess(pid_one_gate, final_readiness)
        self.assertIn("temporary server has completed its shutdown", helper)
        self.assertIn("take a fresh readiness", helper)
        self.assertIn(
            "cannot satisfy this identity gate",
            helper,
        )

        identity_helper = self.drill.split("final_postgres_is_pid_one() {", 1)[
            1
        ].split("\n}", 1)[0]
        self.assertIn('/proc/1/comm)" = postgres', identity_helper)

    def test_readiness_is_state_driven_bounded_and_fail_closed(self) -> None:
        helper = self.drill.split("wait_for_final_postgres() {", 1)[1].split(
            "\n}", 1
        )[0]
        self.assertIn("SECONDS + postgres_ready_timeout", helper)
        self.assertEqual(helper.count("while ((SECONDS < deadline))"), 2)
        self.assertGreaterEqual(helper.count("container_is_running"), 2)
        self.assertIn("return 1", helper)
        self.assertNotRegex(helper, r"(?m)^\s*sleep\s+[1-9][0-9]+\s*$")
        self.assertNotIn("continue-on-error", self.drill)

    def test_migrations_run_only_after_final_readiness_gate(self) -> None:
        gate = self.drill.index("\nwait_for_final_postgres\n")
        migration_loop = self.drill.index(
            'for migration in "$repo_root"/services/auth-service/migrations/*.up.sql'
        )
        self.assertLess(gate, migration_loop)
        self.assertNotRegex(
            self.drill[gate:migration_loop],
            r"pg_isready.*(?:\n.*){0,4}\bbreak\b",
        )


if __name__ == "__main__":
    unittest.main()
