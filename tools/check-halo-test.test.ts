import { afterEach, expect, test } from "bun:test";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { OutputCapture } from "./check-halo-output";

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function fixture(
  change:
    "none" | "missing" | "ignored" | "unignored" | "should-panic" = "none",
) {
  const directory = mkdtempSync(join(tmpdir(), "nucleus halo acceptance "));
  directories.push(directory);
  mkdirSync(join(directory, "tools"));
  mkdirSync(join(directory, "tests"));
  mkdirSync(join(directory, "halo/tools"), { recursive: true });
  copyFileSync(
    new URL("./check-halo-test.ts", import.meta.url),
    join(directory, "tools/check-halo-test.ts"),
  );
  copyFileSync(
    new URL("./check-halo-output.ts", import.meta.url),
    join(directory, "tools/check-halo-output.ts"),
  );
  copyFileSync(
    new URL("../halo/tools/evidence.ts", import.meta.url),
    join(directory, "halo/tools/evidence.ts"),
  );
  copyFileSync(
    new URL("../halo/tools/process.ts", import.meta.url),
    join(directory, "halo/tools/process.ts"),
  );
  copyFileSync(
    new URL("../rust-toolchain.toml", import.meta.url),
    join(directory, "rust-toolchain.toml"),
  );
  writeFileSync(
    join(directory, "Cargo.toml"),
    '[package]\nname = "nucleus"\nversion = "0.1.0"\nedition = "2024"\n',
  );
  writeFileSync(
    join(directory, "tests/http.rs"),
    `use std::io::Write as _;

// This tiny Rust fixture is a controlled producer of the runner/backend
// evidence protocol. The real consumers are exercised separately by the
// reporter and runner-lifecycle tests and by the real website fixture.
fn evidence(file: &str, event: &str, data: &str) {
    let (Ok(directory), Ok(run)) = (std::env::var("HALO_TEST_EVIDENCE_DIR"), std::env::var("HALO_TEST_RUN_ID")) else {
        return;
    };
    let record = format!(r#"{{"version":1,"runId":{run:?},"at":"fixture","event":{event:?},{data}}}"#);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::path::Path::new(&directory).join(file))
        .expect("fixture evidence");
    writeln!(file, "{record}").expect("fixture evidence line");
}

fn completed() {
    evidence("runner.jsonl", "run_end", r#""ok":true"#);
    evidence("runner.jsonl", "cleanup_end", r#""ok":true,"results":[]"#);
    evidence("backend.jsonl", "backend_drain", r#""outcome":"completed""#);
    evidence("backend.jsonl", "backend_acceptance", r#""outcome":"completed""#);
}

fn spawn_runner() -> std::process::Child {
    let directory = std::env::var("HALO_TEST_EVIDENCE_DIR").expect("fixture run directory");
    let run = std::env::var("HALO_TEST_RUN_ID").expect("fixture run id");
    let child = std::process::Command::new("node")
        .arg("browser-runner.mjs")
        .env("EVIDENCE_RUN_DIR", directory)
        .env("EVIDENCE_RUN_ID", run)
        .env("READY", "runner-ready")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("fixture runner");
    for _ in 0..400 {
        if std::path::Path::new("runner-ready").exists() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(std::path::Path::new("runner-ready").exists(), "fixture runner readiness");
    child
}

fn exercise(stage: &str) {
    println!("Rust fixture output");
    std::process::Command::new("sh")
        .args(["-c", "printf 'subprocess output without newline'"])
        .status().expect("inherited browser-style output");
    std::fs::write("executed", stage).expect("independent execution marker");
    match std::env::var("HALO_FIXTURE_OUTCOME").as_deref() {
        Ok("panic") => panic!("fixture assertion failed"),
        Ok("early-success") => std::process::exit(0),
        Ok("signal") => {
            std::process::Command::new("kill")
                .args(["-TERM", &std::process::id().to_string()])
                .status().expect("terminate this fixture test process");
            panic!("signal did not stop the test");
        }
        Ok("wait-release") => {
            println!("fixture-stream-marker");
            for _ in 0..600 {
                if std::path::Path::new("release").exists() { break; }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            assert!(std::path::Path::new("release").exists(), "fixture release timeout");
            if stage == "browser" { completed(); }
        }
        Ok("block") => {
            std::fs::write("rust-pid", std::process::id().to_string())
                .expect("fixture process id");
            println!("fixture-stream-marker");
            for _ in 0..1200 {
                if std::path::Path::new("release").exists() { break; }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            assert!(std::path::Path::new("release").exists(), "fixture release timeout");
        }
        Ok("runner-failed") => {
            evidence("runner.jsonl", "run_end", r#""ok":false"#);
        }
        Ok("missing-runner") => {
            evidence("backend.jsonl", "backend_drain", r#""outcome":"completed""#);
            evidence("backend.jsonl", "backend_acceptance", r#""outcome":"completed""#);
        }
        Ok("missing-backend") => {
            evidence("runner.jsonl", "run_end", r#""ok":true"#);
            evidence("runner.jsonl", "cleanup_end", r#""ok":true,"results":[]"#);
        }
        Ok("missing-acceptance") => {
            evidence("runner.jsonl", "run_end", r#""ok":true"#);
            evidence("runner.jsonl", "cleanup_end", r#""ok":true,"results":[]"#);
            evidence("backend.jsonl", "backend_drain", r#""outcome":"completed""#);
        }
        Ok("malformed-runner") => {
            completed();
            std::fs::OpenOptions::new().create(true).append(true)
                .open(std::path::Path::new(&std::env::var("HALO_TEST_EVIDENCE_DIR").unwrap()).join("runner.jsonl"))
                .expect("fixture runner log").write_all(b"not-json\\n").expect("malformed record");
        }
        Ok("truncated-runner") => {
            completed();
            std::fs::OpenOptions::new().create(true).append(true)
                .open(std::path::Path::new(&std::env::var("HALO_TEST_EVIDENCE_DIR").unwrap()).join("runner.jsonl"))
                .expect("fixture runner log").write_all(b"{\\"version\\":1,\\"runId\\":\\"cut").expect("truncated record");
        }
        Ok("foreign-runner") => {
            completed();
            evidence("runner.jsonl", "run_end", r#""ok":true"#);
            let directory = std::env::var("HALO_TEST_EVIDENCE_DIR").unwrap();
            let mut file = std::fs::OpenOptions::new().create(true).append(true)
                .open(std::path::Path::new(&directory).join("runner.jsonl")).expect("fixture runner log");
            writeln!(file, r#"{{"version":1,"runId":"another-run","at":"fixture","event":"run_end","ok":true}}"#).expect("foreign record");
        }
        Ok("runner-early-exit") => {
            spawn_runner();
            evidence("backend.jsonl", "backend_drain", r#""outcome":"completed""#);
            evidence("backend.jsonl", "backend_acceptance", r#""outcome":"completed""#);
        }
        Ok("runner-unexpected-exit") => {
            spawn_runner();
            panic!("fixture cargo failure after runner start");
        }
        Ok("runner-stubborn") => {
            spawn_runner();
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        Ok("runner-drain") => {
            let mut runner = spawn_runner();
            runner.wait().expect("runner exit");
            evidence("backend.jsonl", "backend_drain", r#""outcome":"started""#);
            std::thread::sleep(std::time::Duration::from_millis(750));
            evidence("backend.jsonl", "backend_drain", r#""outcome":"completed""#);
            evidence("backend.jsonl", "backend_acceptance", r#""outcome":"completed""#);
        }
        Ok("held-pipe" | "held-pipe-failure") => {
            use std::os::unix::process::CommandExt as _;
            // A separate untagged process holds the inherited output pipe.
            // Its lifetime is controlled by the test, not by harness ownership.
            std::process::Command::new("node")
                .arg("pipe-holder.mjs")
                .env_clear()
                .env("PATH", std::env::var("PATH").expect("fixture PATH"))
                .process_group(0)
                .stdin(std::process::Stdio::null())
                .spawn().expect("pipe holder");
            for _ in 0..400 {
                if std::path::Path::new("pipe-holder-ready").exists() { break; }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(std::path::Path::new("pipe-holder-ready").exists());
            completed();
            assert_ne!(std::env::var("HALO_FIXTURE_OUTCOME").unwrap(), "held-pipe-failure", "original fixture failure");
        }
        Ok("evidence-write-failure") => {
            completed();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let directory = std::env::var("HALO_TEST_EVIDENCE_DIR").unwrap();
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o500))
                    .expect("restrict fixture evidence directory");
            }
        }
        _ if stage == "browser" => completed(),
        _ => (),
    }
}
mod protocol {
    #[test]
    ${change === "ignored" ? "#[ignore]" : ""}
    fn ${change === "missing" ? "renamed_snapshot" : "http_validates_requests_before_business_work_and_describes_actual_routes"}() {
        super::exercise("snapshot");
    }
}
mod website {
    #[test]
    ${change === "unignored" ? "" : "#[ignore]"}
    ${change === "should-panic" ? "#[should_panic]" : ""}
    fn browser_c1_preserves_original_send() {
        super::exercise("browser");
    }
}
`,
  );
  writeFileSync(
    join(directory, "browser-runner.mjs"),
    `import { appendFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
const directory = process.env.EVIDENCE_RUN_DIR;
const run = process.env.EVIDENCE_RUN_ID;
const write = (file, event, extra = "") =>
  appendFileSync(
    join(directory, file),
    JSON.stringify({ version: 1, runId: run, at: new Date().toISOString(), event, ...JSON.parse(\`{\${extra}}\`) }) + "\\n",
  );
write("runner.jsonl", "run_start", \`"pid":\${process.pid}\`);
let finished = false;
const finish = () => {
  if (finished) return;
  finished = true;
  write("runner.jsonl", "run_end", '"ok":true');
  write("runner.jsonl", "cleanup_end", '"ok":true,"results":[]');
  process.exit(0);
};
if (process.env.HALO_FIXTURE_OUTCOME === "runner-stubborn") {
  process.on("SIGTERM", () => writeFileSync("received-signal", "SIGTERM"));
  process.on("SIGINT", () => writeFileSync("received-signal", "SIGINT"));
  setInterval(() => {}, 1000);
} else {
  process.on("SIGTERM", () => setTimeout(finish, 150));
  process.on("SIGINT", () => setTimeout(finish, 150));
  if (process.env.HALO_FIXTURE_OUTCOME === "runner-drain") setInterval(() => {}, 1000);
  else setTimeout(finish, 400);
}
writeFileSync(process.env.READY, "ready");
`,
  );
  writeFileSync(join(directory, "pipe-holder.mjs"), `
import { writeFileSync } from "node:fs";
writeFileSync("pipe-holder-ready", String(process.pid));
setTimeout(() => {}, 10000);
`);
  const env = {
    ...process.env,
    CARGO_NET_OFFLINE: "true",
    CARGO_TARGET_DIR: join(directory, "target"),
    RUST_TEST_THREADS: "1",
  };
  const locked = spawnSync("cargo", ["generate-lockfile", "--offline"], {
    cwd: directory,
    env,
    encoding: "utf8",
    timeout: 30_000,
  });
  expect(locked.error).toBeUndefined();
  expect(locked.status, locked.stderr).toBe(0);
  return {
    directory,
    env,
    original(stage: "snapshot" | "browser", outcome = "success") {
      return spawnSync(
        "cargo",
        [
          "test",
          "-p",
          "nucleus",
          "--locked",
          "--test",
          "http",
          stage === "snapshot"
            ? "protocol::http_validates_requests_before_business_work_and_describes_actual_routes"
            : "website::browser_c1_preserves_original_send",
          "--",
          "--exact",
          ...(stage === "browser" ? ["--ignored", "--nocapture"] : []),
        ],
        {
          cwd: directory,
          env: { ...env, HALO_FIXTURE_OUTCOME: outcome },
          encoding: "utf8",
          timeout: 30_000,
        },
      );
    },
    run(stage: string, outcome = "success") {
      return spawnSync(process.execPath, ["tools/check-halo-test.ts", stage], {
        cwd: directory,
        env: { ...env, HALO_FIXTURE_OUTCOME: outcome },
        encoding: "utf8",
        timeout: 30_000,
      });
    },
  };
}

