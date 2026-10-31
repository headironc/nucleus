//! Foreground supervisor for the external native mini fixture.
//!
//! This file owns exactly one run directory, one child harness process and one private control
//! connection. It is test supervision, not product code and not native-client acceptance.

import { createHash, randomUUID } from "node:crypto";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { execFileSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
  symlinkSync,
} from "node:fs";
import { createConnection, createServer, type Socket } from "node:net";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";
import { readback } from "./processes";
import { bunVersionProblems } from "../../../tools/check-bun.ts";

type Mode =
  | "full"
  | "control"
  | "disconnect"
  | "startup"
  | "sigterm"
  | "sigint"
  | "deadline"
  | "cleanup_failure"
  | "external";

type Options = {
  repository: string;
  runRoot: string;
  clientDir: string;
  clientRevision: string;
  clientBuild: string;
  serviceOrigin: string;
  mode: Mode;
  owner: string;
};

type Identity = {
  run: string;
  sourceRevision: string;
  openapiSha256: string;
  clientRevision: string;
  clientBuild: string;
};

const DROP_CASES = [
  "registration",
  "logout_current",
  "logout_all",
  "logout_other",
];
const INJECT_CASES = ["current_503", "sessions_503"];

function usage(): never {
  throw new Error(
    "usage: bun tests/http/native/runner.ts [--owner <chat-or-task>] [--client-revision <commit>] " +
      "[--client-build <id>] [--repository <dir>] [--run-root <dir>] " +
      "[--service-origin https://127.0.0.1:<port>] [--mode full|control|disconnect|startup|sigterm|sigint|deadline|cleanup_failure|external]",
  );
}

function parseOptions(argv: string[]): Options {
  const values = new Map<string, string>();
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (!key?.startsWith("--") || !value || value.startsWith("--")) usage();
    if (
      ![
        "owner",
        "client-revision",
        "client-build",
        "repository",
        "run-root",
        "service-origin",
        "mode",
      ].includes(key.slice(2)) ||
      values.has(key.slice(2))
    )
      usage();
    values.set(key.slice(2), value);
  }
  const repository = resolve(
    values.get("repository") ??
      fileURLToPath(new URL("../../..", import.meta.url)),
  );
  const clientDir = repository;
  const runRoot = resolve(values.get("run-root") ?? tmpdir());
  const mode = (values.get("mode") ?? "full") as Mode;
  const owner = values.get("owner") ?? process.env.CODEX_THREAD_ID;
  if (!owner || owner.length > 128 || !/^[A-Za-z0-9._:/-]+$/.test(owner))
    throw new Error("--owner must bind the actual task or chat");
  if (
    ![
      "full",
      "control",
      "disconnect",
      "startup",
      "sigterm",
      "sigint",
      "deadline",
      "cleanup_failure",
      "external",
    ].includes(mode)
  )
    usage();
  return {
    repository,
    runRoot,
    clientDir,
    clientRevision: values.get("client-revision") ?? "",
    clientBuild: values.get("client-build") ?? "",
    serviceOrigin: values.get("service-origin") ?? "",
    mode,
    owner,
  };
}

function sha256(value: string | Buffer): string {
  return createHash("sha256").update(value).digest("hex");
}

function privateFile(path: string, value: unknown): void {
  const text =
    typeof value === "string" ? value : JSON.stringify(value, null, 2);
  writeFileSync(path, text, { mode: 0o600 });
  chmodSync(path, 0o600);
}

function redact(value: string): string {
  return value
    .replace(/Bearer\s+[A-Za-z0-9._~+/=-]+/gi, "Bearer [redacted]")
    .replace(
      /("(?:token|authorization|secret|code)"\s*:\s*")[^"]+"/gi,
      '$1[redacted]"',
    );
}

function git(repository: string, args: string[]): string {
  return execFileSync("git", ["-C", repository, ...args], {
    encoding: "utf8",
  }).trim();
}

