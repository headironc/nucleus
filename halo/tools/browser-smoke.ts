// Verifies the browser runtimes installed in the fixed Halo environment: the
// default headless shell and full Chromium with the native consumer's
// back-forward-cache behaviour. Run from halo/ after installing dependencies.

import { chromium } from "@playwright/test";
import { readdirSync, readFileSync, readlinkSync } from "node:fs";
import {
  runBrowserSmoke,
  type LaunchSpec,
  type LaunchedBrowser,
  type SmokePage,
} from "./browser-smoke-core.ts";

const BROWSER_EXECUTABLE =
  /(?:^|\/)(?:chrome|chrome-headless-shell|headless_shell)$/;

function directChildExecutable(): string {
  // Playwright starts the browser as a direct child of this process. Reading
  // the actual executable avoids reporting a path that the launch did not use.
  if (process.platform !== "linux")
    throw new Error("Browser runtime smoke requires Linux /proc");
  for (const entry of readdirSync("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    let stat: string;
    try {
      stat = readFileSync(`/proc/${entry}/stat`, "utf8");
    } catch {
      continue;
    }
    const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
    // fields[0] is the state and fields[1] the parent process ID.
    if (Number(fields[1]) !== process.pid) continue;
    try {
      const executable = readlinkSync(`/proc/${entry}/exe`);
      if (BROWSER_EXECUTABLE.test(executable)) return executable;
    } catch {
      // The process may have exited between the scan and the read.
    }
  }
  throw new Error("Could not identify the launched browser executable");
}

async function launch(spec: LaunchSpec): Promise<LaunchedBrowser> {
  const browser = await chromium.launch(
    spec.channel
      ? {
          channel: spec.channel,
          ...(spec.ignoreDefaultArgs
            ? { ignoreDefaultArgs: spec.ignoreDefaultArgs }
            : {}),
        }
      : {},
  );
  return {
    get path() {
      return directChildExecutable();
    },
    version: browser.version(),
    async openPage(): Promise<SmokePage> {
      const page = await browser.newPage();
      return {
        goto: (url, options) => page.goto(url, options),
        textContent: (selector) => page.textContent(selector),
        close: () => page.close(),
      };
    },
    close: () => browser.close(),
  };
}

try {
  await runBrowserSmoke(launch);
} catch (error) {
  console.error(
    `Browser runtime smoke failed: ${error instanceof Error ? error.message : error}`,
  );
  process.exitCode = 1;
}
