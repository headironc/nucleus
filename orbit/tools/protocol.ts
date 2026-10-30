import { mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import openapiTS, { astToString } from "openapi-typescript";
import { selectDocument, compile, type Method } from "../../halo/tools/protocol.ts";

export const endpoints: Record<string, readonly Method[]> = {
  "/api/v1/sessions": ["post", "delete"],
  "/api/v1/registrations": ["post"],
  "/api/v1/accounts/current": ["get"],
  "/api/v1/sessions/current": ["delete"],
  "/api/v1/revocations": ["get"],
};
export async function generate(proposal = false) {
  const document = selectDocument(proposal ? (await import("../tests/proposed-document.ts")).proposedDocument() :
    JSON.parse(await readFile(new URL("../protocol/openapi.json", import.meta.url), "utf8")), endpoints);
  const compiled = compile(document, endpoints, { accountId: "account.Id", sessionId: "session.Id" });
  const target = new URL("../.generated/", import.meta.url);
  await mkdir(target, { recursive: true });
  for (const [name, content] of Object.entries({
    "types.ts": astToString(await openapiTS(document)),
    "validators.cjs": compiled.validators,
    "contracts.js": compiled.contracts,
    "source.json": JSON.stringify({ source: proposal ? "proposal" : "export" }),
  })) await writeFile(new URL(name, target), content);
}
if (process.argv[1] === fileURLToPath(import.meta.url)) await generate(process.argv.includes("--proposal"));
