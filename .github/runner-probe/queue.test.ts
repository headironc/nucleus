import { expect, spyOn, test } from "bun:test";
import { data, Evaluator, Lexer, Parser } from "@actions/expressions";
import { truthy } from "@actions/expressions/result";
import { spawn, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { queueWindow } from "./queue";

const root = resolve(import.meta.dir, "../..");
type Step = {
  name?: string;
  uses?: string;
  run?: string;
  if?: string;
  with?: Record<string, string>;
};
const workflow = Bun.YAML.parse(
  readFileSync(join(root, ".github/workflows/runner-probe.yml"), "utf8"),
) as {
  concurrency: unknown;
  jobs: {
    platform: {
      name: string;
      needs?: unknown;
      concurrency?: unknown;
      environment?: unknown;
      strategy: {
        "max-parallel": number;
        "fail-fast": boolean;
        matrix: { cell: string };
      };
      steps: Step[];
    };
  };
};
const job = workflow.jobs.platform;
const env = {
  GITHUB_EVENT_NAME: "workflow_dispatch",
  GITHUB_REPOSITORY_ID: "1432619931",
  GITHUB_REPOSITORY: "headironc/nucleus",
  GITHUB_REF: "refs/heads/dev",
  GITHUB_RUN_ID: "123456",
  GITHUB_RUN_ATTEMPT: "2",
  GITHUB_SHA: "a".repeat(40),
  GITHUB_WORKFLOW_SHA: "a".repeat(40),
  EXPECTED_REVISION: "a".repeat(40),
  QUEUE_PROBE: "true",
  QUEUE_CELL: "one",
  FAIL_AFTER_CHECKS: "false",
  CANCEL_AFTER_CHECKS: "false",
  TIMEOUT_AFTER_CHECKS: "false",
};
function evaluate(
  source: string,
  enabled?: boolean,
  cell = "single",
  success = true,
) {
  const inputs = new data.Dictionary();
  if (enabled !== undefined)
    inputs.add("queue_probe", new data.BooleanData(enabled));
  const functions = [
    {
      name: "success",
      minArgs: 0,
      maxArgs: 0,
      call: () => new data.BooleanData(success),
    },
  ];
  return new Evaluator(
    new Parser(
      new Lexer(source.replace(/^\$\{\{ | \}\}$/g, "")).lex().tokens,
      ["inputs", "matrix", "github"],
      functions,
    ).parse(),
    new data.Dictionary(
      { key: "inputs", value: inputs },
      {
        key: "matrix",
        value: new data.Dictionary({
          key: "cell",
          value: new data.StringData(cell),
        }),
      },
      {
        key: "github",
        value: new data.Dictionary(
          { key: "run_id", value: new data.StringData(env.GITHUB_RUN_ID) },
          {
            key: "run_attempt",
            value: new data.StringData(env.GITHUB_RUN_ATTEMPT),
          },
        ),
      },
    ),
    new Map(functions.map((fn) => [fn.name, fn])),
  ).evaluate();
}

test("one dispatch exposes two independent pool jobs while default naming stays compatible", () => {
  expect(job.strategy["max-parallel"]).toBe(2);
  expect(job.strategy["fail-fast"]).toBe(false);
  expect(job.needs).toBeUndefined();
  expect(job.concurrency).toBeUndefined();
  expect(job.environment).toBeUndefined();
  expect(workflow.concurrency).toEqual({
    group: "nucleus-runner-first-consumer",
    "cancel-in-progress": false,
  });
  const upload = job.steps.findLast((step) =>
    step.uses?.startsWith("actions/upload-artifact@"),
  );
  for (const enabled of [undefined, false, true]) {
    const cells = evaluate(job.strategy.matrix.cell, enabled) as data.Array;
    expect(cells.values().map((cell) => cell.coerceString())).toEqual(
      enabled ? ["one", "two"] : ["single"],
    );
    const names: string[] = [];
    for (const cell of cells.values().map((cell) => cell.coerceString())) {
      expect(evaluate(job.name, enabled, cell).coerceString()).toBe(
        enabled ? `platform (${cell})` : "platform",
      );
      const name = evaluate(upload!.with!.name!, enabled, cell).coerceString();
      expect(name).toBe(`runner-probe-123456-2${enabled ? `-${cell}` : ""}`);
      names.push(name);
    }
    expect(new Set(names).size).toBe(cells.values().length);
  }
});

test("queue conflicts fail in the actual first step before preparing private resources", () => {
  const guard = job.steps[0]!;
  expect(guard.name).toBe("Bind reviewed dev revision");
  for (let mask = 0; mask < 16; mask++) {
    const values = [
      "FAIL_AFTER_CHECKS",
      "CANCEL_AFTER_CHECKS",
      "TIMEOUT_AFTER_CHECKS",
      "QUEUE_PROBE",
    ];
    const modes = Object.fromEntries(
      values.map((key, i) => [key, String(Boolean(mask & (1 << i)))]),
    );
    const result = spawnSync(
      "bash",
      ["--noprofile", "--norc", "-eo", "pipefail", "-c", guard.run!],
      {
        env: { ...process.env, ...env, ...modes },
        encoding: "utf8",
        timeout: 5000,
      },
    );
    expect(result.status).toBe(
      values.filter((key) => modes[key] === "true").length > 1 ? 1 : 0,
    );
  }
});

test("only successful complete checks and cleanup can enter the queue window", () => {
  const index = (name: string) =>
    job.steps.findIndex((step) => step.name === name);
  const hold = job.steps[index("Hold queue observation window")]!;
  expect(index("Check complete platform package")).toBeLessThan(
    index("Check and clean private PostgreSQL scope"),
  );
  expect(index("Check and clean private PostgreSQL scope")).toBeLessThan(
    index(hold.name!),
  );
  const uploadIndex = job.steps.findLastIndex((step) =>
    step.uses?.startsWith("actions/upload-artifact@"),
  );
  expect(index(hold.name!)).toBeLessThan(uploadIndex);
  expect(hold.run).toBe(
    'bun .github/runner-probe/queue.ts 2>&1 | tee "$RUNNER_TEMP/runner-probe-queue.log"',
  );
  for (const enabled of [undefined, false, true])
    for (const successful of [false, true])
      expect(truthy(evaluate(hold.if!, enabled, "one", successful))).toBe(
        enabled === true && successful,
      );
});

test("both cells hold the full fixed window and emit bound completion only after release", async () => {
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    for (const cell of ["one", "two"]) {
      log.mockClear();
      const controller = new AbortController();
      let release!: () => void;
      let duration: number | undefined;
      const barrier = new Promise<void>((resolve) => {
        release = resolve;
      });
      const waiting = queueWindow(
        { ...env, QUEUE_CELL: cell },
        controller.signal,
        (ms, signal) => {
          expect(signal).toBe(controller.signal);
          duration = ms;
          return barrier;
        },
      );
      expect(duration).toBe(300_000);
      const identity = `run=123456 attempt=2 source=${env.GITHUB_SHA} cell=${cell} duration_seconds=300`;
      expect(log.mock.calls).toEqual([
        [`RUNNER_PROBE_QUEUE_WINDOW_STARTED ${identity}`],
      ]);
      release();
      await waiting;
      expect(log.mock.calls).toEqual([
        [`RUNNER_PROBE_QUEUE_WINDOW_STARTED ${identity}`],
        [`RUNNER_PROBE_QUEUE_WINDOW_COMPLETED ${identity}`],
      ]);
    }
  } finally {
    log.mockRestore();
  }
});