function boundedIdentity(value: string, label: string): string {
  if (!value || value.length > 256 || !/^[A-Za-z0-9._+:@/-]+$/.test(value)) {
    throw new Error(`${label} must be a bounded identity`);
  }
  return value;
}

function treeHash(root: string): string {
  const digest = createHash("sha256");
  const walk = (directory: string, prefix: string) => {
    for (const name of readdirSync(directory).sort()) {
      const path = join(directory, name);
      const relative = prefix ? `${prefix}/${name}` : name;
      const metadata = statSync(path);
      if (metadata.isDirectory()) walk(path, relative);
      else if (metadata.isFile())
        digest.update(relative).update("\0").update(readFileSync(path));
    }
  };
  walk(root, "");
  return digest.digest("hex");
}

function prepare(options: Options): {
  identity: Identity;
  origin: string;
  port: number;
} {
  if (!statSync(options.clientDir).isDirectory())
    throw new Error("--client-dir must be a directory");
  const revision = git(options.repository, [
    "rev-parse",
    `${options.clientRevision || "HEAD"}^{commit}`,
  ]);
  const build =
    options.clientBuild ||
    boundedIdentity(
      `orbit-src-${treeHash(join(options.repository, "orbit", "src")).slice(0, 32)}`,
      "client build",
    );
  const head = boundedIdentity(
    git(options.repository, ["rev-parse", "HEAD"]),
    "source revision",
  );
  const digest = createHash("sha256");
  digest.update(
    execFileSync("git", ["-C", options.repository, "diff", "--binary", "HEAD"]),
  );
  for (const name of git(options.repository, [
    "ls-files",
    "--others",
    "--exclude-standard",
    "-z",
  ])
    .split("\0")
    .filter(Boolean)
    .sort()) {
    digest
      .update(name)
      .update("\0")
      .update(readFileSync(join(options.repository, name)));
  }
  const sourceRevision = `${head}+worktree.${digest.digest("hex")}`;
  const openapi = join(options.repository, "halo", "protocol", "openapi.json");
  if (!existsSync(openapi))
    throw new Error("repository OpenAPI document is unavailable");
  const openapiSha256 = sha256(readFileSync(openapi));
  const origin = options.serviceOrigin || "";
  if (origin && !/^https:\/\/127\.0\.0\.1:[1-9][0-9]{0,4}$/.test(origin)) {
    throw new Error(
      "--service-origin must be exactly https://127.0.0.1:<port>",
    );
  }
  if (origin && Number(origin.slice(origin.lastIndexOf(":") + 1)) > 65535)
    throw new Error("service origin port exceeds 65535");
  return {
    identity: {
      run: randomUUID(),
      sourceRevision,
      openapiSha256,
      clientRevision: boundedIdentity(revision, "client revision"),
      clientBuild: boundedIdentity(build, "client build"),
    },
    origin,
    port: origin ? Number(origin.slice(origin.lastIndexOf(":") + 1)) : 0,
  };
}

function freePort(): Promise<number> {
  return new Promise((resolvePort, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close((error) => (error ? reject(error) : resolvePort(port)));
    });
  });
}

class LineSocket {
  private buffer = "";
  private pending: Array<{
    resolve(value: any): void;
    reject(error: Error): void;
    timer: ReturnType<typeof setTimeout>;
  }> = [];
  private closed = false;

