import { afterEach, expect, test } from "bun:test";
import { data, Evaluator, Lexer, Parser } from "@actions/expressions";
import { truthy } from "@actions/expressions/result";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

type Step = {
  id?: string;
  if?: string;
  uses?: string;
  run?: string;
  env?: Record<string, string>;
  "working-directory"?: string;
  with?: Record<string, unknown>;
  "continue-on-error"?: boolean;
};
type Setup = {
  inputs?: Record<string, { default?: string }>;
  outputs: Record<string, { value: string }>;
  runs: { steps: Step[] };
};
type Workflow = {
  on: {
    push: { branches: string[]; paths: string[] };
    workflow_dispatch: null;
  };
  permissions: Record<string, string>;
  jobs: Record<
    string,
    {
      if?: string;
      needs?: string;
      "runs-on": string;
      permissions?: Record<string, string>;
      container?: {
        image: string;
        options: string;
      };
      env: Record<string, string>;
      outputs?: Record<string, string>;
      strategy?: { matrix: Record<string, unknown> | string };
      steps: Step[];
    }
  >;
};
function workflow(name: string): Workflow {
  return Bun.YAML.parse(
    readFileSync(
      new URL(`../.github/workflows/${name}.yml`, import.meta.url),
      "utf8",
    ),
  ) as Workflow;
}
const cache = workflow("cache");
const rust = workflow("rust");
const halo = workflow("halo");
const rustSetup = Bun.YAML.parse(
  readFileSync(
    new URL("../.github/actions/rust-setup/action.yml", import.meta.url),
    "utf8",
  ),
) as Setup;
const haloSetup = Bun.YAML.parse(
  readFileSync(
    new URL("../.github/actions/halo-setup/action.yml", import.meta.url),
    "utf8",
  ),
) as Setup;
const packageSetup = Bun.YAML.parse(
  readFileSync(
    new URL("../.github/actions/package-setup/action.yml", import.meta.url),
    "utf8",
  ),
) as Setup;
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

function fixture(packageName = "app") {
  const directory = mkdtempSync(join(tmpdir(), "nucleus cache "));
  directories.push(directory);
  function write(path: string, content: string) {
    const destination = join(directory, path);
    mkdirSync(dirname(destination), { recursive: true });
    writeFileSync(destination, content);
  }
  write(
    "rust-toolchain.toml",
    readFileSync(new URL("../rust-toolchain.toml", import.meta.url), "utf8"),
  );
  write(
    "Cargo.toml",
    '[workspace]\nmembers = ["app"]\nexclude = ["dependency"]\nresolver = "3"\n',
  );
  write(
    "dependency/Cargo.toml",
    '[package]\nname = "cache-dependency"\nversion = "0.1.0"\nedition = "2024"\n',
  );
  write("dependency/src/lib.rs", "pub fn value() -> u8 { 7 }\n");
  write(
    "app/Cargo.toml",
    `[package]\nname = "${packageName}"\nversion = "0.1.0"\nedition = "2024"\n[dependencies]\ncache-dependency = { path = "../dependency" }\n`,
  );
  write(
    "app/src/lib.rs",
    'pub fn value() -> u8 { cache_dependency::value() }\n#[test]\nfn must_not_run() { panic!("cache preparation ran a test"); }\n',
  );
  function run(script: string) {
    return spawnSync(
      "bash",
      ["--noprofile", "--norc", "-e", "-o", "pipefail", "-c", script],
      {
        cwd: directory,
        env: {
          ...process.env,
          CARGO_NET_OFFLINE: "true",
          CARGO_TARGET_DIR: join(directory, "target"),
          PACKAGE: "app",
        },
        encoding: "utf8",
        timeout: 20_000,
      },
    );
  }
  const lock = run("cargo generate-lockfile --offline");
  expect(lock.error).toBeUndefined();
  expect(lock.status, lock.stderr).toBe(0);
  return { write, run };
}

function compile(job: string) {
  const step = cache.jobs[job].steps.find((step) => step.id === "compile");
  expect(step?.run).toBeString();
  return step!.run!;
}

function expression(source: string, values: Record<string, unknown>) {
  const tokens = new Lexer(source.slice(3, -2)).lex().tokens;
  const parsed = new Parser(tokens, Object.keys(values), []).parse();
  const context = JSON.parse(
    JSON.stringify(values),
    data.reviver,
  ) as data.Dictionary;
  return new Evaluator(parsed, context).evaluate();
}

