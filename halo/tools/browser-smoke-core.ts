// Browser runtime smoke shared by the local preparation entrypoint and its
// regression tests. It deliberately keeps the browser adapter small so tests
// can observe each launch, navigation, assertion and close independently.

export type BrowserKind = "chromium-headless-shell" | "chromium";

export type LaunchSpec = {
  kind: BrowserKind;
  channel?: "chromium";
  ignoreDefaultArgs?: string[];
};

export type SmokePage = {
  goto(
    url: string,
    options: { waitUntil: "domcontentloaded" },
  ): Promise<unknown>;
  textContent(selector: string): Promise<string | null>;
  close(): Promise<void>;
};

export type LaunchedBrowser = {
  path: string;
  version: string;
  openPage(): Promise<SmokePage>;
  close(): Promise<void>;
};

export type LaunchBrowser = (spec: LaunchSpec) => Promise<LaunchedBrowser>;

export type SmokeObservation = {
  kind: BrowserKind;
  path: string;
  version: string;
};

// The default launch resolves to Playwright's headless shell. Full Chromium
// must use the same channel and back-forward-cache default as the native
// identity consumer; the shell cannot replace it.
export const smokeSpecs: readonly LaunchSpec[] = [
  { kind: "chromium-headless-shell" },
  {
    kind: "chromium",
    channel: "chromium",
    ignoreDefaultArgs: ["--disable-back-forward-cache"],
  },
];

function detail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function smokeUrl(kind: BrowserKind): string {
  const marker = `nucleus-halo-browser-smoke-${kind}`;
  return `data:text/html;charset=utf-8,${encodeURIComponent(
    `<!doctype html><html><body>${marker}</body></html>`,
  )}`;
}

async function exercise(
  launch: LaunchBrowser,
  spec: LaunchSpec,
  report: (line: string) => void,
): Promise<SmokeObservation> {
  let launched: LaunchedBrowser;
  try {
    launched = await launch(spec);
  } catch (error) {
    throw new Error(`${spec.kind} smoke failed: ${detail(error)}`);
  }
  let page: SmokePage | null = null;
  let observation: SmokeObservation | null = null;
  let failure: unknown = null;
  try {
    observation = {
      kind: spec.kind,
      path: launched.path,
      version: launched.version,
    };
    page = await launched.openPage();
    await page.goto(smokeUrl(spec.kind), { waitUntil: "domcontentloaded" });
    const text = await page.textContent("body");
    const expected = `nucleus-halo-browser-smoke-${spec.kind}`;
    if ((text ?? "").trim() !== expected)
      throw new Error(`inline page content was ${JSON.stringify(text)}`);
  } catch (error) {
    failure = error;
  }

  const cleanup: string[] = [];
  if (page) {
    try {
      await page.close();
    } catch (error) {
      cleanup.push(`page close failed: ${detail(error)}`);
    }
  }
  try {
    await launched.close();
  } catch (error) {
    cleanup.push(`browser close failed: ${detail(error)}`);
  }

  const label = `${spec.kind} smoke`;
  if (failure !== null && cleanup.length)
    throw new Error(
      `${label} failed: ${detail(failure)}; cleanup also failed: ${cleanup.join("; ")}`,
    );
  if (failure !== null) throw new Error(`${label} failed: ${detail(failure)}`);
  if (cleanup.length)
    throw new Error(`${label} cleanup failed: ${cleanup.join("; ")}`);
  if (!observation) throw new Error(`${label} metadata is unavailable`);

  report(
    `browser smoke: type=${spec.kind} version=${observation.version} path=${observation.path} page=ok`,
  );
  return observation;
}

/**
 * Launch both runtime modes, navigate each to a harmless inline page, validate
 * its content and close both page and browser. A failure in one mode does not
 * skip the other launch, and a cleanup failure never replaces the primary one.
 */
export async function runBrowserSmoke(
  launch: LaunchBrowser,
  report: (line: string) => void = (line) => console.log(line),
): Promise<SmokeObservation[]> {
  const observations: SmokeObservation[] = [];
  const failures: string[] = [];
  for (const spec of smokeSpecs) {
    try {
      observations.push(await exercise(launch, spec, report));
    } catch (error) {
      failures.push(detail(error));
    }
  }
  if (failures.length) throw new Error(failures.join("\n"));
  if (
    new Set(observations.map((item) => item.path)).size !== observations.length
  )
    throw new Error(
      "Browser runtime modes reused one executable; full Chromium cannot be replaced by the headless shell",
    );
  return observations;
}