  private constructor(private socket: Socket) {
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => this.receive(chunk));
    socket.on("error", () => this.fail("private control socket failed"));
    socket.on("close", () => this.fail("private control socket closed"));
  }

  static connect(path: string): Promise<LineSocket> {
    return new Promise((resolveSocket, reject) => {
      const socket = createConnection({ path });
      socket.once("connect", () => resolveSocket(new LineSocket(socket)));
      socket.once("error", reject);
    });
  }

  private receive(chunk: string) {
    this.buffer += chunk;
    if (this.buffer.length > 1024 * 1024) {
      this.socket.destroy();
      this.fail("private control reply exceeded its limit");
      return;
    }
    for (;;) {
      const newline = this.buffer.indexOf("\n");
      if (newline < 0) return;
      const line = this.buffer.slice(0, newline);
      this.buffer = this.buffer.slice(newline + 1);
      if (!line) continue;
      const pending = this.pending.shift();
      if (!pending) continue;
      clearTimeout(pending.timer);
      try {
        pending.resolve(JSON.parse(line));
      } catch {
        pending.reject(new Error("private control reply is not JSON"));
      }
    }
  }

  private fail(message: string) {
    if (this.closed) return;
    this.closed = true;
    for (const pending of this.pending.splice(0)) {
      clearTimeout(pending.timer);
      pending.reject(new Error(message));
    }
  }

  send(command: Record<string, unknown>, timeoutMs = 180_000): Promise<any> {
    if (this.closed)
      return Promise.reject(new Error("private control socket is closed"));
    return new Promise((resolveReply, reject) => {
      const timer = setTimeout(() => {
        const index = this.pending.findIndex(
          (value) => value.resolve === resolveReply,
        );
        if (index >= 0) this.pending.splice(index, 1);
        reject(new Error("private control reply timed out"));
      }, timeoutMs);
      this.pending.push({ resolve: resolveReply, reject, timer });
      this.socket.write(`${JSON.stringify(command)}\n`);
    });
  }

  destroy() {
    this.fail("private control socket closed");
    this.socket.destroy();
  }

  close() {
    this.fail("private control socket closed");
    this.socket.end();
  }
}

async function connectControl(path: string): Promise<LineSocket> {
  let last: unknown;
  for (let attempt = 0; attempt < 50; attempt++) {
    try {
      return await LineSocket.connect(path);
    } catch (error) {
      last = error;
      await delay(100);
    }
  }
  throw new Error(
    `private control did not accept a connection: ${String(last)}`,
  );
}

function delay(ms: number): Promise<void> {
  return new Promise((resolveDelay) => {
    const timer = setTimeout(resolveDelay, ms);
    timer.unref();
  });
}

export type Child = {
  process: ChildProcessWithoutNullStreams;
  ready: Promise<void>;
  exit: Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
  log(): string;
};

function childHarness(
  options: Options,
  identity: Identity,
  runDir: string,
  origin: string,
): Child {
  let resolveReady: () => void = () => {};
  let rejectReady: (error: Error) => void = () => {};
  const ready = new Promise<void>((resolvePromise, rejectPromise) => {
    resolveReady = resolvePromise;
    rejectReady = rejectPromise;
  });
  const lines: string[] = [];
  const environment = {
    ...process.env,
    CARGO_HOME:
      process.env.CARGO_HOME ?? join(options.repository, "target", "cargo"),
    TMPDIR: join(runDir, "tmp"),
    NATIVE_RUN_ID: identity.run,
    NATIVE_RUN_DIR: runDir,
    NATIVE_SERVICE_ORIGIN: origin,
    NATIVE_SOURCE_REVISION: identity.sourceRevision,
    NATIVE_OPENAPI_SHA256: identity.openapiSha256,
    NATIVE_CLIENT_REVISION: identity.clientRevision,
    NATIVE_CLIENT_BUILD: identity.clientBuild,
  };
  const cargo = process.env.CARGO ?? "cargo";
  const build = execFileSync(
    cargo,
    [
      "test",
      "--locked",
      "--offline",
      "--no-run",
      "--test",
      "http",
      "--message-format=json",
    ],
    {
      cwd: options.repository,
      env: environment,
      encoding: "utf8",
      timeout: 180_000,
      maxBuffer: 8 * 1024 * 1024,
    },
  );
  const executable = build
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line))
    .find(
      (item) =>
        item.reason === "compiler-artifact" &&
        item.target.name === "http" &&
        item.executable,
    )?.executable;
  if (!executable)
    throw new Error("cargo did not identify the HTTP test executable");
  const child = spawn(
    executable,
    [
      `native::harness::${options.mode === "deadline" ? "native_deadline_harness" : options.mode === "cleanup_failure" ? "native_cleanup_harness" : "native_harness"}`,
      "--ignored",
      "--exact",
      "--nocapture",
    ],
    {
      cwd: options.repository,
      env: environment,
      detached: true,
      stdio: ["pipe", "pipe", "pipe"],
    },
  );
  const reader = createInterface({ input: child.stdout });
  reader.on("line", (line) => {
    lines.push(line);
    if (lines.join("\n").length > 256 * 1024) lines.shift();
    if (line.includes(`READY ${identity.run}`)) resolveReady();
  });
  const errors: string[] = [];
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk: string) => {
    errors.push(chunk);
    if (errors.join("").length > 256 * 1024) errors.shift();
  });
  const exit = new Promise<{
    code: number | null;
    signal: NodeJS.Signals | null;
  }>((resolveExit) => {
    child.once("exit", (code, signal) => {
      resolveExit({ code, signal });
      rejectReady(
        new Error(`native harness exited before readiness (${signal ?? code})`),
      );
    });
  });
  const log = () => redact([...lines, ...errors].join("\n"));
  for (const signal of ["SIGTERM", "SIGINT"] as const)
    process.once(signal, () => {
      if (child.pid && child.exitCode === null && child.signalCode === null)
        process.kill(-child.pid, signal);
    });
  const boundedReady = Promise.race([
    ready,
    delay(120_000).then(() => {
      throw new Error("readiness deadline expired");
    }),
  ]);
  void boundedReady.catch(() => {});
  return { process: child, ready: boundedReady, exit, log };
}

