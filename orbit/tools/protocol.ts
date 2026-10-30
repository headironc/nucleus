import { mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import openapiTS, { astToString } from "openapi-typescript";
import { selectDocument, compile, type Method } from "../../halo/tools/protocol.ts";

export const endpoints: Record<string, readonly Method[]> = {
  "/api/v1/sessions": ["get", "post", "delete"],
  "/api/v1/sessions/{id}": ["delete"],
  "/api/v1/registrations": ["post"],
  "/api/v1/accounts/current": ["get"],
  "/api/v1/sessions/current": ["delete"],
  "/api/v1/revocations": ["get"],
};
export async function generate() {
  const snapshot = await readFile(new URL("../../halo/protocol/openapi.json", import.meta.url));
  const source = { source: "export", snapshot: "halo/protocol/openapi.json", sha256: createHash("sha256").update(snapshot).digest("hex") };
  const document = selectDocument(JSON.parse(snapshot.toString("utf8")), endpoints);
  const compiled = compile(document, endpoints, { accountId: "account.Id", sessionId: "session.Id" });
  const target = new URL("../.generated/", import.meta.url);
  await mkdir(target, { recursive: true });
  for (const [name, content] of Object.entries({
    "types.ts": astToString(await openapiTS(document)),
    "validators.cjs": compiled.validators,
    "contracts.js": compiled.contracts,
    "source.json": JSON.stringify(source),
  })) await writeFile(new URL(name, target), content);
  return source;
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  if (process.argv.length !== 2) throw new Error("Usage: node tools/protocol.ts");
  await generate();
}
