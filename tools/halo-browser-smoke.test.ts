import { expect, test } from "bun:test";
import {
  runBrowserSmoke,
  smokeSpecs,
  type LaunchSpec,
} from "../halo/tools/browser-smoke-core";

type Events = string[];

function adapter(
  spec: LaunchSpec,
  events: Events,
  overrides: {
    goto?: () => Promise<unknown>;
    textContent?: () => Promise<string | null>;
    pageClose?: () => Promise<void>;
    browserClose?: () => Promise<void>;
  } = {},
) {
  const marker = `nucleus-halo-browser-smoke-${spec.kind}`;
  return {
    path: `/browsers/${spec.kind}/executable`,
    version: `${spec.kind}-version`,
    async openPage() {
      events.push(`open-page:${spec.kind}`);
      return {
        goto:
          overrides.goto ??
          (async (url: string) => {
            expect(url.startsWith("data:text/html;charset=utf-8,")).toBe(true);
            events.push(`goto:${spec.kind}`);
          }),
        textContent:
          overrides.textContent ??
          (async () => {
            events.push(`text:${spec.kind}`);
            return marker;
          }),
        close:
          overrides.pageClose ??
          (async () => {
            events.push(`close-page:${spec.kind}`);
          }),
      };
    },
    async close() {
      if (overrides.browserClose) return overrides.browserClose();
      events.push(`close-browser:${spec.kind}`);
    },
  };
}

test("the smoke launches the default shell and full Chromium, navigating and closing both", async () => {
  const events: Events = [];
  const specs: LaunchSpec[] = [];
  const lines: string[] = [];
  const result = await runBrowserSmoke(
    async (spec) => {
      specs.push(spec);
      return adapter(spec, events);
    },
    (line) => lines.push(line),
  );

  expect(specs).toEqual([...smokeSpecs]);
  expect(specs[0]).toEqual({ kind: "chromium-headless-shell" });
  expect(specs[1]).toEqual({
    kind: "chromium",
    channel: "chromium",
    ignoreDefaultArgs: ["--disable-back-forward-cache"],
  });
  expect(events).toEqual([
    "open-page:chromium-headless-shell",
    "goto:chromium-headless-shell",
    "text:chromium-headless-shell",
    "close-page:chromium-headless-shell",
    "close-browser:chromium-headless-shell",
    "open-page:chromium",
    "goto:chromium",
    "text:chromium",
    "close-page:chromium",
    "close-browser:chromium",
  ]);
  expect(result).toEqual([
    {
      kind: "chromium-headless-shell",
      path: "/browsers/chromium-headless-shell/executable",
      version: "chromium-headless-shell-version",
    },
    {
      kind: "chromium",
      path: "/browsers/chromium/executable",
      version: "chromium-version",
    },
  ]);
  expect(lines.join("\n")).toContain(
    "type=chromium version=chromium-version path=/browsers/chromium/executable page=ok",
  );
});

test("navigation and assertion failures still close both pages and browsers", async () => {
  const events: Events = [];
  const specs: LaunchSpec[] = [];
  const run = runBrowserSmoke(
    async (spec) => {
      specs.push(spec);
      return adapter(spec, events, {
        ...(spec.kind === "chromium-headless-shell"
          ? {
              goto: async () => {
                throw new Error("navigation failed");
              },
            }
          : {
              textContent: async () => "different content",
            }),
      });
    },
    () => {},
  );

  await expect(run).rejects.toThrow(
    "chromium-headless-shell smoke failed: navigation failed",
  );
  await expect(run).rejects.toThrow(
    "chromium smoke failed: inline page content was",
  );
  expect(specs).toHaveLength(2);
  for (const kind of ["chromium-headless-shell", "chromium"]) {
    expect(events).toContain(`close-page:${kind}`);
    expect(events).toContain(`close-browser:${kind}`);
  }
});

test("cleanup failures are reported without skipping the other browser", async () => {
  const events: Events = [];
  const run = runBrowserSmoke(
    async (spec) =>
      adapter(spec, events, {
        ...(spec.kind === "chromium-headless-shell"
          ? {
              pageClose: async () => {
                throw new Error("page close broke");
              },
              browserClose: async () => {
                throw new Error("browser close broke");
              },
            }
          : {}),
      }),
    () => {},
  );

  await expect(run).rejects.toThrow(
    "chromium-headless-shell smoke cleanup failed: page close failed: page close broke; browser close failed: browser close broke",
  );
  expect(events).toContain("close-browser:chromium");
});

test("a launch failure does not prevent the other runtime from being exercised", async () => {
  const events: Events = [];
  const specs: LaunchSpec[] = [];
  const run = runBrowserSmoke(
    async (spec) => {
      specs.push(spec);
      if (spec.kind === "chromium-headless-shell")
        throw new Error("shell is missing");
      return adapter(spec, events);
    },
    () => {},
  );

  await expect(run).rejects.toThrow(
    "chromium-headless-shell smoke failed: shell is missing",
  );
  expect(specs.map((spec) => spec.kind)).toEqual([
    "chromium-headless-shell",
    "chromium",
  ]);
  expect(events).toContain("close-browser:chromium");
});

test("the smoke rejects an environment where both modes use one executable", async () => {
  const events: Events = [];
  const run = runBrowserSmoke(
    async (spec) => {
      const browser = adapter(spec, events);
      return { ...browser, path: "/browsers/shared/executable" };
    },
    () => {},
  );

  await expect(run).rejects.toThrow(
    "Browser runtime modes reused one executable",
  );
});

test("metadata lookup failure closes the started browser before trying the other mode", async () => {
  const events: Events = [];
  await expect(
    runBrowserSmoke(
      async (spec) => {
        const browser = adapter(spec, events);
        return spec.kind === "chromium-headless-shell"
          ? {
              ...browser,
              get path(): string {
                throw new Error("process metadata denied");
              },
            }
          : browser;
      },
      () => {},
    ),
  ).rejects.toThrow("process metadata denied");
  expect(events).toContain("close-browser:chromium-headless-shell");
  expect(events).not.toContain("open-page:chromium-headless-shell");
  expect(events).toContain("close-browser:chromium");
});