async function hello(control: LineSocket, identity: Identity): Promise<void> {
  const reply = await control.send(
    {
      command: "hello",
      run: identity.run,
      client_revision: identity.clientRevision,
      client_build: identity.clientBuild,
    },
    30_000,
  );
  if (!reply?.ok || reply.code !== "ready") {
    throw new Error(`private control refused hello: ${String(reply?.code)}`);
  }
}

async function finish(
  control: LineSocket,
  identity: Identity,
  observations: Record<string, boolean>,
) {
  const reply = await control.send({
    command: "finish",
    run: identity.run,
    observations,
  });
  if (!reply || typeof reply !== "object")
    throw new Error("finish returned no report");
  return reply;
}

async function abort(control: LineSocket, identity: Identity, reason: string) {
  try {
    await control.send({ command: "abort", run: identity.run, reason }, 5_000);
  } catch {
    /* The child still owns the disconnect and cleanup path. */
  }
}

async function controlExercise(
  control: LineSocket,
  identity: Identity,
  runDir: string,
) {
  const reject = async (command: Record<string, unknown>, code: string) => {
    const reply = await control.send(command, 30_000);
    if (reply?.ok !== false || reply.code !== code) {
      throw new Error(
        `control rejected with ${String(reply?.code)}, expected ${code}`,
      );
    }
  };
  await reject(
    { command: "arm", run: identity.run, case: "registration" },
    "out_of_order",
  );
  await reject(
    { command: "checkpoint", run: identity.run, case: "registration" },
    "out_of_order",
  );
  await reject(
    {
      command: "hello",
      run: identity.run,
      client_revision: "wrong-revision",
      client_build: identity.clientBuild,
    },
    "identity_mismatch",
  );
  const ready = await control.send(
    {
      command: "hello",
      run: identity.run,
      client_revision: identity.clientRevision,
      client_build: identity.clientBuild,
    },
    30_000,
  );
  if (!ready?.ok || ready.code !== "ready")
    throw new Error("control hello was not accepted");
  await reject(
    {
      command: "hello",
      run: identity.run,
      client_revision: identity.clientRevision,
      client_build: identity.clientBuild,
    },
    "out_of_order",
  );
  await reject(
    { command: "snapshot", run: identity.run, unexpected: true },
    "invalid_control",
  );
  await reject({ command: "snapshot", run: randomUUID() }, "invalid_run");
  await reject(
    {
      command: "arm",
      run: identity.run,
      case: "registration",
      actor: randomUUID(),
    },
    "invalid_fault",
  );
  const snapshot = await control.send(
    { command: "snapshot", run: identity.run },
    30_000,
  );
  if (!snapshot?.ok || snapshot.code !== "snapshot")
    throw new Error("control snapshot was not accepted");
  const replies: any[] = [];
  const commands = async function* () {
    for (const command of [
      { command: "finish", run: randomUUID() },
      {
        command: "abort",
        run: identity.run,
        reason: "operator_stop",
        unexpected: true,
      },
      { command: "snapshot", run: identity.run },
      { command: "finish", run: identity.run },
    ])
      yield JSON.stringify(command) + "\n";
  };
  await externalCommands(commands(), control, identity.run, (reply) =>
    replies.push(reply),
  );
  if (
    replies.length !== 4 ||
    replies[0]?.code !== "invalid_run" ||
    replies[1]?.code !== "invalid_control" ||
    replies[2]?.code !== "snapshot"
  ) {
    throw new Error(
      "rejected terminal command ended the external control interaction",
    );
  }
  const report = replies[3];
  if (report.status !== "stopped")
    throw new Error(`control scenario status ${String(report.status)}`);
  const evidence = readFileSync(join(runDir, "evidence.json"), "utf8");
  const parsed = JSON.parse(evidence);
  if (parsed.scenario?.rejections < 2)
    throw new Error("control evidence did not record rejected commands");
  if (parsed.captures?.length)
    throw new Error("control scenario unexpectedly captured a fault");
  validateCleanup(parsed);
}