test("invalid identities, cell and mode never enter a queue window", async () => {
  const log = spyOn(console, "log").mockImplementation(() => {});
  let waits = 0;
  try {
    for (const key of Object.keys(env).filter(
      (key) =>
        ![
          "GITHUB_EVENT_NAME",
          "GITHUB_REPOSITORY_ID",
          "GITHUB_REPOSITORY",
          "GITHUB_REF",
        ].includes(key),
    )) {
      for (const value of ["", "$(exit 0)", "bad\nvalue"]) {
        await expect(
          queueWindow(
            { ...env, [key]: value },
            new AbortController().signal,
            async () => {
              waits++;
            },
          ),
        ).rejects.toThrow();
      }
    }
    for (const key of [
      "FAIL_AFTER_CHECKS",
      "CANCEL_AFTER_CHECKS",
      "TIMEOUT_AFTER_CHECKS",
    ])
      await expect(
        queueWindow(
          { ...env, [key]: "true" },
          new AbortController().signal,
          async () => {
            waits++;
          },
        ),
      ).rejects.toThrow();
    expect(waits).toBe(0);
    expect(log).not.toHaveBeenCalled();
  } finally {
    log.mockRestore();
  }
});

test("an aborted or failed window never emits a completion marker", async () => {
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    const controller = new AbortController();
    await expect(
      queueWindow(env, controller.signal, async () => {
        controller.abort();
      }),
    ).rejects.toThrow();
    expect(log.mock.calls).toHaveLength(1);
    await expect(queueWindow(env, controller.signal)).rejects.toThrow();
    expect(log.mock.calls).toHaveLength(1);
    await expect(
      queueWindow(env, new AbortController().signal, async () => {
        throw new Error("clock failed");
      }),
    ).rejects.toThrow("clock failed");
    expect(log.mock.calls).toHaveLength(2);
  } finally {
    log.mockRestore();
  }
});

for (const [signal, code] of [
  ["SIGINT", 130],
  ["SIGTERM", 143],
] as const) {
  test(`actual queue process fails and exits promptly on ${signal}`, async () => {
    const child = spawn(process.execPath, [join(import.meta.dir, "queue.ts")], {
      cwd: root,
      env: { ...process.env, ...env },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let output = "";
    const exited = new Promise<number | null>((resolve, reject) => {
      child.once("error", reject);
      child.once("exit", resolve);
    });
    const ready = new Promise<void>((resolve) => {
      child.stdout.on("data", (chunk) => {
        output += chunk;
        if (output.includes("RUNNER_PROBE_QUEUE_WINDOW_STARTED")) resolve();
      });
    });
    child.stderr.on("data", (chunk) => {
      output += chunk;
    });
    const timeout = setTimeout(() => child.kill("SIGKILL"), 5000);
    try {
      await Promise.race([
        ready,
        exited.then(() => {
          throw new Error(`Queue process exited before ready: ${output}`);
        }),
      ]);
      expect(child.kill(signal)).toBe(true);
      expect(await exited).toBe(code);
      expect(output).toContain("RUNNER_PROBE_QUEUE_WINDOW_STARTED");
      expect(output).not.toContain("RUNNER_PROBE_QUEUE_WINDOW_COMPLETED");
    } finally {
      clearTimeout(timeout);
      if (child.exitCode === null && child.signalCode === null)
        child.kill("SIGKILL");
      await exited;
    }
  }, 10_000);
}
