"""Exercise the dependency gate through real Cargo workspaces, without a registry."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


CHECK = Path(__file__).with_name("check_dependencies.py").resolve()


class DependencyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="nucleus dependency test ")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.workspace = self.directory / "workspace"
        self.workspace.mkdir()
        # Temporary workspaces must use the repository's Cargo, not the host default.
        (self.workspace / "rust-toolchain.toml").write_bytes(
            (CHECK.parent.parent / "rust-toolchain.toml").read_bytes()
        )
        self.manifest = self.workspace / "Cargo.toml"
        self.manifest.write_text(
            '[workspace]\nresolver = "3"\nmembers = ["nucleus", "domain", "platform", "deployment", "fixture"]\n',
            encoding="utf-8",
        )
        for name in ["nucleus", "domain", "platform", "deployment", "fixture"]:
            self.package(self.workspace / name, name)
        self.append("nucleus", '[dependencies]\ndomain = { path = "../domain" }\nplatform = { path = "../platform" }\n')
        self.append("domain", '[dependencies]\nplatform = { path = "../platform" }\n[features]\ntest-support = []\n')
        self.append("deployment", '[dependencies]\ndomain = { path = "../domain" }\nplatform = { path = "../platform" }\n')
        for name in ["platform", "domain", "deployment"]:
            self.append(name, '[dev-dependencies]\nfixture = { path = "../fixture" }\n')
        self.append("nucleus", '[dev-dependencies]\nfixture = { path = "../fixture" }\ndomain = { path = "../domain", features = ["test-support"] }\n')

    def package(self, path, name):
        (path / "src").mkdir(parents=True)
        (path / "src/lib.rs").write_text("", encoding="utf-8")
        (path / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2024"\n',
            encoding="utf-8",
        )

    def append(self, package, contents):
        with (self.workspace / package / "Cargo.toml").open("a", encoding="utf-8") as target:
            target.write(contents)

    def external(self, name, contents=""):
        path = self.directory / name
        self.package(path, name)
        with (path / "Cargo.toml").open("a", encoding="utf-8") as target:
            target.write(contents)
        return path

    def run_check(self, *, lock=True):
        environment = {**os.environ, "CARGO_NET_OFFLINE": "true"}
        if lock:
            subprocess.run(
                ["cargo", "generate-lockfile", "--manifest-path", str(self.manifest), "--offline"],
                cwd=self.workspace, env=environment, check=True,
                capture_output=True, text=True, timeout=30,
            )
        inputs = {path: path.read_bytes() for path in self.directory.rglob("Cargo.*")}
        result = subprocess.run(
            [sys.executable, "-B", str(CHECK), "--manifest-path", str(self.manifest)],
            cwd=self.directory, env=environment, capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(inputs, {path: path.read_bytes() for path in self.directory.rglob("Cargo.*")})
        self.assertFalse(list(self.directory.rglob("target")), "the gate must not compile or execute build scripts")
        return result

    def rejected(self, diagnostic):
        result = self.run_check()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(diagnostic, result.stderr)
        self.assertIn("docs/engineering/rust.md#dependency-boundaries", result.stderr)

    def test_current_directions_and_dev_features_are_allowed(self):
        self.external("rcgen")
        self.external("refinery")
        self.append("fixture", '[dependencies]\nrcgen = { path = "../../rcgen" }\n')
        self.append("deployment", '[dependencies.refinery]\npath = "../../refinery"\n')
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Dependency boundaries passed", result.stdout)

    def test_normal_test_dependency_is_rejected(self):
        self.append("nucleus", '[dependencies.fixture]\npath = "../fixture"\n')
        self.rejected("nucleus normal dependency: fixture crosses")

    def test_build_test_dependency_is_rejected(self):
        self.append("domain", '[build-dependencies]\nfixture = { path = "../fixture" }\n')
        self.rejected("domain build dependency: fixture crosses")

    def test_transitive_build_dependency_is_rejected(self):
        self.external("rcgen")
        path = self.external("helper", '[build-dependencies]\nrcgen = { path = "../rcgen" }\n')
        (path / "build.rs").write_text('fn main() { panic!("must never execute"); }\n', encoding="utf-8")
        self.append("nucleus", '[dependencies.helper]\npath = "../../helper"\n')
        self.rejected("nucleus production graph: rcgen crosses")

    def test_dormant_optional_test_tool_is_rejected(self):
        self.external("sqlx")
        self.append("nucleus", '[dependencies.sqlx]\npath = "../../sqlx"\noptional = true\n')
        self.rejected("nucleus normal dependency: sqlx crosses")

    def test_non_host_transitive_test_dependency_is_rejected(self):
        self.external("rcgen")
        self.external("helper", '[target.\'cfg(target_os = "none")\'.dependencies]\nrcgen = { path = "../rcgen" }\n')
        self.append("nucleus", '[dependencies.helper]\npath = "../../helper"\n')
        self.rejected("nucleus production graph: rcgen crosses")

    def test_renamed_migration_dependency_is_rejected(self):
        self.append("nucleus", '[dependencies.maintenance]\npackage = "deployment"\npath = "../deployment"\n')
        self.rejected("nucleus normal dependency: deployment crosses")

    def test_deployment_cannot_depend_on_the_online_entrypoint(self):
        self.append("deployment", '[dependencies.nucleus]\npath = "../nucleus"\n')
        self.rejected("deployment normal dependency: nucleus crosses")

    def test_fixture_cannot_depend_on_business_code(self):
        self.append("fixture", '[dependencies]\ndomain = { path = "../domain" }\n')
        self.rejected("fixture normal dependency: domain crosses")

    def test_direct_test_support_feature_is_rejected(self):
        self.append("deployment", '[build-dependencies]\ndomain = { path = "../domain", features = ["test-support"] }\n')
        self.rejected("deployment build dependency: domain/test-support belongs only in dev-dependencies")

    def test_default_feature_forwarding_is_rejected(self):
        self.append("nucleus", '[features]\ndefault = ["testing"]\ntesting = ["domain/test-support"]\n')
        self.rejected("nucleus production graph: domain/test-support is enabled")

    def test_domain_default_test_feature_is_rejected(self):
        path = self.workspace / "domain/Cargo.toml"
        path.write_text(path.read_text().replace(
            'test-support = []', 'test-support = []\ndefault = ["test-support"]'
        ), encoding="utf-8")
        self.rejected("domain production graph: domain/test-support is enabled")

    def test_crypto_provider_is_rejected_even_in_dev_dependencies(self):
        for name in ["aws-lc-rs", "aws-lc-sys"]:
            self.external(name)
            self.append("nucleus", f'[dev-dependencies.{name}]\npath = "../../{name}"\n')
        result = self.run_check()
        self.assertEqual(result.returncode, 1, result.stderr)
        for name in ["aws-lc-rs", "aws-lc-sys"]:
            self.assertIn(f"Cargo.lock: forbidden crypto provider {name}", result.stderr)

    def test_new_member_needs_an_explicit_boundary(self):
        self.package(self.workspace / "unknown", "unknown")
        self.manifest.write_text(self.manifest.read_text().replace('"fixture"]', '"fixture", "unknown"]'), encoding="utf-8")
        self.rejected("unknown: define its architecture dependency boundary")

    def test_missing_lockfile_fails_without_creating_one(self):
        result = self.run_check(lock=False)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("Dependency check could not complete", result.stderr)
        self.assertFalse((self.workspace / "Cargo.lock").exists())

    def test_cargo_failure_does_not_report_success(self):
        self.assertEqual(self.run_check().returncode, 0)
        self.append("nucleus", '[dependencies.missing]\npath = "../missing"\n')
        result = self.run_check(lock=False)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("Dependency check could not inspect Cargo inputs", result.stderr)
        self.assertNotIn("Dependency boundaries passed", result.stdout)


if __name__ == "__main__":
    unittest.main()