// Run the cache boundary from the actual composite and producer expressions.
// The controlled Action double models the pinned provider's outputs and per-step
// post state; Cargo commands still execute against the small offline fixture.
function prepare(
  job: string,
  probe: string | "failure" | "cancelled",
  restored = "false",
  consumer = false,
) {
  const setup = { checks: rustSetup, packages: packageSetup, halo: haloSetup }[
    job
  ]!;
  const action = `./.github/actions/${{ checks: "rust", packages: "package", halo: "halo" }[job]}-setup`;
  const caller = (
    consumer
      ? job === "halo"
        ? halo.jobs.website
        : rust.jobs[job]
      : cache.jobs[job]
  ).steps.find((step) => step.uses === action)!;
  const inputs = Object.fromEntries(
    Object.entries(setup.inputs ?? {}).map(([name, input]) => [
      name,
      input.default ?? "",
    ]),
  );
  Object.assign(
    inputs,
    Object.fromEntries(
      Object.entries(caller.with ?? {}).map(([name, value]) => [
        name,
        String(value),
      ]),
    ),
  );
  const steps: Record<string, { outputs: Record<string, string> }> = {};
  const events: string[] = [];
  const posts: { id: string; save: boolean; dirty: boolean }[] = [];
  let status = "success";
  for (const step of setup.runs.steps.filter((step) =>
    step.uses?.startsWith("Swatinem/rust-cache@"),
  )) {
    expect(step["continue-on-error"]).toBeUndefined();
    // GitHub supplies success() to step conditions without a status function.
    if (status !== "success") continue;
    if (step.if && !truthy(expression(step.if, { inputs, steps }))) continue;
    const lookup = String(step.with?.["lookup-only"] ?? "false") === "true";
    const hit = lookup ? probe : restored;
    events.push(lookup ? "lookup" : "restore");
    posts.push({
      id: step.id!,
      save: String(step.with?.["save-if"] ?? "true") === "true",
      dirty: hit !== "true",
    });
    if (hit === "failure" || hit === "cancelled") {
      status = hit;
      continue;
    }
    if (!lookup && hit !== "missing") events.push("download");
    steps[step.id!] = {
      outputs: { "cache-hit": hit === "missing" ? "false" : hit },
    };
  }
  const hit = expression(setup.outputs["cache-hit"].value, {
    steps,
  }).coerceString();
  if (status === "success") {
    const compilation = cache.jobs[job].steps.find(
      (step) => step.id === "compile",
    )!;
    if (
      consumer ||
      truthy(
        expression(compilation.if!, {
          steps: { setup: { outputs: { "cache-hit": hit } } },
        }),
      )
    ) {
      const { write, run } = fixture(job === "halo" ? "nucleus" : "app");
      if (job === "halo")
        write("app/tests/http.rs", "#[test]\nfn acceptance() {}\n");
      const result = run(
        consumer
          ? "cargo check --workspace --all-targets --locked"
          : compilation.run!,
      );
      expect(result.error).toBeUndefined();
      expect(result.status, result.stderr).toBe(0);
      events.push(consumer ? "acceptance" : "compile");
    }
    // Each invocation owns its saved state, and post actions unwind in reverse.
    for (const post of posts.toReversed())
      if (post.save && post.dirty) events.push(`save:${post.id}`);
  }
  return { events, hit, status, posts };
}

test.each(["checks", "packages", "halo"])(
  "exact prewarm lookup avoids archive transfer, compilation and post save: %s",
  (job) => {
    const result = prepare(job, "true", "true");
    expect(result.status).toBe("success");
    expect(result.hit).toBe("true");
    expect(result.events).toEqual(["lookup"]);
  },
);

test.each(["checks", "packages", "halo"])(
  "prewarm fallback and default consumers preserve their cache work: %s",
  (job) => {
    for (const probe of ["false", "", "missing", "unexpected", "1"]) {
      const result = prepare(job, probe);
      expect(result.events).toEqual([
        "lookup",
        "restore",
        "download",
        "compile",
        "save:cache",
      ]);
      expect(result.hit).toBe("false");
      expect(result.posts).toEqual([
        { id: "cache-lookup", save: false, dirty: true },
        { id: "cache", save: true, dirty: true },
      ]);
    }
    expect(prepare(job, "false", "missing").events).toEqual([
      "lookup",
      "restore",
      "compile",
      "save:cache",
    ]);
    expect(prepare(job, "false", "true").events).toEqual([
      "lookup",
      "restore",
      "download",
    ]);
    for (const restored of ["true", "false", "missing"]) {
      const events = prepare(job, "failure", restored, true).events;
      expect(events).toEqual([
        "restore",
        ...(restored === "missing" ? [] : ["download"]),
        "acceptance",
        ...(restored === "true" ? [] : ["save:cache"]),
      ]);
    }
  },
  30_000,
);

