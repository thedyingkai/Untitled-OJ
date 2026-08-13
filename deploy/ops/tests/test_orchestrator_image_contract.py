from __future__ import annotations

import pathlib
import unittest


REPO_ROOT = pathlib.Path(__file__).parents[3]
WORKSPACE = REPO_ROOT / "Cargo.toml"
DOCKERFILE = REPO_ROOT / "services" / "orchestrator" / "backend" / "Dockerfile"
AGENT_DOCKERFILE = REPO_ROOT / "services" / "orchestrator" / "agent" / "Dockerfile"


class OrchestratorImageContractTests(unittest.TestCase):
    def test_workspace_source_roots_exist_before_cargo_build(self) -> None:
        workspace = WORKSPACE.read_text(encoding="utf-8")
        dockerfile = DOCKERFILE.read_text(encoding="utf-8")
        build_index = dockerfile.index(
            'RUN OJOS_BUILD_COMMIT="$OJOS_BUILD_COMMIT" cargo build --release '
            "-p ojos-orchestrator-daemon"
        )
        build_input = dockerfile[:build_index]

        self.assertIn('"tools/ojos-service"', workspace)
        self.assertIn("COPY tools ./tools", build_input)
        self.assertIn("COPY services/orchestrator ./services/orchestrator", build_input)
        self.assertIn("COPY manager ./manager", build_input)
        self.assertIn("COPY platform ./platform", build_input)

    def test_agent_workspace_source_roots_exist_before_cargo_build(self) -> None:
        workspace = WORKSPACE.read_text(encoding="utf-8")
        dockerfile = AGENT_DOCKERFILE.read_text(encoding="utf-8")
        build_index = dockerfile.index(
            'RUN OJOS_BUILD_COMMIT="$OJOS_BUILD_COMMIT"'
        )
        build_input = dockerfile[:build_index]

        self.assertIn('"tools/ojos-service"', workspace)
        self.assertIn("COPY tools ./tools", build_input)
        self.assertIn("COPY services/orchestrator ./services/orchestrator", build_input)
        self.assertIn("COPY manager ./manager", build_input)
        self.assertIn("COPY platform ./platform", build_input)


if __name__ == "__main__":
    unittest.main()
