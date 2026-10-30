import { cp, mkdir, readFile, writeFile } from "node:fs/promises";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { generate } from "./protocol.ts";

const root = fileURLToPath(new URL("../", import.meta.url));
export async function build() {
  const origin = process.env.ORBIT_API_ORIGIN;
  const project = JSON.parse(await readFile(join(root, "project.config.json"), "utf8"));
  if (!project || typeof project !== "object" || Array.isArray(project)) throw new Error("Expected a public project configuration object");
  const appid = process.env.ORBIT_APP_ID ?? project.appid;
  if (!origin || typeof appid !== "string" || !appid) throw new Error("ORBIT_API_ORIGIN and a project AppID are required");
  const url = new URL(origin);
  if (url.protocol !== "https:" || url.origin !== origin || url.username || url.password) throw new Error("Expected exact HTTPS API origin");
  if (/[\s\p{Cc}]/u.test(appid)) throw new Error("Expected an explicit project AppID without whitespace or control characters");
  if (appid === "touristappid") throw new Error("A test-project identity cannot produce a production build");
  const library = process.env.ORBIT_BASE_LIBRARY;
  if (library && !/^(?:[0-9]+\.[0-9]+\.[0-9]+|latest|trial|widelyUsed)$/.test(library)) throw new Error("Invalid explicit base-library selection");
  const out = join(root, "build");
  async function configuration(name: string): Promise<Record<string, any>> {
    try {
      const value = JSON.parse(await readFile(join(out, name), "utf8"));
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Expected a project configuration object");
      return value;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return {};
      throw error;
    }
  }
  const common = await configuration("project.config.json");
  const local = await configuration("project.private.config.json");
  if ((local.appid !== undefined && local.appid !== appid) || (library && local.libVersion !== undefined && local.libVersion !== library)) {
    throw new Error("Existing private project configuration overrides the requested project identity or library");
  }
  const source = await generate();
  await mkdir(out, { recursive: true });
  const entries = ["app.ts", "pages/home/index.ts", "pages/account/index.ts"];
  for (const entry of entries) {
    const result = await Bun.build({
      entrypoints: [join(root, "src", entry)], target: "browser", format: "cjs", minify: false,
      define: { ORBIT_API_ORIGIN: JSON.stringify(origin) },
    });
    if (!result.success || result.outputs.length !== 1) throw new Error("Native package bundling failed");
    const output = join(out, entry.replace(/\.ts$/, ".js"));
    await mkdir(dirname(output), { recursive: true });
    const text = await result.outputs[0].text();
    if (/\b(?:eval|Function)\s*\(/.test(text) || new Bun.Transpiler({ loader: "js" }).scanImports(text).length) {
      throw new Error("Package contains dynamic compilation or external imports");
    }
    await writeFile(output, text);
  }
  for (const entry of ["app.json", "app.wxss", "sitemap.json", "pages/home/index.json", "pages/home/index.wxml", "pages/account/index.json", "pages/account/index.wxml"]) {
    await cp(join(root, "src", entry), join(out, entry));
  }
  await writeFile(join(out, "project.config.json"), JSON.stringify({
    ...common, appid, projectname: "nucleus-orbit", miniprogramRoot: "./", compileType: "miniprogram",
    ...(library ? { libVersion: library } : {}),
    setting: { ...common.setting, es6: false, enhance: false, minified: false },
  }, null, 2));
  await writeFile(join(out, "build-source.json"), JSON.stringify({
    ...source,
    apiOrigin: origin,
    platformAcceptance: false,
  }, null, 2));
  // Ensure the source copy exists and is still parseable before reporting completion.
  JSON.parse(await readFile(join(out, "app.json"), "utf8"));
  return out;
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  if (process.argv.length !== 2) throw new Error("Usage: bun tools/build.ts");
  console.log(await build());
}