test.each(["checks", "packages", "halo"])(
  "failed or cancelled probes preserve the original outcome: %s",
  (job) => {
    for (const status of ["failure", "cancelled"]) {
      const result = prepare(job, status);
      expect(result.status).toBe(status);
      expect(result.events).toEqual(["lookup"]);
      const fallback = prepare(job, "false", status);
      expect(fallback.status).toBe(status);
      expect(fallback.events).toEqual(["lookup", "restore"]);
    }
  },
);

test.each([rustSetup, packageSetup, haloSetup])(
  "lookup and restore use the same pinned Action and cache identity",
  (setup) => {
    expect(setup.inputs?.["cache-lookup-only"].default).toBe("false");
    const [lookup, restore] = setup.runs.steps.filter((step) =>
      step.uses?.startsWith("Swatinem/rust-cache@"),
    );
    expect(lookup.uses).toBe(
      "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6",
    );
    expect(restore.uses).toBe(lookup.uses);
    expect(lookup.with).toEqual({
      ...restore.with,
      "lookup-only": true,
      "save-if": false,
    });
    expect(restore.with?.["save-if"]).toBeUndefined();
    expect(restore.with?.["lookup-only"]).toBeUndefined();
    for (const step of setup.runs.steps.slice(
      0,
      setup.runs.steps.indexOf(lookup),
    ))
      expect(step.if).toBeUndefined();
  },
);

test("cache preparation is restricted to dev and dependency inputs", () => {
  expect(Object.keys(cache.on).sort()).toEqual(["push", "workflow_dispatch"]);
  expect(cache.on.push.branches).toEqual(["dev"]);
  expect(cache.permissions).toEqual({ contents: "read" });
  expect(cache.jobs.checks.if).toBe(
    "${{ github.ref == 'refs/heads/dev' && (github.event_name == 'push' || github.event_name == 'workflow_dispatch') }}",
  );
  expect(cache.jobs.packages.needs).toBe("checks");
  const paths = cache.on.push.paths.map((path) => new Bun.Glob(path));
  for (const path of [
    "Cargo.toml",
    "domain/Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/config.toml",
    ".github/actions/rust-setup/action.yml",
    ".github/actions/package-setup/action.yml",
    ".github/actions/halo-setup/action.yml",
    ".github/workflows/cache.yml",
    ".github/workflows/rust.yml",
    ".github/workflows/halo.yml",
    ".tool-versions",
    "halo/package.json",
    "halo/bun.lock",
    "tools/check-packages.ts",
    "fixture/postgresql.version",
    "fixture/postgresql.ts",
  ]) {
    expect(
      paths.some((glob) => glob.match(path)),
      path,
    ).toBe(true);
  }
  for (const path of [
    "domain/src/lib.rs",
    "docs/architecture.md",
    "design/orbit-pages.html",
  ]) {
    expect(
      paths.some((glob) => glob.match(path)),
      path,
    ).toBe(false);
  }
});

test("dev prewarms the same Halo environment and target as website acceptance", () => {
  const producer = cache.jobs.halo;
  const consumer = halo.jobs.website;
  expect(producer.if).toBe(
    "${{ github.ref == 'refs/heads/dev' && (github.event_name == 'push' || github.event_name == 'workflow_dispatch') }}",
  );
  expect(producer["runs-on"]).toBe(consumer["runs-on"]);
  for (const job of [producer, consumer]) {
    expect(job.env.CARGO_TARGET_DIR).toBe("target/cargo");
    expect(
      job.steps.filter((step) => step.uses === "./.github/actions/halo-setup"),
    ).toHaveLength(1);
  }
  const setup = producer.steps.find(
    (step) => step.uses === "./.github/actions/halo-setup",
  )!;
  expect(setup.id).toBe("setup");
  expect(setup.if).toBeUndefined();
  const compilation = producer.steps.find((step) => step.id === "compile")!;
  expect(compilation.if).toBe("${{ steps.setup.outputs.cache-hit != 'true' }}");
  expect(compilation.run).toBe(
    "cargo test -p nucleus --locked --test http --no-run",
  );
  expect(producer.steps.indexOf(compilation)).toBe(
    producer.steps.indexOf(setup) + 1,
  );
  expect(producer.steps.at(-1)).toBe(compilation);
});