test.each(["snapshot", "browser"])(
  "the %s entry executes its real Rust test and preserves the source and lockfile",
  (stage) => {
    const sample = fixture();
    const source = readFileSync(join(sample.directory, "tests/http.rs"));
    const lock = readFileSync(join(sample.directory, "Cargo.lock"));
    const result = sample.run(stage);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stdout + result.stderr).toBe(0);
    expect(readFileSync(join(sample.directory, "executed"), "utf8")).toBe(
      stage,
    );
    expect(result.stdout).toContain("1 passed; 0 failed; 0 ignored");
    expect(result.stdout).toContain("subprocess output without newline");
    expect(readFileSync(join(sample.directory, "tests/http.rs"))).toEqual(
      source,
    );
    expect(readFileSync(join(sample.directory, "Cargo.lock"))).toEqual(lock);
  },
  30_000,
);

test.each([
  ["missing", "snapshot"],
  ["ignored", "snapshot"],
  ["unignored", "browser"],
] as const)(
  "%s cannot silently omit the required %s acceptance",
  (change, stage) => {
    const sample = fixture(change);
    const original = sample.original(stage);
    expect(original.error).toBeUndefined();
    expect(original.status, original.stdout + original.stderr).toBe(0);
    expect(existsSync(join(sample.directory, "executed"))).toBe(false);
    const result = sample.run(stage);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stdout + result.stderr).toBe(1);
    expect(result.stderr).toContain(
      "did not complete exactly one required test",
    );
    expect(existsSync(join(sample.directory, "executed"))).toBe(false);
  },
  30_000,
);

