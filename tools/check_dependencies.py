"""Check the dependency boundaries owned by docs/engineering/rust.md.

Cargo resolves dependencies and features; this check does not parse manifests or
reimplement feature resolution. No build scripts, binaries or services are run.
"""

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tomllib


# The production directions in docs/architecture.md#crate-与模块组织.
DEPENDENCIES = {
    "nucleus": {"domain", "platform"},
    "domain": {"platform"},
    "platform": set(),
    "deployment": {"domain", "platform"},
    "fixture": set(),
}
TEST_TOOLS = {
    "fixture", "postgresql_embedded", "postgresql_archive",
    "postgresql_commands", "rcgen", "sqlx",
}
MIGRATION_TOOLS = {"deployment", "refinery", "refinery-core"}
CRYPTO_PROVIDERS = {"aws-lc-rs", "aws-lc-sys"}


def cargo(manifest: Path, *arguments: str) -> str:
    return subprocess.run(
        ["cargo", *arguments, "--manifest-path", str(manifest), "--locked", "--color", "never"],
        cwd=manifest.parent,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=120,
    ).stdout


def forbidden(owner: str, dependency: str) -> bool:
    if dependency in DEPENDENCIES and dependency not in DEPENDENCIES[owner]:
        return True
    if owner != "fixture" and dependency in TEST_TOOLS:
        return True
    return owner != "deployment" and dependency in MIGRATION_TOOLS


def check(manifest: Path) -> list[str]:
    workspace = json.loads(cargo(manifest, "metadata", "--format-version", "1", "--no-deps"))
    members = [
        package for package in workspace["packages"]
        if package["id"] in workspace["workspace_members"]
    ]
    if not members:
        raise ValueError("Cargo reported no workspace members")

    failures = set()
    with (Path(workspace["workspace_root"]) / "Cargo.lock").open("rb") as source:
        locked = {package["name"] for package in tomllib.load(source)["package"]}
    for name in sorted(locked & CRYPTO_PROVIDERS):
        failures.add(f"Cargo.lock: forbidden crypto provider {name}; keep the graph ring-only")

    for member in members:
        owner = member["name"]
        if owner not in DEPENDENCIES:
            failures.add(f"{owner}: define its architecture dependency boundary before adding a workspace member")
            continue

        # Include dormant optional and target-specific declarations. Cargo metadata
        # reports the actual package name even when the dependency is renamed.
        for dependency in member["dependencies"]:
            if dependency["kind"] == "dev":
                continue
            name = dependency["name"]
            location = f"{owner} {dependency['kind'] or 'normal'} dependency"
            if dependency["target"]:
                location += f" ({dependency['target']})"
            if forbidden(owner, name):
                failures.add(f"{location}: {name} crosses the production boundary")
            if name == "domain" and "test-support" in dependency["features"]:
                failures.add(f"{location}: domain/test-support belongs only in dev-dependencies")

        graph = cargo(
            manifest, "tree", "--package", member["id"], "--edges", "normal,build",
            "--target", "all", "--prefix", "none", "--format", "{p}|{f}",
        )
        if not graph.strip():
            raise ValueError(f"Cargo reported an empty production graph for {owner}")
        for line in graph.splitlines():
            package, separator, features = line.rpartition("|")
            if not separator or not package.strip():
                raise ValueError(f"Cargo reported an unrecognized tree entry for {owner}")
            name = package.split()[0]
            # The root is part of cargo tree output; check its features as well.
            if name != owner and forbidden(owner, name):
                failures.add(f"{owner} production graph: {name} crosses the production boundary")
            enabled = features.removesuffix(" (*)").strip().split(",")
            if name == "domain" and "test-support" in enabled:
                failures.add(f"{owner} production graph: domain/test-support is enabled")

    return sorted(failures)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest-path", type=Path,
        default=Path(__file__).resolve().parent.parent / "Cargo.toml",
    )
    arguments = parser.parse_args()
    try:
        failures = check(arguments.manifest_path.resolve())
    except subprocess.CalledProcessError as error:
        print(f"Dependency check could not inspect Cargo inputs:\n{error.stderr.strip()}", file=sys.stderr)
        return 2
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        print(f"Dependency check could not complete: {error}", file=sys.stderr)
        return 2
    if failures:
        for failure in failures:
            print(failure, file=sys.stderr)
        print("See docs/engineering/rust.md#dependency-boundaries and docs/architecture.md#crate-与模块组织.", file=sys.stderr)
        return 1
    print("Dependency boundaries passed (declarations, locked crypto providers, production graphs and features).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