test("Halo verifies its immutable preinstalled image before restoring image-bound dependencies", () => {
  const steps = haloSetup.runs.steps;
  const environment = steps.findIndex((step) => step.id === "environment");
  const dependencies = steps.findIndex(
    (step) => step.run === "bun install --frozen-lockfile --ignore-scripts",
  );
  const cached = steps.findIndex((step) => step.id === "cache");
  expect(environment).toBe(0);
  expect(steps[environment].run).toContain("bun tools/halo-ci-image.ts verify");
  expect(steps[environment].run).toContain('"${HALO_CI_IMAGE##*@}"');
  expect(dependencies).toBeGreaterThan(environment);
  expect(steps[dependencies]["working-directory"]).toBe("halo");
  expect(cached).toBeGreaterThan(dependencies);
  expect(steps[cached].with).toEqual({
    "shared-key": "${{ steps.environment.outputs.key }}-halo-http",
    workspaces: ". -> target/cargo",
    "cache-bin": false,
  });
  for (const step of steps.slice(0, cached - 1))
    expect(step.if).toBeUndefined();
  const image = halo.jobs.website.container!;
  expect(image.image).toMatch(
    /^ghcr.io\/silence-force\/nucleus-ci@sha256:[0-9a-f]{64}$/,
  );
  expect(image.options).toBe("--init --user 1001:1001 --ipc=host");
  expect(cache.jobs.halo.container).toEqual({
    ...image,
    options: "--user 1001:1001 --ipc=host",
  });
  for (const job of [halo.jobs.website, cache.jobs.halo]) {
    expect(job.env.HALO_CI_IMAGE).toBe(image.image);
    expect(job.permissions).toEqual({ contents: "read" });
  }
  for (const job of [
    cache.jobs.checks,
    ...Object.entries(rust.jobs)
      .filter(([name]) => name !== "packages")
      .map(([, job]) => job),
  ]) {
    expect(job.container).toBeUndefined();
  }
  const preparation = steps.map((step) => step.run ?? step.uses).join("\n");
  expect(preparation).not.toMatch(
    /apt-get|install-deps|rust-setup|actions\/postgresql|ImageOS|ImageVersion/,
  );

  const setup = Bun.YAML.parse(
    readFileSync(
      new URL("../.github/actions/rust-setup/action.yml", import.meta.url),
      "utf8",
    ),
  ) as { runs: { steps: Step[] } };
  const system = setup.runs.steps.findIndex((step) =>
    step.run?.includes("apt-get install"),
  );
  const hostEnvironment = setup.runs.steps.findIndex(
    (step) => step.id === "environment",
  );
  expect(system).toBeGreaterThanOrEqual(0);
  expect(hostEnvironment).toBeGreaterThan(system);
  expect(setup.runs.steps[hostEnvironment].run).toContain("dpkg-query --show");
  expect(
    setup.runs.steps.findIndex((step) => step.id === "cache"),
  ).toBeGreaterThan(hostEnvironment);
});