test.each([
  ["snapshot", "panic"],
  ["browser", "panic"],
  ["snapshot", "early-success"],
  ["browser", "early-success"],
  ["snapshot", "signal"],
  ["browser", "signal"],
])(
  "the %s test ending with %s cannot pass the acceptance entry",
  (stage, outcome) => {
    const sample = fixture();
    const result = sample.run(stage, outcome);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stdout + result.stderr).toBe(1);
    expect(readFileSync(join(sample.directory, "executed"), "utf8")).toBe(
      stage,
    );
  },
  30_000,
);

test("should_panic cannot turn a failed browser assertion into accepted evidence", () => {
  const sample = fixture("should-panic");
  const original = sample.original("browser", "panic");
  expect(original.error).toBeUndefined();
  expect(original.status, original.stdout + original.stderr).toBe(0);
  const result = sample.run("browser", "panic");
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("did not complete exactly one required test");
}, 30_000);

test("compilation failure remains a failure and runs no test", () => {
  const sample = fixture();
  writeFileSync(
    join(sample.directory, "tests/http.rs"),
    'compile_error!("broken fixture");',
  );
  const result = sample.run("snapshot");
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("broken fixture");
  expect(existsSync(join(sample.directory, "executed"))).toBe(false);
});

test("an unknown acceptance mode fails without running a test", () => {
  const sample = fixture();
  const result = sample.run("unknown");
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("snapshot | browser");
  expect(existsSync(join(sample.directory, "executed"))).toBe(false);
});

