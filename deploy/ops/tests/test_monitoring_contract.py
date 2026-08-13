import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
MONITORING = ROOT / "deploy" / "ops" / "monitoring"


class MonitoringContractTest(unittest.TestCase):
    def text(self, name):
        return (MONITORING / name).read_text(encoding="utf-8")

    def test_prometheus_uses_credentialed_tls_http_sd(self):
        config = self.text("prometheus.yml")
        self.assertIn(
            "https://orchestrator:8090/internal/v1/observability/metrics/targets",
            config,
        )
        self.assertIn(
            "https://orchestrator:8090/internal/v1/observability/health/targets",
            config,
        )
        self.assertGreaterEqual(config.count("http_sd_configs:"), 2)
        self.assertGreaterEqual(config.count("authorization:"), 3)
        self.assertGreaterEqual(
            config.count(
                "credentials_file: /etc/prometheus/secrets/"
                "orchestrator-observability-token"
            ),
            3,
        )
        self.assertGreaterEqual(
            config.count("ca_file: /etc/prometheus/tls/orchestrator-ca.crt"), 3
        )
        self.assertIn("server_name: orchestrator", config)
        self.assertIn("ca_file: /etc/prometheus/tls/gateway-ca.crt", config)
        self.assertIn("metrics_path: /internal/v1/observability/metrics", config)

    def test_production_monitoring_has_no_legacy_business_dns_or_db_exporters(self):
        prometheus = self.text("prometheus.yml")
        compose = self.text("docker-compose.yml")
        for target in (
            "gateway:8080",
            "auth-service:8081",
            "judge-api:8082",
            "problem-service:8083",
            "user-service:8084",
            "storage-service:8085",
        ):
            self.assertNotIn(target, prometheus)
        rule_tests = self.text("alert-tests.yml")
        self.assertNotIn("gateway:8080", rule_tests)
        self.assertNotIn('database="judge"', rule_tests)
        for service in ("auth", "problem", "judge", "user"):
            self.assertNotIn(f"postgres-exporter-{service}", compose)
        self.assertIn("postgres-exporter-orchestrator", compose)
        self.assertNotIn("AUTH_DATABASE_URL", compose)
        self.assertNotIn("PROBLEM_DATABASE_URL", compose)
        self.assertNotIn("JUDGE_DATABASE_URL", compose)
        self.assertNotIn("USER_DATABASE_URL", compose)
        self.assertFalse((MONITORING / "postgres-judge-queries.yml").exists())

    def test_discovery_source_requires_active_head_and_current_runtime_evidence(self):
        source = (
            ROOT
            / "services"
            / "orchestrator"
            / "backend"
            / "src"
            / "observability_discovery.rs"
        ).read_text(encoding="utf-8")
        for invariant in (
            "ContributionRevisionStatusV1::Active",
            "head.active_revision_id()",
            "runtime_with_current_evidence",
            "RuntimeDesiredState::Running",
            "RuntimeObservedState::Running",
            'health.eq_ignore_ascii_case("HEALTHY")',
            "runtime.instance.runtime_attested",
            "platform.contract_digest.as_str()",
            "parse_endpoint_id",
        ):
            self.assertIn(invariant, source)
        self.assertIn('"127.0.0.1:1"', source)
        self.assertNotIn("deployment_id}-", source)
        self.assertIn("CONTRIBUTION_ACK_VERIFIER_ENVS", source)
        self.assertIn("constant_time_hash_eq(observability_hash, &verifier)", source)

    def test_explicit_gateway_target_is_external_tls_and_blackbox_verified(self):
        source = (
            ROOT
            / "services"
            / "orchestrator"
            / "backend"
            / "src"
            / "observability_discovery.rs"
        ).read_text(encoding="utf-8")
        self.assertIn("ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN", source)
        self.assertIn("credential-free external HTTPS origin", source)
        self.assertIn('"explicit-platform"', source)
        blackbox = self.text("blackbox.yml")
        self.assertIn("http_2xx_gateway_tls", blackbox)
        self.assertIn("/etc/blackbox-exporter/tls/gateway-ca.crt", blackbox)
        self.assertIn("follow_redirects: false", blackbox)

    def test_observability_secret_mounts_are_private_read_only_copies(self):
        compose = self.text("docker-compose.yml")
        self.assertIn("PROMETHEUS_ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE", compose)
        self.assertIn(
            "target: /etc/prometheus/secrets/orchestrator-observability-token",
            compose,
        )
        self.assertRegex(
            compose,
            r"(?s)target: /etc/prometheus/secrets/orchestrator-observability-token"
            r"\s+read_only: true",
        )

    def test_judge_exports_authoritative_local_db_metrics_fail_visible(self):
        collector = (
            ROOT / "services" / "judge-api" / "internal" / "svc" / "queue_metrics.go"
        ).read_text(encoding="utf-8")
        main = (ROOT / "services" / "judge-api" / "judgeapi.go").read_text(
            encoding="utf-8"
        )
        self.assertIn("ojos_judge_workers_online", collector)
        self.assertIn("ojos_judge_queue_pending_tasks", collector)
        self.assertIn("ojos_judge_queue_metrics_collection_error", collector)
        self.assertIn("context.WithTimeout", collector)
        self.assertIn("collector.service.DB.QueryRow", collector)
        self.assertIn("status = 'PENDING'", collector)
        self.assertIn("prometheus.MustRegister(svc.NewJudgeQueueCollector(svcCtx))", main)

    def test_alerts_reference_dynamic_and_fail_visible_metrics(self):
        alerts = self.text("alerts.yml")
        self.assertNotIn("vector(", alerts)
        for metric in (
            "ojos_orchestrator_observability_target_ready",
            "ojos_orchestrator_observability_discovery_collection_error",
            "ojos_judge_workers_online",
            "ojos_judge_queue_pending_tasks",
            "ojos_judge_queue_metrics_collection_error",
        ):
            self.assertIn(metric, alerts)
        for alert in (
            "OJOSActiveRuntimeUnhealthy",
            "OJOSObservabilityDiscoveryFailed",
            "OJOSJudgeWorkerOffline",
            "OJOSJudgeQueueBacklog",
            "OJOSJudgeMetricsUnavailable",
        ):
            self.assertIn(alert, alerts)

        drill = (ROOT / "deploy" / "ops" / "alert-firing-drill.sh").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("vector(", drill)
        self.assertIn("test rules alert-tests.yml", drill)
        self.assertIn("write_drill_state 503 0 101 0 true", drill)
        self.assertIn('write_drill_state 200 1 0 "$(date -u +%s)" false', drill)

    def test_rule_tests_cover_firing_and_resolution(self):
        rule_tests = self.text("alert-tests.yml")
        self.assertRegex(
            rule_tests,
            r"(?s)alertname: OJOSHighHTTP5xxRate.*?service: orchestrator.*?"
            r"alertname: OJOSHighHTTP5xxRate\s+exp_alerts: \[\]",
        )
        for alert in (
            "OJOSActiveRuntimeUnhealthy",
            "OJOSObservabilityDiscoveryFailed",
            "OJOSJudgeMetricsUnavailable",
        ):
            self.assertIn(f"alertname: {alert}", rule_tests)

    def test_node_exporter_reads_backup_textfile_directory(self):
        compose = self.text("docker-compose.yml")
        self.assertIn(
            "--collector.textfile.directory=/var/lib/node-exporter/textfile",
            compose,
        )
        self.assertIn("/var/lib/node-exporter/textfile:ro", compose)


if __name__ == "__main__":
    unittest.main()
