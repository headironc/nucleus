// Opt-in real-container lifecycle acceptance. Ordinary maintenance regression
// does not silently require Docker, network access or a multi-GB local image.
import { expect, test } from "bun:test";
import { spawn, spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createDocker, currentIdentity, prepareAccounts } from "./halo-local";
import { prepareHaloDependencies } from "./halo-image-runtime";

const image = process.env.NUCLEUS_IMAGE_RUNTIME_TEST_IMAGE;
const root = resolve(import.meta.dir, "..");
function docker(args: string[]) {
  const result = spawnSync("docker", args, {
    encoding: "utf8",
    timeout: 70_000,
  });
  if (result.error || result.status !== 0)
    throw new Error(`Docker fixture failed: ${result.error ?? result.stderr}`);
  return result.stdout;
}

for (const fault of [
  "pg",
  "browser",
  "consumer",
  "cancel",
  "root",
  "inputs",
  "node",
] as const) {
  test.skipIf(!image)(
    `real image refuses ${fault} and settles its private resources`,
    async () => {
      if (!/^sha256:[0-9a-f]{64}$/.test(image!))
        throw new Error("Use an exact already-restored image ID");
      const identity = currentIdentity();
      const scratch = mkdtempSync(join(tmpdir(), "nucleus-runtime-negative-"));
      const evidenceRoot = process.env.NUCLEUS_IMAGE_RUNTIME_TEST_EVIDENCE;
      if (!evidenceRoot)
        throw new Error("Retained lifecycle evidence path required");
      const evidence = join(evidenceRoot, fault);
      mkdirSync(evidence, { recursive: true });
      const name = `nucleus-artifact-negative-${fault}-${process.pid}`;
      let created = false;
      try {
        const accounts = prepareAccounts(
          createDocker(root, identity).accounts(image!),
          identity,
          "/home/acceptance",
          scratch,
        );
        for (const dir of ["target", "home"]) mkdirSync(join(scratch, dir));
        const modules = ["browser", "consumer"].includes(fault)
          ? prepareHaloDependencies(root, join(scratch, "dependencies"))
          : join(scratch, "modules");
        mkdirSync(modules, { recursive: true });
        for (const path of ["target", "halo/node_modules"])
          mkdirSync(join(root, path), { recursive: true });
        const extra: string[] = [];
        if (fault === "inputs") {
          writeFileSync(join(scratch, "inputs"), "invalid-input-digest\n", {
            mode: 0o644,
          });
          extra.push(
            "-v",
            `${join(scratch, "inputs")}:/etc/nucleus-halo-ci-inputs:ro`,
          );
        }
        if (fault === "node") {
          writeFileSync(join(scratch, "node"), "#!/bin/sh\necho v0.0.1\n", {
            mode: 0o755,
          });
          extra.push("-v", `${join(scratch, "node")}:/opt/node/bin/node:ro`);
        }
        const code = ["root", "inputs", "node"].includes(fault)
          ? 'await import("./tools/halo-image-runtime.ts").then(m=>m.acceptRuntime("/evidence"));'
          : `await import("./tools/halo-image-runtime.ts").then(m=>m.acceptRuntime("/evidence",${JSON.stringify(fault)}));`;
        docker([
          "create",
          "--name",
          name,
          "--init",
          "--network=none",
          "--read-only",
          "--shm-size=1g",
          "--user",
          fault === "root" ? "0:0" : `${identity.uid}:${identity.gid}`,
          "--workdir",
          "/workspace",
          "--tmpfs",
          "/tmp:rw,nosuid,nodev,mode=1777",
          "-e",
          fault === "root" ? "HOME=/tmp" : "HOME=/home/acceptance",
          "-v",
          `${root}:/workspace:ro`,
          "-v",
          `${join(scratch, "target")}:/workspace/target`,
          "-v",
          `${modules}:/workspace/halo/node_modules:ro`,
          "-v",
          `${join(scratch, "home")}:/home/acceptance`,
          "-v",
          `${evidence}:/evidence`,
          ...accounts.flatMap((mount) => [
            "-v",
            `${mount.source}:${mount.target}:ro`,
          ]),
          ...extra,
          image!,
          "bun",
          "-e",
          code,
        ]);
        created = true;
        const child = spawn("docker", ["start", "--attach", name], {
          stdio: ["ignore", "pipe", "pipe"],
        });
        let logs = "";
        child.stdout.on("data", (bytes) => {
          logs += bytes.toString();
        });
        child.stderr.on("data", (bytes) => {
          logs += bytes.toString();
        });
        const exit = new Promise<number | null>((done, reject) => {
          child.once("exit", done);
          child.once("error", reject);
        });
        let ended = false;
        void exit.then(() => {
          ended = true;
        });
        if (fault === "cancel") {
          const deadline = Date.now() + 60_000;
          while (
            !existsSync(join(evidence, "pg-ready.json")) &&
            Date.now() < deadline
          ) {
            if (ended)
              throw new Error("Runtime exited before cancellation boundary");
            await delay(50);
          }
          expect(existsSync(join(evidence, "pg-ready.json"))).toBe(true);
          docker(["kill", "--signal=SIGTERM", name]);
        }
        const status = await exit;
        writeFileSync(join(evidence, "container.log"), logs);
        expect(status, logs).not.toBe(0);
        const expectedFailure = {
          pg: "Private PostgreSQL query mismatch",
          browser: "Controlled browser failure",
          consumer: "Controlled consumer failure",
          cancel: "Controlled cancellation",
          root: "Artifact acceptance requires non-root",
          inputs: "Halo CI image inputs changed",
          node: "Halo CI requires Node",
        }[fault];
        expect(logs).toContain(expectedFailure);
        const state = JSON.parse(
          docker(["inspect", "--format", "{{json .State}}", name]),
        );
        writeFileSync(
          join(evidence, "container-state.json"),
          JSON.stringify(state, null, 2),
        );
        expect(state.Running).toBe(false);
        expect(state.ExitCode).not.toBe(0);
        if (fault !== "root") {
          const result = JSON.parse(
            readFileSync(join(evidence, "runtime.json"), "utf8"),
          );
          expect(result.success).toBe(false);
          expect(result.pgDirectoryRemoved).toBe(true);
          expect(result.postmasterGone).toBe(true);
          expect(result.cleanup).toEqual([]);
          if (fault === "cancel") expect(result.cancelled).toBe(true);
        }
        expect(existsSync(join(evidence, "validation.json"))).toBe(false);
      } finally {
        if (created) {
          const state = JSON.parse(
            docker(["inspect", "--format", "{{json .State}}", name]),
          );
          if (state.Running) docker(["stop", "--time", "60", name]);
          docker(["rm", name]);
          const absent = spawnSync("docker", ["inspect", name], {
            encoding: "utf8",
          });
          expect(absent.status).not.toBe(0);
          expect(absent.stderr).toMatch(/no such (object|container)/i);
        }
        rmSync(scratch, { recursive: true });
        expect(existsSync(scratch)).toBe(false);
        writeFileSync(
          join(evidence, "fixture-cleanup.json"),
          JSON.stringify({ containerAbsent: true, scratchRemoved: true }),
        );
      }
    },
    180_000,
  );
}