async function waitFor(
  condition: () => boolean,
  timeoutMs: number,
  label: string,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() >= deadline)
      throw new Error(`Timed out waiting for ${label}`);
    await delay(25);
  }
}

test("streams child output before the test exits", async () => {
  const sample = fixture();
  const output: string[] = [];
  const child = spawn(
    process.execPath,
    ["tools/check-halo-test.ts", "browser"],
    {
      cwd: sample.directory,
      env: { ...sample.env, HALO_FIXTURE_OUTCOME: "wait-release" },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  child.stdout.on("data", (chunk) => output.push(String(chunk)));
  child.stderr.on("data", (chunk) => output.push(String(chunk)));
  try {
    await waitFor(
      () => output.join("").includes("fixture-stream-marker"),
      30_000,
      "the child marker to be forwarded",
    );
    // The test is still waiting for its release file: buffered spawnSync
    // output could not have reached us at this point.
    expect(child.exitCode).toBeNull();
    writeFileSync(join(sample.directory, "release"), "go");
    const [code] = await Promise.race([
      once(child, "exit"),
      delay(30_000).then(() => {
        throw new Error("checker did not exit after release");
      }),
    ]);
    expect(code).toBe(0);
    expect(output.join("")).toContain("1 passed; 0 failed");
  } finally {
    if (child.exitCode === null && child.signalCode === null)
      child.kill("SIGKILL");
  }
}, 60_000);

test("a parent SIGTERM stops the cargo tree and records an interrupted failure", async () => {
  const sample = fixture();
  const output: string[] = [];
  const child = spawn(
    process.execPath,
    ["tools/check-halo-test.ts", "browser"],
    {
      cwd: sample.directory,
      env: { ...sample.env, HALO_FIXTURE_OUTCOME: "block" },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  child.stdout.on("data", (chunk) => output.push(String(chunk)));
  child.stderr.on("data", (chunk) => output.push(String(chunk)));
  try {
    await waitFor(
      () => output.join("").includes("fixture-stream-marker"),
      30_000,
      "the blocked test to start",
    );
    const rustPid = Number(
      readFileSync(join(sample.directory, "rust-pid"), "utf8"),
    );
    process.kill(child.pid!, "SIGTERM");
    const [code] = await Promise.race([
      once(child, "exit"),
      delay(30_000).then(() => {
        throw new Error("checker did not stop after SIGTERM");
      }),
    ]);
    expect(code).toBe(1);
    await waitFor(() => {
      try {
        process.kill(rustPid, 0);
        return false;
      } catch (error) {
        return (error as NodeJS.ErrnoException).code === "ESRCH";
      }
    }, 10_000, "the cargo test tree to end");
    const root = join(sample.directory, "target", "halo-browser-evidence");
    const current = JSON.parse(
      readFileSync(join(root, "current.json"), "utf8"),
    ) as { runId: string };
    const harness = JSON.parse(
      readFileSync(join(root, "runs", current.runId, "harness.json"), "utf8"),
    ) as {
      parentSignal: string | null;
      failure: { kind: string };
      exit: { observed: boolean; code: number | null; signal: string | null };
      backend: { outcome: string };
    };
    expect(harness.parentSignal).toBe("SIGTERM");
    expect(harness.failure.kind).toBe("parent_signal");
    expect(harness.exit.observed).toBe(true);
    expect(harness.backend.outcome).toBe("unknown");
    // The fixture writes this marker before blocking; the test did start.
    expect(existsSync(join(sample.directory, "executed"))).toBe(true);
  } finally {
    if (child.exitCode === null && child.signalCode === null)
      child.kill("SIGKILL");
  }
}, 60_000);

test("an unwritable evidence path fails before running the acceptance", () => {
  const sample = fixture();
  const blocked = join(sample.directory, "blocked-evidence");
  writeFileSync(blocked, "not a directory");
  const result = spawnSync(
    process.execPath,
    ["tools/check-halo-test.ts", "browser"],
    {
      cwd: sample.directory,
      env: { ...sample.env, HALO_TEST_EVIDENCE: blocked },
      encoding: "utf8",
      timeout: 30_000,
    },
  );
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stderr.trim().length).toBeGreaterThan(0);
  expect(existsSync(join(sample.directory, "executed"))).toBe(false);
});

test("a cargo spawn failure is an explicit failure", () => {
  const sample = fixture();
  const result = spawnSync(
    process.execPath,
    ["tools/check-halo-test.ts", "snapshot"],
    {
      cwd: sample.directory,
      env: { ...sample.env, PATH: "/nonexistent" },
      encoding: "utf8",
      timeout: 30_000,
    },
  );
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("cargo");
  expect(existsSync(join(sample.directory, "executed"))).toBe(false);
});

test.each([
  ["snapshot", false],
  ["browser", false],
  ["browser", true],
] as const)("the %s entry rejects unclosed output (prior failure: %s)", async (stage, priorFailure) => {
  const sample = fixture();
  try {
    const result = sample.run(stage, priorFailure ? "held-pipe-failure" : "held-pipe");
    expect(result.error).toBeUndefined();
    expect(result.status, result.stdout + result.stderr).toBe(1);
    expect(result.stderr).toContain(priorFailure ? "cargo ended with 101" : "cargo output streams did not close");
    if (stage === "browser") {
      expect(evidenceRun(sample).harness()).toMatchObject({
        exit: { observed: true, code: priorFailure ? 101 : 0 },
        proof: { matched: !priorFailure },
        streamClosed: false,
        processesGone: true,
        failure: { kind: priorFailure ? "cargo_exit" : "stream_close_unknown" },
        evidenceFailure: null,
      });
    }
  } finally {
    const marker = join(sample.directory, "pipe-holder-ready");
    if (existsSync(marker)) {
      const pid = Number(readFileSync(marker, "utf8"));
      if (!dead(pid)) process.kill(pid, "SIGKILL");
      await waitFor(() => dead(pid), 5_000, "pipe holder termination");
    }
  }
}, 15_000);

test("a failed runner record cannot pass despite a clean cargo exit", () => {
  const sample = fixture();
  const result = sample.run("browser", "runner-failed");
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("browser runner recorded a failed outcome");
});

function evidenceRun(sample: ReturnType<typeof fixture>) {
  const root = join(sample.directory, "target", "halo-browser-evidence");
  const current = JSON.parse(readFileSync(join(root, "current.json"), "utf8")) as {
    runId: string;
  };
  const directory = join(root, "runs", current.runId);
  return {
    directory,
    harness: () =>
      JSON.parse(readFileSync(join(directory, "harness.json"), "utf8")) as {
        failure: { kind: string } | null;
        evidenceFailure: string | null;
        parentSignal: string | null;
        processes: Array<Record<string, unknown>>;
        processesGone: boolean;
        runner: { present: boolean; ok: boolean | null; cleanupOk: boolean | null };
        backend: { present: boolean; outcome: string | null; acceptance: string | null };
      },
    records: (file: string) =>
      readFileSync(join(directory, file), "utf8")
        .split("\n")
        .filter(Boolean)
        .map((line) => JSON.parse(line) as Record<string, unknown>),
  };
}

function dead(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return false;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "ESRCH";
  }
}

test("Cargo exiting first still waits for the registered runner to finish cleanup", () => {
  const sample = fixture();
  const result = sample.run("browser", "runner-early-exit");
  expect(result.error).toBeUndefined();
  expect(result.status, result.stdout + result.stderr).toBe(0);
  const evidence = evidenceRun(sample);
  const harness = evidence.harness();
  expect(harness.processesGone).toBe(true);
  expect(harness.evidenceFailure).toBeNull();
  expect(harness.runner).toMatchObject({
    present: true,
    ok: true,
    cleanupOk: true,
  });
  expect(harness.backend).toMatchObject({
    present: true,
    outcome: "completed",
    acceptance: "completed",
  });
  const start = evidence
    .records("runner.jsonl")
    .find((record) => record.event === "run_start");
  expect(typeof start?.pid).toBe("number");
  // The checker observed actual termination, not merely Cargo's exit.
  expect(dead(start!.pid as number)).toBe(true);
}, 30_000);

test("an unexpected Cargo exit still terminates and accounts for a live runner", () => {
  const sample = fixture();
  const result = sample.run("browser", "runner-unexpected-exit");
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("cargo ended");
  const evidence = evidenceRun(sample);
  const harness = evidence.harness();
  expect(harness.processesGone).toBe(true);
  expect(harness.processes).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ label: "runner", termSent: true }),
    ]),
  );
  const runEnd = evidence
    .records("runner.jsonl")
    .find((record) => record.event === "run_end");
  expect(runEnd?.ok).toBe(true);
  const start = evidence
    .records("runner.jsonl")
    .find((record) => record.event === "run_start");
  expect(dead(start!.pid as number)).toBe(true);
}, 30_000);

