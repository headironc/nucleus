import { readFile, writeFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import openapiTS, {
  astToString,
  type OpenAPI3,
  type PathItemObject,
  type SchemaObject,
} from "openapi-typescript";
import Ajv2020 from "ajv/dist/2020.js";
import standaloneCode from "ajv/dist/standalone/index.js";
import addFormats from "ajv-formats";

const root = new URL("../", import.meta.url);
export type Method = `get` | `post` | `put` | `delete`;
const endpoints: Record<string, readonly Method[]> = {
  "/api/v1/codes": ["post"],
  "/api/v1/settings": ["get"],
  "/api/v1/sessions": ["post", "delete"],
  "/api/v1/registrations": ["post"],
  "/api/v1/accounts/current": ["get"],
  "/api/v1/accounts/current/role": ["get"],
  "/api/v1/accounts/current/cancellations": ["post"],
  "/api/v1/accounts/current/corrections": ["get"],
  "/api/v1/accounts/current/affiliation": ["get", "put"],
  "/api/v1/affiliations": ["get"],
  "/api/v1/workforce/current/grant": ["get"],
  "/api/v1/workforce/grants/candidates": ["post"],
  "/api/v1/workforce/grants": ["post"],
  "/api/v1/workforce/current/numbers": ["get"],
  "/api/v1/workforce/directories": ["get"],
  "/api/v1/workforce/numbers": ["post"],
  "/api/v1/workforce/application-template": ["get"],
  "/api/v1/workforce/application-photos": ["post"],
  "/api/v1/workforce/application-resources/{id}": ["get"],
  "/api/v1/workforce/applications/preview": ["post"],
  "/api/v1/workforce/applications": ["post"],
  "/api/v1/workforce/current/applications": ["get"],
  "/api/v1/workforce/current/applications/{id}": ["get"],
  "/api/v1/workforce/grants/{id}/terminations": ["post"],
  "/api/v1/sessions/current": ["delete"],
  "/api/v1/revocations": ["get"],
  "/api/v1/accounts/current/codes": ["post"],
  "/api/v1/mobile-changes": ["post"],
  "/api/v1/mobile-changes/{id}/codes": ["post"],
  "/api/v1/accounts/current/mobile": ["put"],
  "/api/v1/mobile-changes/{id}": ["get"],
};

export function selectDocument(document: OpenAPI3, selectedEndpoints = endpoints) {
  if (document.openapi !== "3.1.0") throw new Error("Expected OpenAPI 3.1.0");
  const schemas: Record<string, SchemaObject> = {};
  function visit(value: unknown) {
    if (!value || typeof value !== "object") return;
    if ("$ref" in value && typeof value.$ref === "string") {
      const match = /^#\/components\/schemas\/([^/]+)$/.exec(value.$ref);
      if (!match) throw new Error(`Unsupported reference: ${value.$ref}`);
      const name = match[1];
      if (!(name in schemas)) {
        if (!document.components?.schemas?.[name])
          throw new Error("Missing local schema");
        schemas[name] = structuredClone(document.components.schemas[name]);
        visit(schemas[name]);
      }
    }
    Object.values(value).forEach(visit);
  }
  const paths: Record<string, PathItemObject> = {};
  for (const [path, methods] of Object.entries(selectedEndpoints)) {
    const selected: PathItemObject = {};
    const item = document.paths?.[path];
    if (!item || "$ref" in item) throw new Error("Missing concrete path");
    for (const method of methods) {
      const operation = structuredClone(item[method]);
      if (!operation)
        throw new Error(`Missing implemented operation: ${method} ${path}`);
      Object.assign(selected, { [method]: operation });
      visit(operation);
    }
    paths[path] = selected;
  }
  return { ...document, paths, components: { schemas } };
}

const retainedIdentities = {
  accountId: "account.Id",
  grantId: "grant.Id",
  messageId: "message.Id",
  personId: "person.Id",
  sessionId: "session.Id",
  transferId: "transfer.Id",
};

export function compile(
  document: ReturnType<typeof selectDocument>,
  selectedEndpoints = endpoints,
  identities: Record<string, string> = retainedIdentities,
) {
  const ajv = new Ajv2020({
    strict: true,
    allErrors: true,
    code: { source: true, esm: false },
  });
  addFormats(ajv);
  // OpenAPI annotation; it changes no validation constraint.
  ajv.addKeyword({ keyword: "example", valid: true });
  function rewrite(value: unknown): unknown {
    if (Array.isArray(value)) return value.map(rewrite);
    if (!value || typeof value !== "object") return value;
    return Object.fromEntries(
      Object.entries(value).map(([key, child]) => [
        key,
        key === "$ref" && typeof child === "string"
          ? child.replace("#/components/schemas/", "#/$defs/")
          : rewrite(child),
      ]),
    );
  }
  const definitions = rewrite(document.components.schemas);
  const exports: Record<string, string> = {};
  const binary: Record<
    string,
    { input?: string[]; responses: Record<string, string[]> }
  > = {};
  function binaryMedia(content: Record<string, unknown>): string[] {
    const media = Object.keys(content);
    if (!media.length) throw new Error("Missing binary content");
    for (const type of media) {
      const entry = content[type];
      if (!entry || typeof entry !== "object" || !("schema" in entry))
        throw new Error("Unsupported binary media reference");
      let schema = entry.schema as SchemaObject | undefined;
      if (schema && "$ref" in schema) {
        // OpenAPI 3.1 applies sibling constraints alongside the referenced schema.
        if (
          Object.keys(schema).some(
            (key) => !["$ref", "description"].includes(key),
          )
        )
          throw new Error("Unsupported binary schema");
        const name = String(schema.$ref).replace("#/components/schemas/", "");
        schema = document.components.schemas[name];
      }
      if (
        !schema ||
        schema.type !== "string" ||
        schema.format !== "binary" ||
        Object.keys(schema).some(
          (key) => !["type", "format", "description"].includes(key),
        )
      )
        throw new Error("Unsupported binary schema");
    }
    return media;
  }
  const contracts: Record<
    string,
    Record<string, { validator: string; codes: string[] }>
  > = {};
  // Logout and termination recovery retain these identities without private projections.
  for (const [name, source] of Object.entries(identities)) {
    const schema = document.components.schemas[source];
    if (!schema) throw new Error(`Missing retained identity schema: ${source}`);
    ajv.addSchema({
      $id: name,
      $defs: definitions,
      ...(rewrite(schema) as object),
    });
    exports[name] = name;
  }
  for (const [path, methods] of Object.entries(selectedEndpoints)) {
    for (const method of methods) {
      const operation = document.paths[path][method];
      if (!operation || "$ref" in operation || !operation.responses)
        throw new Error("Missing concrete operation");
      const responses: Record<string, { validator: string; codes: string[] }> =
        {};
      const endpoint = `${method.toUpperCase()} ${path}`;
      for (const [status, response] of Object.entries(operation.responses)) {
        if (!/^[1-5][0-9]{2}$/.test(status))
          throw new Error("Unbound HTTP status");
        if ("$ref" in response)
          throw new Error("Unsupported response reference");
        const schema = response.content?.["application/json"]?.schema;
        if (!schema) {
          if (status !== "200" || !response.content)
            throw new Error("Missing JSON response schema");
          binary[endpoint] ??= { responses: {} };
          binary[endpoint].responses[status] = binaryMedia(response.content);
          continue;
        }
        const name = `${operation.operationId}_${status}`;
        ajv.addSchema({
          $id: name,
          $defs: definitions,
          ...(rewrite(schema) as object),
        });
        exports[name] = name;
        const codes =
          status === "200" || status === "201"
            ? ["ok"]
            : (response.description ?? "").split(";")[0].split(/, | or /);
        if (codes.some((code) => !/^[a-z]+(?:_[a-z]+)*$/.test(code))) {
          throw new Error(`Unbound application code in ${name}`);
        }
        responses[status] = { validator: name, codes };
      }
      contracts[endpoint] = responses;
      if (operation.requestBody) {
        if ("$ref" in operation.requestBody)
          throw new Error("Unsupported request reference");
        const media = operation.requestBody.content["application/json"];
        if (!media) {
          binary[endpoint] ??= { responses: {} };
          binary[endpoint].input = binaryMedia(operation.requestBody.content);
          continue;
        }
        if ("$ref" in media || !media.schema)
          throw new Error("Missing JSON input schema");
        const name = `${operation.operationId}_input`;
        ajv.addSchema({
          $id: name,
          $defs: definitions,
          ...(rewrite(media.schema) as object),
        });
        exports[name] = name;
      }
    }
  }
  return {
    validators: standaloneCode(ajv, exports),
    contracts: `// Generated from protocol/openapi.json. Do not edit.\nexport default ${JSON.stringify(contracts, null, 2)};\n`,
    binary: `// Generated from protocol/openapi.json. Do not edit.\nexport default ${JSON.stringify(binary, null, 2)};\n`,
  };
}

export async function generate(check: boolean) {
  const document = selectDocument(
    JSON.parse(await readFile(new URL("protocol/openapi.json", root), "utf8")),
  );
  const compiled = compile(document);
  const files = {
    "types.ts": astToString(await openapiTS(document)),
    "validators.cjs": `// Generated by Ajv from protocol/openapi.json. Do not edit.\n${compiled.validators}\n`,
    "contracts.js": compiled.contracts,
    "binary.js": compiled.binary,
  };
  await mkdir(new URL("app/protocol/generated/", root), { recursive: true });
  for (const [name, content] of Object.entries(files)) {
    const file = new URL(`app/protocol/generated/${name}`, root);
    if (check) {
      if ((await readFile(file, "utf8")) !== content)
        throw new Error(`Stale protocol: ${name}`);
    } else {
      await writeFile(file, content);
    }
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await generate(process.argv.includes("--check"));
}