function validateCleanup(evidence: any) {
  for (const key of [
    "privateRelay",
    "applicationStopSignal",
    "applicationDrain",
    "independentFacts",
    "observer",
    "miniSupplier",
    "unusedCaptchaSupplier",
    "postgresql",
  ]) {
    if (evidence.cleanup?.[key] !== true)
      throw new Error(`cleanup ${key} did not confirm`);
  }
  if (evidence.leaks !== false) throw new Error("cleanup reported a leak");
  if (
    !Array.isArray(evidence.cleanup.errors) ||
    evidence.cleanup.errors.length
  ) {
    throw new Error("cleanup recorded an error");
  }
}

function validateFull(evidence: any) {
  if (evidence.status !== "passed" || evidence.scenario?.passed !== true) {
    throw new Error(
      `full scenario status ${String(evidence.status)}: ${String(evidence.scenario?.reason)}`,
    );
  }
  validateCleanup(evidence);
  const captures = new Map<string, any>(
    evidence.captures.map((capture: any) => [capture.case, capture]),
  );
  for (const name of DROP_CASES) {
    const capture = captures.get(name);
    if (
      !capture ||
      capture.dropped !== true ||
      capture.durable !== true ||
      capture.status < 200 ||
      capture.status >= 300
    ) {
      throw new Error(
        `drop case ${name} did not prove one durable dropped success`,
      );
    }
  }
  for (const name of INJECT_CASES) {
    const capture = captures.get(name);
    if (!capture || capture.dropped !== false || capture.status !== 503) {
      throw new Error(`dependency case ${name} did not prove a bounded 503`);
    }
  }
  if (captures.size !== DROP_CASES.length + INJECT_CASES.length)
    throw new Error("evidence captured unexpected cases");
  for (const [key, value] of Object.entries(evidence.observations ?? {})) {
    if (value !== true) throw new Error(`observation ${key} was not confirmed`);
  }
}

async function waitExit(
  child: Child,
  ms: number,
): Promise<{ code: number | null; signal: NodeJS.Signals | null } | undefined> {
  return Promise.race([child.exit, delay(ms).then(() => undefined)]);
}