test.each(["SIGINT", "SIGTERM"] as const)("%s allows backend drain after the runner exits", async (signal) => {
  const sample = fixture();
  const checker = join(sample.directory, "tools/check-halo-test.ts");
  const child = spawn(process.execPath, [checker, "browser"], {
    cwd: sample.directory,
    env: { ...sample.env, HALO_FIXTURE_OUTCOME: "runner-drain" },
    stdio: "ignore",
  });
  try {
    await waitFor(() => existsSync(join(sample.directory, "runner-ready")), 10_000, "runner readiness");
    const stopped = once(child, "exit");
    process.kill(child.pid!, signal);
    const [code] = await Promise.race([
      stopped,
      delay(5_000).then(() => { throw new Error("backend drain did not finish"); }),
    ]);
    expect(code).toBe(1);
    const evidence = evidenceRun(sample);
    expect(evidence.records("backend.jsonl").filter((r) => r.event === "backend_drain")
      .map((r) => r.outcome), JSON.stringify(evidence.harness())).toEqual(["started", "completed"]);
    expect(evidence.harness()).toMatchObject({
      parentSignal: signal, forcedKill: false, processesGone: true,
      backend: { outcome: "completed", acceptance: "completed" },
    });
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
  }
}, 20_000);

test("SIGINT reaches the registered runner and one grace bounds forced cleanup", async () => {
  const sample = fixture();
  const checker = join(sample.directory, "tools/check-halo-test.ts");
  let source = readFileSync(checker, "utf8");
  for (const [before, after] of [
    ["const SIGNAL_GRACE_MS = 60_000;", "const SIGNAL_GRACE_MS = 250;"],
    ["const KILL_GRACE_MS = 10_000;", "const KILL_GRACE_MS = 1_000;"],
  ]) {
    expect(source.split(before!).length).toBe(2);
    source = source.replace(before!, after!);
  }
  writeFileSync(checker, source);
  const child = spawn(process.execPath, [checker, "browser"], {
    cwd: sample.directory,
    env: { ...sample.env, HALO_FIXTURE_OUTCOME: "runner-stubborn" },
    stdio: "ignore",
  });
  let runner: number | undefined;
  try {
    await waitFor(() => existsSync(join(sample.directory, "runner-ready")), 10_000, "runner readiness");
    const evidence = evidenceRun(sample);
    runner = evidence.records("runner.jsonl").find((r) => r.event === "run_start")!.pid as number;
    const stopped = once(child, "exit");
    process.kill(child.pid!, "SIGINT");
    const [code] = await Promise.race([
      stopped,
      delay(5_000).then(() => { throw new Error("bounded cancellation did not finish"); }),
    ]);
    expect(code).toBe(1);
    expect(readFileSync(join(sample.directory, "received-signal"), "utf8")).toBe("SIGINT");
    expect(evidence.harness()).toMatchObject({
      parentSignal: "SIGINT", forcedKill: true, processesGone: true,
    });
    expect(dead(runner)).toBe(true);
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    if (runner !== undefined && !dead(runner)) process.kill(runner, "SIGKILL");
  }
}, 20_000);