test("Halo cache hits and misses retain every acceptance command without later system changes", () => {
  const steps = halo.jobs.website.steps;
  const setup = steps.findIndex(
    (step) => step.uses === "./.github/actions/halo-setup",
  );
  expect(setup).toBeGreaterThan(
    steps.findIndex((step) => step.id === "changes"),
  );
  const evidenceUpload = steps.find((step) =>
    step.uses?.startsWith("actions/upload-artifact@"),
  );
  const acceptance = steps
    .slice(setup + 1)
    .filter((step) => step !== evidenceUpload);
  for (const step of steps.slice(setup)) {
    expect(step.if).toBe(
      step === evidenceUpload
        ? "${{ always() && steps.browser_acceptance.outcome != 'skipped' }}"
        : "${{ steps.changes.outputs.halo != 'skip' }}",
    );
  }
  expect(acceptance.map((step) => step.run?.trim())).toEqual([
    "bun tools/check-halo-test.ts snapshot",
    [
      "bun run check:generated",
      "bun run typecheck",
      "bun run test",
      "bun run build",
    ].join("\n"),
    "bun tools/check-halo-test.ts browser",
  ]);
  expect(acceptance[0].env?.NUCLEUS_OPENAPI_REFERENCE).toBe(
    "${{ github.workspace }}/halo/protocol/openapi.json",
  );
  expect(acceptance[2].env?.HALO_NGINX).toBe("/usr/sbin/nginx");
  expect(acceptance[2].env?.HALO_TEST_EVIDENCE).toBe(
    "${{ github.workspace }}/target/halo-browser-evidence/${{ github.run_id }}-${{ github.run_attempt }}",
  );
  expect(acceptance[1]["working-directory"]).toBe("halo");
  expect(
    rust.jobs.tools.steps.some((step) =>
      step.run?.includes("tools/check-halo-test.test.ts"),
    ),
  ).toBe(true);
});

test("every package restores dependencies only after verifying the same fixed PostgreSQL image", () => {
  for (const job of [cache.jobs.packages, rust.jobs.packages]) {
    expect(job.container).toEqual(cache.jobs.halo.container);
    expect(job.env.HALO_CI_IMAGE).toBe(job.container!.image);
    expect(job.permissions).toEqual({ contents: "read" });
    const setup = job.steps.filter(
      (step) => step.uses === "./.github/actions/package-setup",
    );
    expect(setup).toHaveLength(1);
    expect(setup[0].if).toBeUndefined();
  }
  const [verify, lookup, cached] = packageSetup.runs.steps;
  expect(packageSetup.runs.steps).toHaveLength(3);
  expect(verify.run).toContain("bun tools/halo-ci-image.ts verify");
  expect(verify.run).toContain('"${HALO_CI_IMAGE##*@}"');
  expect(verify.if).toBeUndefined();
  expect(lookup.id).toBe("cache-lookup");
  expect(cached.with).toEqual({
    "shared-key":
      "${{ steps.environment.outputs.key }}-${{ inputs.cache-key }}",
    workspaces: ". -> target/cargo",
    "cache-bin": false,
  });
  for (const job of [
    cache.jobs.checks,
    cache.jobs.packages,
    rust.jobs.packages,
  ]) {
    expect(
      job.steps.some((step) => step.uses === "./.github/actions/postgresql"),
    ).toBe(false);
  }
});

function cachedPostgresql(broken?: string, failure = "version") {
  const directory = mkdtempSync(join(tmpdir(), "nucleus postgres cache "));
  directories.push(directory);
  const version = readFileSync(
    new URL("../fixture/postgresql.version", import.meta.url),
    "utf8",
  );
  mkdirSync(join(directory, "fixture"));
  writeFileSync(join(directory, "fixture/postgresql.version"), version);
  writeFileSync(
    join(directory, "fixture/postgresql.ts"),
    readFileSync(new URL("../fixture/postgresql.ts", import.meta.url)),
  );
  const installation = join(
    directory,
    "target/postgresql",
    version.trim(),
    "bin",
  );
  mkdirSync(installation, { recursive: true });
  for (const program of ["postgres", "initdb", "pg_ctl", "psql"]) {
    if (program === broken && failure === "missing") continue;
    const path = join(installation, program);
    writeFileSync(
      path,
      `#!/bin/sh\n[ "$1" = --version ] || exit 1\nprintf '%s\\n' '${program} (PostgreSQL) ${program === broken && failure === "version" ? "0.0" : version.trim()}'\n`,
    );
    chmodSync(
      path,
      program === broken && failure === "permission" ? 0o600 : 0o700,
    );
  }
  return spawnSync(
    "bash",
    [
      "--noprofile",
      "--norc",
      "-e",
      "-o",
      "pipefail",
      "-c",
      "bun fixture/postgresql.ts",
    ],
    {
      cwd: directory,
      encoding: "utf8",
      timeout: 10_000,
    },
  );
}