/** A parent timer remains live even when the harness cannot schedule its own deadline. */
export async function bounded<T>(
  work: Promise<T>,
  milliseconds: number,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      work,
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error("independent supervisor deadline expired")),
          milliseconds,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function externalCommands(
  input: AsyncIterable<string>,
  control: LineSocket,
  run: string,
  output: (reply: any) => void,
): Promise<void> {
  let buffer = "";
  for await (const chunk of input) {
    buffer += chunk;
    if (Buffer.byteLength(buffer) > 64 * 1024)
      throw new Error("native control input exceeded its limit");
    for (;;) {
      const end = buffer.indexOf("\n");
      if (end < 0) break;
      const line = buffer.slice(0, end);
      buffer = buffer.slice(end + 1);
      const command = JSON.parse(line);
      const reply = await control.send(command);
      output(reply);
      if (
        ["finish", "abort"].includes(command.command) &&
        reply?.run === run &&
        ["passed", "stopped", "failed"].includes(reply.status)
      )
        return;
    }
  }
  control.destroy();
}

export async function stopChild(
  child: Child,
  naturalWait = 150_000,
): Promise<{
  forced: boolean;
  exit: { code: number | null; signal: NodeJS.Signals | null };
}> {
  const natural = await waitExit(child, naturalWait);
  if (natural) return { forced: false, exit: natural };
  if (child.process.pid) process.kill(-child.process.pid, "SIGTERM");
  const terminated = await waitExit(child, 10_000);
  if (terminated) return { forced: true, exit: terminated };
  if (child.process.pid) process.kill(-child.process.pid, "SIGKILL");
  const killed = await waitExit(child, 5_000);
  if (!killed) throw new Error("native harness child did not terminate");
  return { forced: true, exit: killed };
}