test.each([
  ["missing-runner", "runner evidence is missing"],
  ["missing-backend", "backend evidence is missing"],
  ["missing-acceptance", "backend acceptance did not complete"],
  ["malformed-runner", "malformed records"],
  ["truncated-runner", "malformed records"],
  ["foreign-runner", "another run"],
] as const)("%s cannot pass a clean Cargo exit", (outcome, message) => {
  const sample = fixture();
  const result = sample.run("browser", outcome);
  expect(result.error).toBeUndefined();
  expect(result.status, result.stdout + result.stderr).toBe(1);
  expect(result.stderr).toContain(message);
}, 30_000);

test("a reused run identifier is rejected before Cargo runs", () => {
  const sample = fixture();
  const root = join(sample.directory, "target", "halo-browser-evidence");
  const runId = "reused-run";
  mkdirSync(join(root, "runs", runId), { recursive: true });
  const result = spawnSync(
    process.execPath,
    ["tools/check-halo-test.ts", "browser"],
    {
      cwd: sample.directory,
      env: { ...sample.env, HALO_TEST_RUN_ID: runId },
      encoding: "utf8",
      timeout: 30_000,
    },
  );
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("already exists");
  expect(existsSync(join(sample.directory, "executed"))).toBe(false);
});

test.skipIf(typeof process.getuid === "function" && process.getuid() === 0)(
  "a final harness write failure is nonzero even with otherwise complete evidence",
  () => {
    const sample = fixture();
    const result = sample.run("browser", "evidence-write-failure");
    const root = join(sample.directory, "target", "halo-browser-evidence");
    try {
      expect(result.error).toBeUndefined();
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("Halo evidence write failed");
    } finally {
      const current = JSON.parse(
        readFileSync(join(root, "current.json"), "utf8"),
      ) as { runId: string };
      chmodSync(join(root, "runs", current.runId), 0o700);
    }
  },
  30_000,
);

