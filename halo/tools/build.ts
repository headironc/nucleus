import { readFile, writeFile, rm } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { files, digest, mediaType, verifyRelease } from "./release.mjs";
import { generate } from "./protocol.ts";

const root = fileURLToPath(new URL("../", import.meta.url));
await generate(true);
const sources = [
  ...(await files(join(root, "app")).then((paths) =>
    paths.map((path) => `app/${path}`),
  )),
  ...(await files(join(root, "tools")).then((paths) =>
    paths.map((path) => `tools/${path}`),
  )),
  "server.mjs",
  "package.json",
  "bun.lock",
  "tsconfig.json",
  "vite.config.ts",
  "react-router.config.ts",
  "../design/assets/tokens.css",
  "../design/assets/logo.png",
  "../design/assets/halo-botanical.svg",
  "../client/protocol/response.ts",
];
const sourceHashes = await Promise.all(
  sources
    .sort()
    .map(async (file) => [file, digest(await readFile(join(root, file)))]),
);
const buildId = digest(JSON.stringify(sourceHashes)).slice(0, 20);
await rm(join(root, "build"), { recursive: true, force: true });
const build = spawnSync(
  process.execPath,
  ["node_modules/@react-router/dev/bin.cjs", "build"],
  {
    cwd: root,
    stdio: "inherit",
    env: { ...process.env, HALO_BUILD_ID: buildId, NODE_ENV: "production" },
  },
);
if (build.status !== 0) throw new Error("Website build failed");
const resources = await Promise.all(
  (await files(join(root, "build/client"))).map(async (file) => ({
    file,
    path: `/assets/${buildId}/${file}`,
    mediaType: mediaType(file),
    sha256: digest(await readFile(join(root, "build/client", file))),
  })),
);
const server = await Promise.all(
  (await files(join(root, "build/server"))).map(async (file) => ({
    file,
    sha256: digest(await readFile(join(root, "build/server", file))),
  })),
);
const runtime = await Promise.all(
  ["server.mjs", "tools/release.mjs", "package.json", "bun.lock"].map(
    async (file) => ({
      file,
      sha256: digest(await readFile(join(root, file))),
    }),
  ),
);
await writeFile(
  join(root, "build/release.json"),
  JSON.stringify({ version: 1, buildId, resources, server, runtime }, null, 2) +
    "\n",
);
await verifyRelease(root);
console.log(
  `Verified Halo build ${buildId}: ${resources.length} public resources`,
);