test("package setup stops before cache output on an invalid image or failed verification", () => {
  const directory = mkdtempSync(join(tmpdir(), "nucleus image boundary "));
  directories.push(directory);
  const bin = join(directory, "bin");
  mkdirSync(bin);
  writeFileSync(
    join(bin, "bun"),
    '#!/bin/sh\nprintf verified > "$VERIFY_CALLED"\nexit "$VERIFY_RESULT"\n',
  );
  chmodSync(join(bin, "bun"), 0o700);
  for (const [image, verified, accepted] of [
    ["ghcr.io/silence-force/nucleus-halo-ci:latest", true, false],
    ["ghcr.io/another/image@sha256:" + "a".repeat(64), true, false],
    [
      "ghcr.io/silence-force/nucleus-ci@sha256:" + "a".repeat(64),
      false,
      false,
    ],
    [
      "ghcr.io/silence-force/nucleus-ci@sha256:" + "a".repeat(64),
      true,
      true,
    ],
  ] as const) {
    const output = join(directory, "output");
    const called = join(directory, "called");
    rmSync(output, { force: true });
    rmSync(called, { force: true });
    const result = spawnSync("bash", ["-c", packageSetup.runs.steps[0].run!], {
      env: {
        ...process.env,
        PATH: `${bin}:${process.env.PATH}`,
        HALO_CI_IMAGE: image,
        GITHUB_OUTPUT: output,
        VERIFY_CALLED: called,
        VERIFY_RESULT: verified ? "0" : "1",
      },
      encoding: "utf8",
      timeout: 5_000,
    });
    expect(result.error).toBeUndefined();
    expect(
      result.status === 0,
      JSON.stringify({
        image,
        verified,
        stdout: result.stdout,
        stderr: result.stderr,
      }),
    ).toBe(accepted);
    if (accepted)
      expect(readFileSync(output, "utf8")).toBe(
        `key=sha256:${"a".repeat(64)}\n`,
      );
    else expect(() => readFileSync(output)).toThrow();
    if (!verified) expect(readFileSync(called, "utf8")).toBe("verified");
  }
});

test("a restored PostgreSQL installation is verified without rebuilding", () => {
  const result = cachedPostgresql();
  expect(result.error).toBeUndefined();
  expect(result.status, result.stderr).toBe(0);
  expect(result.stdout).toContain("is ready at");
});

test.each(["postgres", "initdb", "pg_ctl", "psql"])(
  "restored PostgreSQL rejects an incompatible executable: %s",
  (program) => {
    const result = cachedPostgresql(program);
    expect(result.error).toBeUndefined();
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain(
      `Fixture ${program} must run as PostgreSQL`,
    );
    expect(result.stdout).not.toContain("is ready at");
  },
);

test.each(["missing", "permission"])(
  "PostgreSQL rejects a %s executable without rebuilding",
  (failure) => {
    const result = cachedPostgresql("initdb", failure);
    expect(result.error).toBeUndefined();
    expect(result.status).not.toBe(0);
    expect(result.stdout).not.toContain("is ready at");
    expect(result.stdout).not.toContain("configure");
  },
);

test("trusted preparation and PR validation share cache owners and dynamic packages", () => {
  for (const name of ["checks", "packages"]) {
    const job = cache.jobs[name];
    const consumer = rust.jobs[name];
    expect(job["runs-on"]).toBe(consumer["runs-on"]);
    expect(job.env.CARGO_TARGET_DIR).toBe(consumer.env.CARGO_TARGET_DIR);
    const action =
      name === "checks"
        ? "./.github/actions/rust-setup"
        : "./.github/actions/package-setup";
    const setup = job.steps.find((step) => step.uses === action);
    expect(setup?.with).toEqual({
      ...consumer.steps.find((step) => step.uses === action)?.with,
      "cache-lookup-only": true,
    });
    expect(setup?.id).toBe("setup");
    expect(job.steps.find((step) => step.id === "compile")?.if).toBe(
      "${{ steps.setup.outputs.cache-hit != 'true' }}",
    );
  }
  expect(cache.jobs.checks.outputs).toEqual({
    packages: "${{ steps.packages.outputs.names }}",
  });
  expect(rust.jobs.changes.outputs?.matrix).toBe(
    "${{ steps.packages.outputs.matrix }}",
  );
  expect(
    cache.jobs.checks.steps.find((step) => step.id === "packages")?.run,
  ).toContain("bun tools/check-packages.ts --list");
  expect(
    rust.jobs.changes.steps.find((step) => step.id === "packages")?.run,
  ).toContain(
    'bun tools/check-packages.ts --matrix --affected "$AFFECTED_PACKAGES"',
  );
  expect(cache.jobs.packages.strategy?.matrix).toEqual({
    package: "${{ fromJSON(needs.checks.outputs.packages) }}",
  });
  expect(rust.jobs.packages.strategy?.matrix).toBe(
    "${{ fromJSON(needs.changes.outputs.matrix) }}",
  );
  const setup = Bun.YAML.parse(
    readFileSync(
      new URL("../.github/actions/rust-setup/action.yml", import.meta.url),
      "utf8",
    ),
  ) as { outputs: Record<string, { value: string }>; runs: { steps: Step[] } };
  expect(
    setup.runs.steps.find((step) => step.id === "cache")?.uses,
  ).toStartWith("Swatinem/rust-cache@");
});