async function main() {
  const options = parseOptions(process.argv.slice(2));
  const versions = readFileSync(
    join(options.repository, ".tool-versions"),
    "utf8",
  );
  const problems = bunVersionProblems(
    versions,
    JSON.parse(readFileSync(join(options.repository, "package.json"), "utf8")),
    Bun.version,
  );
  if (problems.length) throw new Error(problems.join("; "));
  const node = versions.match(/^nodejs (\d+\.\d+\.\d+)$/m)?.[1];
  if (
    !node ||
    execFileSync("node", ["--version"], { encoding: "utf8" }).trim() !==
      `v${node}`
  )
    throw new Error("Node must match .tool-versions");
  const { identity, origin: requested, port: requestedPort } = prepare(options);
  const runDir = join(options.runRoot, identity.run);
  mkdirSync(runDir, { mode: 0o700 });
  process.stdout.write(`private native run: ${runDir}\n`);
  privateFile(join(runDir, "owner.json"), {
    owner: options.owner,
    run: identity.run,
    purpose: "finite native mini integration self-proof",
    createdAt: new Date().toISOString(),
  });
  chmodSync(runDir, 0o700);
  mkdirSync(join(runDir, "tmp"), { recursive: true, mode: 0o700 });
  chmodSync(join(runDir, "tmp"), 0o700);
  // Consume an immutable, formally published Git input, without a second implementation tree.
  const input = join(runDir, "client");
  mkdirSync(input, { mode: 0o700 });
  const revision = identity.clientRevision;
  const paths = [
    "orbit/src",
    "orbit/tools",
    "orbit/package.json",
    "orbit/bun.lock",
    "halo/tools/protocol.ts",
    "halo/protocol/openapi.json",
    "client/protocol",
  ];
  const archive = execFileSync(
    "git",
    ["-C", options.repository, "archive", "--format=tar", revision, ...paths],
    { maxBuffer: 32 * 1024 * 1024 },
  );
  execFileSync("tar", ["-x", "-C", input], { input: archive });
  if (
    sha256(readFileSync(join(input, "orbit/bun.lock"))) !==
    sha256(readFileSync(join(options.repository, "orbit/bun.lock")))
  )
    throw new Error(
      "published client dependency lock differs from prepared dependencies",
    );
  symlinkSync(
    join(options.repository, "orbit/node_modules"),
    join(input, "node_modules"),
  );
  execFileSync("node", [join(input, "orbit/tools/protocol.ts")], {
    cwd: input,
    timeout: 30_000,
    stdio: "pipe",
  });
  options.clientDir = input;
  const sourceBuild = `source-${treeHash(join(input, "orbit/src"))}.generated-${treeHash(join(input, "orbit/.generated"))}`;
  identity.clientBuild =
    options.mode === "external" && options.clientBuild
      ? boundedIdentity(options.clientBuild, "native client build")
      : sourceBuild;
  const port = requestedPort || (await freePort());
  const origin = requested || `https://127.0.0.1:${port}`;
  privateFile(join(runDir, "input.json"), {
    run: identity.run,
    mode: options.mode,
    serviceOrigin: origin,
    source: {
      sourceRevision: identity.sourceRevision,
      openapiSha256: identity.openapiSha256,
      clientRevision: identity.clientRevision,
      clientBuild: identity.clientBuild,
    },
    limits: { windowSeconds: 900, fixedSource: "203.0.113.77" },
    exportedClientBuild: sourceBuild,
    note:
      options.mode === "external"
        ? "Finite fixture only. Native client and TLS acceptance belong to the native owner."
        : "Non-native self-proof. Native client acceptance belongs to a separate owner.",
  });

  if (options.mode === "startup") {
    const blocker = createServer();
    await new Promise<void>((resolveListen, reject) => {
      blocker.once("error", reject);
      blocker.listen(port, "127.0.0.1", resolveListen);
    });
    const child = childHarness(options, identity, runDir, origin);
    const exited = await waitExit(child, 120_000);
    await new Promise<void>((resolveClosed) =>
      blocker.close(() => resolveClosed()),
    );
    if (!exited || exited.code === 0)
      throw new Error("startup failure was not observed");
    const terminal = readback(join(runDir, "tmp"), options.repository);
    if (!terminal.clean)
      throw new Error("startup resources did not terminate normally");
    if (existsSync(join(runDir, "evidence.json")))
      throw new Error("startup failure wrote scenario evidence");
    privateFile(join(runDir, "runner.json"), {
      run: identity.run,
      mode: options.mode,
      status: "passed",
      expectedStartupFailure: true,
      independentReadback: terminal,
      exit: exited,
    });
    privateFile(join(runDir, "supervisor.log"), child.log());
    // Failed startup retains diagnostics for an independent ownership readback.
    rmSync(join(runDir, "control.sock"), { force: true });
    return 0;
  }

  const child = childHarness(options, identity, runDir, origin);
  let control: LineSocket | undefined;
  let failure: Error | undefined;
  let report: any;
  let forced = false;
  try {
    await Promise.race([
      child.ready,
      child.exit.then((exit) => {
        throw new Error(
          `native harness exited before readiness (${exit.signal ?? exit.code})`,
        );
      }),
    ]);
    control = await connectControl(join(runDir, "control.sock"));
    if (options.mode !== "control") await hello(control, identity);
    if (options.mode === "full") {
      const driver = await import("./driver.ts");
      const observations = await driver.drive({
        control,
        run: identity.run,
        origin,
        relayBase: `http://127.0.0.1:${port}`,
        clientDir: options.clientDir,
      });
      report = await finish(control, identity, observations);
      if (report.status !== "passed")
        throw new Error(`driver scenario status ${String(report.status)}`);
    } else if (options.mode === "control") {
      await controlExercise(control, identity, runDir);
    } else if (options.mode === "external") {
      process.stdout.write(
        `native control ready: ${identity.run}; input one closed-schema JSON command per line\n`,
      );
      process.stdin.setEncoding("utf8");
      const interaction = externalCommands(
        process.stdin,
        control,
        identity.run,
        (reply) => {
          process.stdout.write(JSON.stringify(reply) + "\n");
        },
      );
      try {
        await bounded(Promise.race([interaction, child.exit]), 900_000);
      } finally {
        process.stdin.destroy();
      }
    } else if (options.mode === "cleanup_failure") {
      report = await finish(control, identity, {});
      if (report.status !== "failed" || report.cleanup?.postgresql !== false)
        throw new Error("cleanup interruption was not observed");
    } else if (["sigterm", "sigint", "deadline"].includes(options.mode)) {
      if (options.mode !== "deadline" && child.process.pid)
        process.kill(
          -child.process.pid,
          options.mode === "sigterm" ? "SIGTERM" : "SIGINT",
        );
      await bounded(child.exit, 900_000);
    } else {
      control.destroy();
      control = undefined;
    }
  } catch (error) {
    failure = error instanceof Error ? error : new Error(String(error));
    if (control) await abort(control, identity, "client_failure");
  } finally {
    control?.close();
  }

  const stopped = await stopChild(child);
  forced = stopped.forced;
  if (
    ["full", "control", "external"].includes(options.mode) &&
    stopped.exit.code !== 0 &&
    !failure
  )
    failure = new Error("foreground harness did not exit successfully");
  if (existsSync(join(runDir, "control.sock")) && !failure)
    failure = new Error("private control owner left its socket path behind");
  const evidencePath = join(runDir, "evidence.json");
  let evidence: any;
  if (existsSync(evidencePath))
    evidence = JSON.parse(readFileSync(evidencePath, "utf8"));
  const terminal = readback(join(runDir, "tmp"), options.repository);
  if (!terminal.clean && !failure)
    failure = new Error(
      "independent PostgreSQL readback failed or required fallback",
    );
  try {
    if (!failure) {
      if (!evidence) failure = new Error("native harness wrote no evidence");
      else if (options.mode === "full") validateFull(evidence);
      else if (options.mode === "external") {
        if (evidence.status === "passed") validateFull(evidence);
        else if (evidence.status === "stopped") validateCleanup(evidence);
        else throw new Error("external fixture did not confirm its outcome");
      } else if (
        ["disconnect", "sigterm", "sigint", "deadline"].includes(options.mode)
      ) {
        const expected =
          options.mode === "disconnect" ? "control_disconnect" : options.mode;
        if (
          evidence.status !== "failed" ||
          evidence.scenario?.outcome !== expected
        ) {
          throw new Error(
            "lifecycle failure did not follow the expected shutdown path",
          );
        }
        validateCleanup(evidence);
      }
    }
  } catch (error) {
    failure =
      error instanceof Error ? error : new Error("evidence validation failed");
  }
  if (forced && !failure)
    failure = new Error("native harness required forced process termination");
  const status = failure
    ? "failed"
    : options.mode === "external" && evidence?.status === "stopped"
      ? "stopped"
      : "passed";
  privateFile(join(runDir, "runner.json"), {
    run: identity.run,
    mode: options.mode,
    status,
    exit: stopped.exit,
    forcedTermination: forced,
    independentReadback: terminal,
    evidence: evidence?.status ?? null,
    failure: failure?.message ?? null,
  });
  privateFile(join(runDir, "supervisor.log"), child.log());
  // Only confirmed cleanup permits retirement; failure keeps all recovery material.
  if (
    !failure &&
    evidence?.cleanup?.postgresql === true &&
    evidence?.leaks === false
  ) {
    rmSync(join(runDir, "tmp"), { recursive: true });
  }
  if (failure) {
    process.stderr.write(
      `native fixture supervisor failed: ${failure.message}\n`,
    );
    return 1;
  }
  process.stdout.write(
    `native fixture ${identity.run} ${options.mode} ${status}\n`,
  );
  return 0;
}

if (import.meta.main) {
  const code = await main().catch((error) => {
    process.stderr.write(
      `${redact(error instanceof Error ? error.message : String(error))}\n`,
    );
    return 1;
  });
  process.exitCode = code;
}