const proofName = "website::browser_c1_preserves_original_send";
const proofSummary =
  "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 12.34s";

test("the proof parser accepts split chunks without a final newline", () => {
  const capture = new OutputCapture(proofName);
  capture.push(`test ${proofName} ... ok\r\n\r\n`, "stdout");
  capture.push(proofSummary.slice(0, 20), "stdout");
  capture.push(proofSummary.slice(20), "stdout");
  expect(capture.completed()).toBe(true);
});

test("stderr cannot provide the acceptance proof", () => {
  const capture = new OutputCapture(proofName);
  capture.push(`test ${proofName} ... ok\r\n\r\n${proofSummary}`, "stderr");
  expect(capture.completed()).toBe(false);
});

test("a different filtered count is not the expected proof", () => {
  const capture = new OutputCapture(proofName);
  capture.push(
    `test ${proofName} ... ok\r\n\r\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 1.0s`,
    "stdout",
  );
  expect(capture.completed()).toBe(false);
});

test("a huge unterminated line stays bounded and a later proof line survives", () => {
  const capture = new OutputCapture(proofName);
  capture.push("x".repeat(2 * 1024 * 1024), "stdout");
  capture.push("\n", "stdout");
  capture.push(`test ${proofName} ... ok\r\n\r\n${proofSummary}\n`, "stdout");
  expect(capture.completed()).toBe(true);
  expect(capture.tail("stdout").length).toBeLessThanOrEqual(512 * 1024);
});

test("the tail is bounded and not appended a second time", () => {
  const capture = new OutputCapture(proofName);
  capture.push("a".repeat(600 * 1024), "stdout");
  capture.push("b".repeat(10), "stdout");
  expect(capture.tail("stdout").length).toBe(512 * 1024);
  expect(capture.tail("stdout").endsWith("b".repeat(10))).toBe(true);
});
