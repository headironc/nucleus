"""Inspect OCI bytes and scan every historical layer without extracting paths.

Only numbered regular-file copies are materialized, inside TemporaryDirectory.
Archive names, links, devices and whiteouts are data, never host destinations.
"""

import hashlib
import gzip
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile


FORMATS = {
    "application/vnd.oci.image.manifest.v1+json": (
        "application/vnd.oci.image.config.v1+json",
        {"application/vnd.oci.image.layer.v1.tar", "application/vnd.oci.image.layer.v1.tar+gzip"}),
    "application/vnd.docker.distribution.manifest.v2+json": (
        "application/vnd.docker.container.image.v1+json",
        {"application/vnd.docker.image.rootfs.diff.tar", "application/vnd.docker.image.rootfs.diff.tar.gzip"}),
}


def check_platform(value):
    if "platform" in value and value["platform"] != {"os": "linux", "architecture": "amd64"}:
        raise ValueError("Conflicting image platform")


def compressed_layer(layer):
    return layer["mediaType"].endswith(("+gzip", ".gzip"))


def digest(stream):
    value = hashlib.sha256()
    while chunk := stream.read(1024 * 1024):
        value.update(chunk)
    return "sha256:" + value.hexdigest()


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def docker_archive(path, metadata_path=None, manifest_output=None):
    """Verify every Docker archive reference and independently derive config/diff IDs."""
    with tarfile.open(path, "r:*", ignore_zeros=True) as tar:
        members = {}
        seen = set()
        for member in tar:
            name = member.name.removeprefix("./")
            if name in seen:
                raise ValueError("Duplicate Docker archive member")
            seen.add(name)
            if member.isdir():
                continue
            if not member.isfile() or name in members or PurePosixPath(name).is_absolute() or ".." in PurePosixPath(name).parts:
                raise ValueError("Unsupported Docker archive member")
            members[name] = member
        with tar.extractfile(members["manifest.json"]) as stream:
            manifest = json.load(stream)
        if not isinstance(manifest, list) or len(manifest) != 1:
            raise ValueError("Exactly one Docker archive image required")
        entry = manifest[0]
        config_member = members[entry["Config"]]
        with tar.extractfile(config_member) as stream:
            config_digest = digest(stream)
        with tar.extractfile(members[entry["Config"]]) as stream:
            config = json.load(stream)
        if (config["os"], config["architecture"]) != ("linux", "amd64"):
            raise ValueError("Docker archive platform mismatch")
        diff_ids = config["rootfs"]["diff_ids"]
        if len(entry["Layers"]) != len(diff_ids):
            raise ValueError("Docker archive layer count mismatch")
        layers = []
        for index, name in enumerate(entry["Layers"]):
            member = members[name]
            with tar.extractfile(member) as stream:
                compressed_digest = digest(stream)
            with tar.extractfile(member) as stream:
                compressed = stream.read(2) == b"\x1f\x8b"
            with tar.extractfile(member) as stream:
                uncompressed = gzip.GzipFile(fileobj=stream) if compressed else stream
                if digest(uncompressed) != diff_ids[index]:
                    raise ValueError("Docker archive diff ID mismatch")
            layers.append({"path": name, "size": member.size, "digest": compressed_digest, "diffId": diff_ids[index]})
        metadata = json.loads(Path(metadata_path).read_text()) if metadata_path else None
        if metadata_path and not isinstance(metadata, dict):
            raise ValueError("Build metadata object required")
        if metadata is not None:
            direct = metadata.get("containerimage.config.digest")
            if "containerimage.config.digest" in metadata:
                if not isinstance(direct, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", direct) or direct != config_digest:
                    raise ValueError("Direct build config binding invalid or conflicting")
            elif "containerimage.descriptor" not in metadata:
                raise ValueError("Direct build config binding missing")
        if metadata is not None and "containerimage.descriptor" in metadata:
            descriptor = metadata["containerimage.descriptor"]
            manifest_digest = metadata["containerimage.digest"]
            if (not isinstance(descriptor, dict) or not isinstance(manifest_digest, str) or len(manifest_digest) != 71
                    or not manifest_digest.startswith("sha256:")
                    or any(c not in "0123456789abcdef" for c in manifest_digest[7:])
                    or descriptor["digest"] != manifest_digest
                    or descriptor["mediaType"] not in ("application/vnd.oci.image.manifest.v1+json",
                                                       "application/vnd.docker.distribution.manifest.v2+json")):
                raise ValueError("Invalid build manifest descriptor")
            name = "blobs/sha256/" + manifest_digest[7:]
            if name in members:
                def blob(expected):
                    member = members["blobs/sha256/" + expected["digest"][7:]]
                    with tar.extractfile(member) as stream:
                        if member.size != expected["size"] or digest(stream) != expected["digest"]:
                            raise ValueError("Build manifest blob size/digest mismatch")
                    return member

                member = blob(descriptor)
                with tar.extractfile(member) as stream:
                    raw = stream.read()
                manifest = json.loads(raw)
                if manifest.get("schemaVersion") != 2 or manifest.get("mediaType") != descriptor["mediaType"]:
                    raise ValueError("Unsupported build manifest")
                check_platform(descriptor)
                check_platform(manifest)
                config_type, layer_types = FORMATS[descriptor["mediaType"]]
                config_descriptor = manifest["config"]
                if (config_descriptor["mediaType"] != config_type
                        or blob(config_descriptor) != config_member
                        or config_descriptor["digest"] != config_digest):
                    raise ValueError("Build manifest config conflict")
                check_platform(config_descriptor)
                if len(manifest["layers"]) != len(layers):
                    raise ValueError("Build manifest layer count mismatch")
                for expected, actual in zip(manifest["layers"], layers):
                    if (expected["mediaType"] not in layer_types or "urls" in expected
                            or blob(expected) != members[actual["path"]]
                            or expected["digest"] != actual["digest"] or expected["size"] != actual["size"]):
                        raise ValueError("Build manifest layer conflict")
                    check_platform(expected)
                if "index.json" in members:
                    with tar.extractfile(members["index.json"]) as stream:
                        index = json.load(stream)
                    if index.get("schemaVersion") != 2 or len(index["manifests"]) != 1:
                        raise ValueError("Ambiguous Docker archive index")
                    wrapper = index["manifests"][0]
                    if any(wrapper[key] != descriptor[key] for key in ("digest", "size", "mediaType")):
                        raise ValueError("Docker archive index conflict")
                    check_platform(wrapper)
                    check_platform(index)
                Path(manifest_output).write_bytes(raw)
            elif "containerimage.config.digest" not in metadata or "index.json" in members:
                raise ValueError("Exact build manifest missing")
        return {"configDigest": config_digest, "configSize": config_member.size,
                "platform": {"os": "linux", "architecture": "amd64"},
                "config": config, "layers": layers}


class OCI:
    def __init__(self, path):
        self.archive = tarfile.open(path, "r:*", ignore_zeros=True)
        self.members = {}
        for member in self.archive:
            name = member.name.removeprefix("./")
            if name in self.members:
                raise ValueError("Duplicate OCI archive member")
            if member.isdir():
                continue
            if not member.isfile() or not (
                name in ("oci-layout", "index.json")
                or (name.startswith("blobs/sha256/") and len(name) == 77
                    and all(c in "0123456789abcdef" for c in name[13:]))
            ):
                raise ValueError("Unexpected OCI archive member")
            self.members[name] = member

    def file(self, name):
        return self.archive.extractfile(self.members[name])

    def json(self, name):
        with self.file(name) as stream:
            return json.load(stream)

    def blob(self, descriptor):
        sha = descriptor["digest"]
        if (not isinstance(sha, str) or len(sha) != 71 or not sha.startswith("sha256:")
                or any(c not in "0123456789abcdef" for c in sha[7:])):
            raise ValueError("Invalid OCI digest")
        name = "blobs/sha256/" + sha[7:]
        member = self.members[name]
        if member.size != descriptor["size"]:
            raise ValueError("OCI blob size mismatch")
        with self.file(name) as stream:
            if digest(stream) != sha:
                raise ValueError("OCI blob digest mismatch")
        return name

    def identity(self):
        if self.json("oci-layout") != {"imageLayoutVersion": "1.0.0"}:
            raise ValueError("Unsupported OCI layout")
        index = self.json("index.json")
        if index.get("schemaVersion") != 2 or len(index["manifests"]) != 1:
            raise ValueError("Exactly one OCI manifest required")
        descriptor = index["manifests"][0]
        if descriptor["mediaType"] not in FORMATS:
            raise ValueError("Supported single image manifest required")
        config_type, layer_types = FORMATS[descriptor["mediaType"]]
        manifest = self.json(self.blob(descriptor))
        if (manifest.get("schemaVersion") != 2 or manifest.get("mediaType") != descriptor["mediaType"]
                or manifest["config"]["mediaType"] != config_type):
            raise ValueError("Invalid OCI manifest/config")
        config = self.json(self.blob(manifest["config"]))
        if (config["os"], config["architecture"]) != ("linux", "amd64"):
            raise ValueError("Only linux/amd64 is accepted")
        for value in (index, descriptor, manifest, manifest["config"]):
            check_platform(value)
        user = config["config"].get("User", "")
        if not user or user.split(":")[0] in ("root", "0"):
            raise ValueError("Image default user must be non-root")
        layers = manifest["layers"]
        if len(layers) != len(config["rootfs"]["diff_ids"]):
            raise ValueError("OCI layer/diff ID count mismatch")
        for number, layer in enumerate(layers):
            if layer["mediaType"] not in layer_types or "urls" in layer:
                raise ValueError("Unsupported OCI layer encoding")
            check_platform(layer)
            self.blob(layer)
            with self.file("blobs/sha256/" + layer["digest"][7:]) as raw:
                stream = gzip.GzipFile(fileobj=raw) if compressed_layer(layer) else raw
                if digest(stream) != config["rootfs"]["diff_ids"][number]:
                    raise ValueError("OCI uncompressed diff ID mismatch")
        referenced = {"oci-layout", "index.json", "blobs/sha256/" + descriptor["digest"][7:],
                      "blobs/sha256/" + manifest["config"]["digest"][7:]}
        referenced.update("blobs/sha256/" + layer["digest"][7:] for layer in layers)
        if set(self.members) != referenced:
            raise ValueError("Unreferenced OCI archive bytes")
        return {"manifest": descriptor, "config": manifest["config"], "layers": layers,
                "platform": {"os": "linux", "architecture": "amd64"}}, config


def scan_oci(path, output, scanner, config_path, accepted_path=None):
    output = Path(output)
    output.mkdir(exist_ok=True)
    oci = OCI(path)
    try:
        identity, config = oci.identity()
        write_json(output / "oci.json", identity)
        write_json(output / "config.json", config)
        write_json(output / "history.json", config.get("history", []))
        reports = []
        findings = []
        accepted = []
        pending = []
        used = {}
        judgments = {}
        if accepted_path:
            policy = json.loads(Path(accepted_path).read_text())
            for filename, expected in ((scanner, policy["scannerSha256"]), (config_path, policy["configurationSha256"])):
                with open(filename, "rb") as stream:
                    if digest(stream) != "sha256:" + expected:
                        raise ValueError("Accepted findings require the original detector and configuration")
            for judgment in policy["findings"]:
                key = (judgment["path"], judgment["sourceSha256"], judgment["rule"], judgment["line"])
                if (key in judgments or not judgment["reason"] or not judgment["path"] or not judgment["rule"]
                        or type(judgment["line"]) is not int or judgment["line"] <= 0
                        or type(judgment["count"]) is not int or judgment["count"] <= 0
                        or any(not re.fullmatch(r"[0-9a-f]{64}", judgment[field]) for field in ("sourceSha256", "valueSha256"))):
                    raise ValueError("Ambiguous or unexplained accepted finding")
                judgments[key] = judgment
        with tempfile.TemporaryDirectory(prefix="nucleus-layer-") as scratch:
            scratch = Path(scratch)

            def scan(directory, label, mapping):
                report = scratch / "report.json"
                report.unlink(missing_ok=True)
                result = subprocess.run([
                    scanner, "dir", str(directory), "--config", config_path,
                    "--no-banner", "--redact", "--ignore-gitleaks-allow",
                    "--max-target-megabytes=0", "--max-archive-depth=2", "--max-decode-depth=2",
                    "--report-format=json", "--report-path", str(report),
                    "--log-level=error",
                ], capture_output=True, text=True)
                if result.returncode not in (0, 1) or not report.is_file():
                    raise ValueError("Gitleaks scan failed (raw output withheld)")
                matches = json.loads(report.read_text())
                if bool(matches) != (result.returncode == 1):
                    raise ValueError("Gitleaks result/report conflict")
                for match in matches:
                    # Keep only redacted diagnostics; no Match/Secret/Line or raw stderr.
                    # Gitleaks may append nested archive paths. Resolve only
                    # our minted outer filename; never echo nested raw names.
                    file = next((key for key in mapping if key in match["File"].replace("\\", "/").split("/")
                                 or ("/" + key + "!") in match["File"]), None)
                    if file is None:
                        raise ValueError("Unmapped redacted Gitleaks finding")
                    finding = {"layer": label, "file": file,
                                     "path": mapping.get(file), "rule": match["RuleID"],
                                     "startLine": match["StartLine"], "secret": "REDACTED"}
                    with (directory / file).open("rb") as source:
                        source_sha = digest(source)[7:]
                    key = (mapping[file], source_sha, match["RuleID"], match["StartLine"])
                    # Full source bytes plus the exact detector/config preserve
                    # the original value judgment without unredacting reports.
                    if key in judgments:
                        used[key] = used.get(key, 0) + 1
                    pending.append((finding, key))
                reports.append({"layer": label, "exit": result.returncode,
                                "files": len(mapping), "detected": len(matches)})

            meta = scratch / "metadata"
            meta.mkdir()
            write_json(meta / "config.json", config)
            scan(meta, "config-and-history", {"config.json": "OCI config and complete history"})
            with (output / "layers.jsonl").open("w") as inventory:
                for number, layer in enumerate(identity["layers"]):
                    directory = scratch / "files"
                    directory.mkdir()
                    mapping = {}
                    metadata = (directory / "metadata.jsonl").open("w")
                    layer_name = "blobs/sha256/" + layer["digest"][7:]
                    with oci.file(layer_name) as compressed:
                        with tarfile.open(fileobj=compressed, mode="r|*", ignore_zeros=True) as tar:
                            for index, member in enumerate(tar):
                                name = member.name
                                safe = not PurePosixPath(name).is_absolute() and ".." not in PurePosixPath(name).parts
                                entry = {"layer": number, "digest": layer["digest"], "entry": index,
                                         "path": name, "size": member.size, "mode": member.mode,
                                         "uid": member.uid, "gid": member.gid,
                                         "type": member.type.decode("ascii", "backslashreplace"),
                                         "link": member.linkname, "safePath": safe,
                                         "scan": "metadata-only"}
                                if member.isfile():
                                    suffix = "".join(PurePosixPath(name).suffixes)
                                    # Preserve recognizable archive suffixes for nested scanning.
                                    suffix = suffix if suffix in (".zip", ".tar", ".tar.gz", ".tgz", ".gz", ".bz2", ".xz", ".7z", ".jar") else ""
                                    file = f"{index:09d}{suffix}"
                                    mapping[file] = name
                                    with tar.extractfile(member) as source, (directory / file).open("xb") as dest:
                                        shutil.copyfileobj(source, dest, 1024 * 1024)
                                    with (directory / file).open("rb") as source:
                                        entry["sha256"] = digest(source)
                                    entry["scan"] = "gitleaks-dir-all-bytes"
                                    if (directory / file).stat().st_size != member.size:
                                        raise ValueError("Truncated layer file")
                                inventory.write(json.dumps(entry) + "\n")
                                metadata.write(json.dumps({key: entry[key] for key in ("path", "link", "type")}) + "\n")
                    metadata.close()
                    mapping["metadata.jsonl"] = "all layer entry paths, links and types"
                    scan(directory, number, mapping)
                    shutil.rmtree(directory)
            for finding, key in pending:
                judgment = judgments.get(key)
                if judgment and used[key] == judgment["count"]:
                    accepted.append({**finding, "evidence": judgment})
                else:
                    findings.append(finding)
            for key, judgment in judgments.items():
                if key not in used:
                    findings.append({"path": judgment["path"], "rule": "accepted-finding-count", "expected": judgment["count"], "actual": 0})
            for report in reports:
                report["accepted"] = sum(item["layer"] == report["layer"] for item in accepted)
                report["unresolved"] = sum(item.get("layer") == report["layer"] for item in findings)
            policy_sha = None
            if accepted_path:
                with open(accepted_path, "rb") as stream:
                    policy_sha = digest(stream)
            write_json(output / "scan.json", {
                "python": platform.python_version(), "scans": reports, "findings": findings, "accepted": accepted,
                "detected": len(pending), "unresolved": len(findings), "policySha256": policy_sha,
                "limitations": ["Pattern matching is not proof of absence of secrets or vulnerabilities.",
                                "All regular file bytes read; links/devices/whiteouts inspected as metadata only.",
                                "Nested archives and encoded strings scanned to depth 2; deeper/encrypted/unsupported formats are not expanded.",
                                "Binary bytes are passed to Gitleaks; arbitrary binary secrets need not match text rules."],
            })
        if findings:
            raise ValueError("Unresolved secret patterns; see redacted scan.json")
        return identity
    finally:
        oci.archive.close()


if __name__ == "__main__":
    command, archive, *args = sys.argv[1:]
    if command == "docker" and len(args) in (0, 2):
        print(json.dumps(docker_archive(archive, *args)))
    elif command == "inspect" and len(args) in (0, 1, 2):
        oci = OCI(archive)
        try:
            identity, config = oci.identity()
            for record_path in args:
                record = json.loads(Path(record_path).read_text())
                if (record["configDigest"] != identity["config"]["digest"]
                        or record["config"] != config
                        or record["platform"] != identity["platform"]
                        or [layer["diffId"] for layer in record["layers"]] != config["rootfs"]["diff_ids"]):
                    raise ValueError("Docker record conflicts with actual OCI config bytes")
            print(json.dumps(identity))
        finally:
            oci.archive.close()
    elif command == "scan" and len(args) in (3, 4):
        scan_oci(archive, *args)
    else:
        raise ValueError("Usage: halo-image-oci.py inspect ARCHIVE | scan ARCHIVE OUTPUT GITLEAKS CONFIG")
