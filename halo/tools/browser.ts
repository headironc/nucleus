import { appendFile, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:net";
import * as http from "node:http";
import * as https from "node:https";
import { createHash } from "node:crypto";
import { assetLocations } from "./nginx.ts";
import { waitForNginx, waitForNodeReady } from "./browser-ready.ts";
import {
  EvidenceLog,
  ensureRun,
  readJsonl,
  summarizeReporter,
  verifyReporter,
  writeJsonAtomic,
} from "./evidence.ts";
import {
  bounded,
  waitForChildExit,
  type ExitObservation,
} from "./process.ts";
import {
  RunnerLifecycle,
  type Managed,
  type ServiceTask,
} from "./runner-lifecycle.ts";
import { redactText } from "./redact.ts";

// Allow bounded variation across the full suite, including real SMS cooldowns.
// The Rust fixture retains another 120 seconds for cleanup and final assertions.
const EXECUTION_DEADLINE_MS = 660_000;
const CHILD_TERM_MS = 5_000;
const CHILD_KILL_MS = 2_000;
const SERVICE_CLOSE_MS = 5_000;
const EVIDENCE_DRAIN_MS = 5_000;
const TEMP_REMOVE_MS = 5_000;

const run = (() => {
  try {
    return ensureRun();
  } catch (error) {
    console.error(
      `Cannot initialize browser evidence: ${error instanceof Error ? error.message : error}`,
    );
    process.exit(1);
  }
})();
const runner = new EvidenceLog(run, "runner.jsonl");
const startedAt = new Date().toISOString();
let evidenceFailure = false;

function record(event: string, data: Record<string, unknown> = {}): void {
  try {
    runner.append(event, data);
  } catch (error) {
    evidenceFailure = true;
    console.error(
      `Browser evidence write failed: ${error instanceof Error ? error.message : error}`,
    );
  }
}

function message(error: unknown): string {
  return redactText(error instanceof Error ? error.message : String(error)).text;
}

let playwrightExit: ExitObservation | null = null;

function recordExit(observation: ExitObservation): void {
  const previous = playwrightExit;
  if (
    previous?.observed &&
    observation.observed &&
    previous.code === observation.code &&
    previous.signal === observation.signal
  )
    return;
  playwrightExit = observation;
  record("child_exit", { label: "playwright", ...observation });
  if (previous && previous.observed !== observation.observed)
    record("child_exit_updated", { previous, next: observation });
}

const lifecycle = new RunnerLifecycle({
  deadlineMs: EXECUTION_DEADLINE_MS,
  termMs: CHILD_TERM_MS,
  killMs: CHILD_KILL_MS,
  serviceMs: SERVICE_CLOSE_MS,
  drainMs: EVIDENCE_DRAIN_MS,
  removeMs: TEMP_REMOVE_MS,
  record,
  onChildTerminated: (item: Managed, termination) => {
    if (item.label === "playwright" && termination.observed)
      recordExit({
        observed: true,
        code: termination.code,
        signal: termination.signal,
      });
  },
});

const nginx = process.env.HALO_NGINX;
const backend = process.env.HALO_TEST_BACKEND;
const port = Number(process.env.HALO_TEST_PORT);
const mediaPort = Number(process.env.HALO_TEST_MEDIA_PORT);
const avatarOrigin = process.env.HALO_AVATAR_ORIGIN;
const material = process.env.HALO_TEST_MATERIAL;
if (
  !nginx ||
  !backend ||
  !/^http:\/\/127\.0\.0\.1:\d+$/.test(backend) ||
  !Number.isInteger(port) ||
  !Number.isInteger(mediaPort) ||
  mediaPort === port ||
  avatarOrigin !== `https://127.0.0.1:${mediaPort}` ||
  !material
) {
  const reason =
    "HALO_NGINX, media origin and material are required by the isolated Rust website test";
  record("configuration_error", { message: reason });
  record("run_end", { ok: false, reasons: ["configuration_error"] });
  process.exit(1);
}

lifecycle.start();
record("run_start", {
  pid: process.pid,
  startedAt,
  executionDeadlineMs: EXECUTION_DEADLINE_MS,
  runId: run.runId,
});

async function start(
  label: Managed["label"],
  command: string,
  args: string[],
  options: Parameters<typeof spawn>[2],
  group: boolean,
): Promise<ChildProcess> {
  if (lifecycle.cancellation.signal.aborted) {
    record("launch_skipped", { label, reason: lifecycle.cancellationReason });
    throw new Error(`Acceptance cancelled before ${label} launch`);
  }
  const child = spawn(command, args, { ...options, detached: group });
  const item: Managed = { label, child, group, spawnFailed: false };
  lifecycle.add(item);
  record("child_spawn", {
    label,
    pid: child.pid ?? null,
    command,
    args,
    group,
  });
  try {
    await once(child, "spawn");
  } catch (error) {
    item.spawnFailed = true;
    record("child_spawn_error", { label, message: message(error) });
    throw error;
  }
  return child;
}

async function closeServer(
  server: http.Server | https.Server,
  label: string,
): Promise<Record<string, unknown>> {
  try {
    server.closeAllConnections();
  } catch {
    // Still attempt the owned close below.
  }
  const closed = new Promise<void>((resolve, reject) => {
    server.close((error) => {
      if (!error || (error as NodeJS.ErrnoException).code === "ERR_SERVER_NOT_RUNNING")
        resolve();
      else reject(error);
    });
  });
  const result = await bounded(closed, SERVICE_CLOSE_MS, `${label} close`);
  if (result.ok) return { label, outcome: "closed" };
  try {
    server.closeAllConnections();
  } catch {
    // The bounded close already reported unknown.
  }
  return { label, outcome: "unknown", error: result.error };
}

type Cleanup = {
  ok: boolean;
  results: Array<Record<string, unknown>>;
};

async function cleanupAll(temporary: string | null): Promise<Cleanup> {
  const services: ServiceTask[] = [];
  if (cacheProbe)
    services.push({
      label: "cache_probe",
      run: () => closeServer(cacheProbe!, "cache_probe"),
    });
  if (mediaServer)
    services.push({
      label: "media",
      run: async () => {
        const closed = closeServer(mediaServer!, "media");
        const requests = [...mediaRequests.entries()];
        for (const [request] of requests) request.destroy();
        await Promise.all(requests.map(([, finished]) => finished));
        return closed;
      },
    });
  return lifecycle.cleanupAll(temporary, {
    services,
    drains: [{ label: "media_evidence", run: () => mediaEvidence }],
  });
}

let temporary: string | null = null;
let cacheProbe: http.Server | undefined;
let mediaServer: https.Server | undefined;
const mediaRequests = new Map<http.ClientRequest, Promise<void>>();
const mediaLog = join(material, "media.jsonl");
let mediaEvidence = Promise.resolve();
const mediaRecord = (entry: Record<string, unknown>) => {
  mediaEvidence = mediaEvidence.then(() =>
    appendFile(mediaLog, `${JSON.stringify(entry)}\n`),
  );
  return mediaEvidence;
};
let primaryError: string | null = null;
try {
  const directory = await mkdtemp(join(tmpdir(), "halo-browser-"));
  temporary = directory;
  record("temporary_created", { directory });
  const sockets = createServer();
  sockets.listen(0, "127.0.0.1");
  await once(sockets, "listening");
  const address = sockets.address();
  if (!address || typeof address === "string")
    throw new Error("Missing fixture socket");
  const nodePort = address.port;
  await new Promise<void>((resolve, reject) =>
    sockets.close((error) => (error ? reject(error) : resolve())),
  );
  lifecycle.cancellation.signal.throwIfAborted();
  const certificate = spawnSync(
    "openssl",
    [
      "req",
      "-x509",
      "-newkey",
      "rsa:2048",
      "-nodes",
      "-keyout",
      join(directory, "key.pem"),
      "-out",
      join(directory, "cert.pem"),
      "-days",
      "1",
      "-subj",
      "/CN=127.0.0.1",
      "-addext",
      "subjectAltName=IP:127.0.0.1",
    ],
    { stdio: "ignore", timeout: 10_000 },
  );
  record("fixture_step", {
    step: "openssl",
    status: certificate.status,
    signal: certificate.signal ?? null,
  });
  if (certificate.status !== 0)
    throw new Error("Fixture TLS generation failed");
  lifecycle.cancellation.signal.throwIfAborted();
  const media = https.createServer(
    {
      key: await readFile(join(directory, "key.pem")),
      cert: await readFile(join(directory, "cert.pem")),
    },
    (request, response) => {
      const url = new URL(request.url ?? "/", avatarOrigin);
      if (
        request.method !== "GET" ||
        !url.pathname.startsWith("/api/v1/avatars/")
      ) {
        response.writeHead(404, { "Content-Type": "text/plain" });
        response.end();
        return;
      }
      const upstream = http.request(
        new URL(`${backend}${request.url}`),
        {
          method: "GET",
          headers: {
            ...(request.headers.cookie
              ? { cookie: request.headers.cookie }
              : {}),
            ...(request.headers.accept
              ? { accept: request.headers.accept }
              : {}),
            // A fictional source for the direct media entry.
            "x-real-ip": "192.0.2.211",
          },
        },
        (result) => {
          const chunks: Buffer[] = [];
          result.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
          result.on("end", () => {
            const body = Buffer.concat(chunks);
            const headers: Record<string, string> = {
              "Content-Length": String(body.length),
            };
            const header = (name: string) => {
              const value = result.headers[name];
              return Array.isArray(value) ? value.join(", ") : value;
            };
            const contentType = header("content-type");
            const cacheControl = header("cache-control");
            const nosniff = header("x-content-type-options");
            if (contentType) headers["Content-Type"] = contentType;
            if (cacheControl) headers["Cache-Control"] = cacheControl;
            // Forward the backend's header exactly; do not create proof here.
            if (nosniff) headers["X-Content-Type-Options"] = nosniff;
            let code: string | null = null;
            if (contentType?.toLowerCase().startsWith("application/json")) {
              try {
                const parsed = JSON.parse(body.toString("utf8"));
                if (typeof parsed?.code === "string") code = parsed.code;
              } catch {
                code = null;
              }
            }
            void mediaRecord({
              method: "GET",
              path: url.pathname,
              authorization: Boolean(request.headers.authorization),
              sessionId: Boolean(request.headers["session-id"]),
              operationId: Boolean(request.headers["operation-id"]),
              cookie: Boolean(request.headers.cookie),
              status: result.statusCode ?? 502,
              contentType: contentType ?? null,
              cacheControl: cacheControl ?? null,
              setCookie: header("set-cookie") !== undefined,
              nosniff: nosniff ?? null,
              code,
              bytes: body.length,
              sha256: createHash("sha256").update(body).digest("hex"),
            }).catch(() => {});
            response.writeHead(result.statusCode ?? 502, headers);
            response.end(body);
          });
        },
      );
      upstream.on("error", () => {
        if (!response.headersSent)
          response.writeHead(502, { "Content-Type": "text/plain" });
        response.end();
      });
      mediaRequests.set(upstream, new Promise<void>((resolve) => {
        upstream.once("close", () => {
          mediaRequests.delete(upstream);
          resolve();
        });
      }));
      upstream.end();
    },
  );
  mediaServer = media;
  media.listen(mediaPort, "127.0.0.1");
  await once(media, "listening");
  lifecycle.cancellation.signal.throwIfAborted();
  // Only this fixture response stands in for a private backend byte resource.
  const cache = http.createServer((request, response) => {
    if (request.url !== "/api/__halo_cache_fixture") {
      response.writeHead(404);
      response.end();
      return;
    }
    response.writeHead(200, {
      "Content-Type": "application/octet-stream",
      "Cache-Control": "private, max-age=60, immutable",
    });
    response.end("fictional-private-bytes");
  });
  cacheProbe = cache;
  cache.listen(0, "127.0.0.1");
  await once(cache, "listening");
  const cacheAddress = cache.address();
  if (!cacheAddress || typeof cacheAddress === "string")
    throw new Error("Missing cache fixture socket");
  const assetConfig = await assetLocations(resolve("."));
  const production = await (
    await import("node:fs/promises")
  ).readFile("deploy/halo.nginx.conf", "utf8");
  const routes = production
    .replace("include /srv/halo/current/build/nginx-assets.conf;", assetConfig)
    .replace("http://127.0.0.1:8080", "$halo_fixture_backend")
    .replace("http://127.0.0.1:3080", `http://127.0.0.1:${nodePort}`)
    // Independent fictional visitors share one loopback socket in this fixture.
    .replace(
      "proxy_set_header X-Real-IP $remote_addr;",
      "proxy_set_header X-Real-IP $halo_fixture_source;",
    );
  const config = join(directory, "nginx.conf");
  await writeFile(
    config,
    `daemon off; master_process off; pid ${directory}/nginx.pid;
error_log ${directory}/nginx.log crit;
events { worker_connections 128; }
http { client_body_temp_path ${directory}/body; proxy_temp_path ${directory}/proxy;
fastcgi_temp_path ${directory}/fastcgi; scgi_temp_path ${directory}/scgi; uwsgi_temp_path ${directory}/uwsgi;
map $uri $halo_fixture_backend {
default ${backend};
/api/__halo_cache_fixture http://127.0.0.1:${cacheAddress.port};
}
map $http_x_halo_fixture_source $halo_fixture_source {
default 192.0.2.200;
${Array.from({ length: 16 }, (_, index) => `${String(index + 1).padStart(2, "0")} 192.0.2.${index + 1};`).join("\n")}
}
server { listen 127.0.0.1:${port} ssl; server_name 127.0.0.1;
ssl_certificate ${directory}/cert.pem; ssl_certificate_key ${directory}/key.pem;
${routes}
} }\n`,
  );
  lifecycle.cancellation.signal.throwIfAborted();
  const validation = spawnSync(nginx, ["-t", "-c", config], {
    stdio: "inherit",
    timeout: 10_000,
  });
  record("fixture_step", {
    step: "nginx_config",
    status: validation.status,
    signal: validation.signal ?? null,
  });
  if (validation.status !== 0) throw new Error("Nginx config invalid");
  lifecycle.cancellation.signal.throwIfAborted();

  const node = await start(
    "node",
    process.execPath,
    ["server.mjs"],
    {
      env: {
        ...process.env,
        NODE_ENV: "production",
        HALO_PORT: String(nodePort),
        HALO_AVATAR_ORIGIN: avatarOrigin,
      },
      stdio: ["ignore", "pipe", "inherit"],
    },
    false,
  );
  try {
    await waitForNodeReady(node, "halo_ready", lifecycle.cancellation.signal);
    record("readiness", { service: "node", ok: true });
  } catch (error) {
    record("readiness", { service: "node", ok: false, message: message(error) });
    throw error;
  }
  const authority = await readFile(join(directory, "cert.pem"));
  const proxy = await start(
    "nginx",
    nginx,
    ["-c", config],
    { stdio: "inherit" },
    false,
  );
  try {
    await waitForNginx(
      proxy,
      `https://127.0.0.1:${port}`,
      authority,
      lifecycle.cancellation.signal,
    );
    record("readiness", { service: "nginx", ok: true });
  } catch (error) {
    record("readiness", { service: "nginx", ok: false, message: message(error) });
    throw error;
  }
  lifecycle.cancellation.signal.throwIfAborted();
  const test = await start(
    "playwright",
    process.execPath,
    ["node_modules/@playwright/test/cli.js", "test"],
    {
      env: {
        ...process.env,
        HALO_TEST_NODE: `http://127.0.0.1:${nodePort}`,
        HALO_TEST_ORIGIN: `https://127.0.0.1:${port}`,
        HALO_TEST_MEDIA_LOG: mediaLog,
        HALO_TEST_EVIDENCE: run.root,
        HALO_TEST_RUN_ID: run.runId,
        HALO_TEST_EVIDENCE_DIR: run.dir,
      },
      stdio: "inherit",
    },
    true,
  );
  const exited = waitForChildExit(test);
  const cancelled = new Promise<"cancelled">((resolve) => {
    if (lifecycle.cancellation.signal.aborted) resolve("cancelled");
    else
      lifecycle.cancellation.signal.addEventListener(
        "abort",
        () => resolve("cancelled"),
        { once: true },
      );
  });
  const outcome = await Promise.race([
    exited.then((observation) => ({ kind: "exit" as const, observation })),
    cancelled.then(() => ({ kind: "cancelled" as const })),
  ]);
  if (outcome.kind === "exit") recordExit(outcome.observation);
  else
    record("waiting_for_cleanup", {
      reason: lifecycle.cancellationReason,
      message: "Playwright is terminated by the bounded cleanup path",
    });
} catch (error) {
  primaryError = message(error);
  record("run_error", { message: primaryError });
} finally {
  let cleanup: Cleanup = { ok: false, results: [] };
  try {
    cleanup = await cleanupAll(temporary);
  } catch (error) {
    const detail = message(error);
    record("cleanup_error", { message: detail });
    cleanup = {
      ok: false,
      results: [{ label: "cleanup", outcome: "unknown", error: detail }],
    };
  }
  try {
  const reportRead = readJsonl(join(run.dir, "reporter.jsonl"), run.runId);
  const report = summarizeReporter(reportRead.records, run.runId, reportRead);
  record("report_summary", {
    inventory: report.inventoryCount,
    counts: report.counts,
    finalized: report.finalized,
    reportStatus: report.reportStatus,
    writeErrors: report.writeErrors,
    malformed: report.malformed,
    foreign: report.foreign,
    undeclared: report.undeclared,
    inFlight: report.tests
      .filter((test) => test.state === "interrupted_in_flight")
      .map((test) => test.titlePath),
    neverStarted: report.tests
      .filter((test) => test.state === "never_started")
      .map((test) => test.titlePath),
    startUnknown: report.tests
      .filter((test) => test.state === "start_unknown")
      .map((test) => test.titlePath),
  });
  // The assignment happens in recordExit; keep the read explicitly typed so
  // control-flow narrowing does not collapse it to the initial null.
  const exit = playwrightExit as ExitObservation | null;
  const cleanExit =
    exit?.observed === true && exit.code === 0 && exit.signal === null;
  const verification = verifyReporter(report, { childExitCleanly: cleanExit });
  record("report_verification", {
    ok: verification.ok,
    reasons: verification.reasons,
  });
  const reasons: string[] = [];
  if (!exit?.observed) reasons.push("playwright_exit_not_observed");
  else if (!cleanExit)
    reasons.push(`playwright_exit:${exit.code ?? exit.signal}`);
  if (lifecycle.cancellationReason)
    reasons.push(`cancelled:${lifecycle.cancellationReason}`);
  reasons.push(...verification.reasons);
  if (!cleanup.ok) reasons.push("cleanup_incomplete");
  if (primaryError && !lifecycle.cancellationReason)
    reasons.push(`runner_error:${primaryError}`);
  const finishedAt = new Date().toISOString();
  const summary = {
    version: 1,
    runId: run.runId,
    ok: reasons.length === 0 && !evidenceFailure,
    reasons,
    startedAt,
    finishedAt,
    executionDeadlineMs: EXECUTION_DEADLINE_MS,
    cancellation: lifecycle.cancellationReason,
    exit,
    cleanup,
    report: {
      inventory: report.inventoryCount,
      counts: report.counts,
      finalized: report.finalized,
      status: report.reportStatus,
      writeErrors: report.writeErrors,
      malformed: report.malformed,
      foreign: report.foreign,
      undeclared: report.undeclared,
      inFlight: report.tests
        .filter((test) => test.state === "interrupted_in_flight")
        .map((test) => test.titlePath),
      neverStarted: report.tests
        .filter((test) => test.state === "never_started")
        .map((test) => test.titlePath),
      startUnknown: report.tests
        .filter((test) => test.state === "start_unknown")
        .map((test) => test.titlePath),
    },
  };
  try {
    writeJsonAtomic(join(run.dir, "summary.json"), summary);
  } catch (error) {
    evidenceFailure = true;
    reasons.push(`summary_write_failed:${message(error)}`);
    console.error(`Browser evidence summary write failed: ${message(error)}`);
  }
  record("run_end", {
    ok: reasons.length === 0 && !evidenceFailure,
    reasons,
    cancellation: lifecycle.cancellationReason,
    exit,
    cleanup: { ok: cleanup.ok },
    report: summary.report,
  });
  lifecycle.stop();
  process.exit(reasons.length === 0 && !evidenceFailure ? 0 : 1);
  } catch (error) {
    console.error(`Browser evidence finalization failed: ${message(error)}`);
    process.exit(1);
  }
}