test("every selected matrix entry restores the same package cache prepared once on dev", () => {
  for (const workflow of [cache, rust]) {
    const setup = workflow.jobs.packages.steps.find(
      (step) => step.id === "setup",
    );
    expect(setup?.with?.["cache-key"]).toBe("package-${{ matrix.package }}");
    expect(workflow.jobs.packages.env.CARGO_TARGET_DIR).toBe("target/cargo");
  }
  expect(cache.jobs.packages.strategy?.matrix).toEqual({
    package: "${{ fromJSON(needs.checks.outputs.packages) }}",
  });
  expect(rust.jobs.packages.strategy?.matrix).toBe(
    "${{ fromJSON(needs.changes.outputs.matrix) }}",
  );
});

test("preparation compiles independent delivery and test targets without running tests", () => {
  const { run } = fixture();
  const result = run(compile("packages"));
  expect(result.error).toBeUndefined();
  expect(result.status, result.stderr).toBe(0);
  expect(result.stderr).toContain("Finished `dev` profile");
  expect(result.stderr).toContain("Finished `test` profile");
  expect(result.stdout).not.toContain("running 1 test");
}, 30_000);

test("a delivery build failure stops preparation before the test build", () => {
  const { write, run } = fixture();
  write(
    "app/src/lib.rs",
    '#[cfg(not(test))]\ncompile_error!("delivery build rejected");\n',
  );
  const result = run(compile("packages"));
  expect(result.error).toBeUndefined();
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain("delivery build rejected");
  expect(result.stderr).not.toContain("Finished `test` profile");
});

test("a test compilation failure fails preparation after a successful delivery build", () => {
  const { write, run } = fixture();
  write(
    "app/src/lib.rs",
    '#[cfg(test)]\ncompile_error!("test build rejected");\n',
  );
  const result = run(compile("packages"));
  expect(result.error).toBeUndefined();
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain("Finished `dev` profile");
  expect(result.stderr).toContain("test build rejected");
});

test("Halo prewarming compiles its real HTTP target without executing tests", () => {
  const { write, run } = fixture("nucleus");
  write(
    "app/tests/http.rs",
    '#[test]\nfn acceptance_must_not_run() { panic!("HTTP acceptance ran during cache preparation"); }\n',
  );
  const prepared = run(compile("halo"));
  expect(prepared.error).toBeUndefined();
  expect(prepared.status, prepared.stderr).toBe(0);
  expect(prepared.stderr).toContain("Executable tests/http.rs");
  expect(prepared.stdout).not.toContain("running 1 test");
  const executed = run("cargo test -p nucleus --locked --test http");
  expect(executed.error).toBeUndefined();
  expect(executed.status).not.toBe(0);
  expect(executed.stdout).toContain(
    "HTTP acceptance ran during cache preparation",
  );
});

test("Halo HTTP compilation failure fails prewarming", () => {
  const { write, run } = fixture("nucleus");
  write("app/tests/http.rs", 'compile_error!("HTTP target rejected");\n');
  const result = run(compile("halo"));
  expect(result.error).toBeUndefined();
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain("HTTP target rejected");
});

test("check-built dependencies are reused by Clippy", () => {
  const { run } = fixture();
  const prepared = run(compile("checks"));
  expect(prepared.error).toBeUndefined();
  expect(prepared.status, prepared.stderr).toBe(0);
  const checked = run(
    "cargo clippy --workspace --all-targets --locked --verbose -- -D warnings",
  );
  expect(checked.error).toBeUndefined();
  expect(checked.status, checked.stderr).toBe(0);
  expect(checked.stderr).toContain("Fresh cache-dependency v0.1.0");
});
