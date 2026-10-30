import website from "../../halo/protocol/openapi.json" with { type: "json" };

/** Controlled test proposal only. Never written to protocol/openapi.json. */
export function proposedDocument() {
  const document = JSON.parse(JSON.stringify(website));
  const schemas = document.components.schemas;
  schemas["session.Credential"] = {
    type: "object", required: ["type", "code"], additionalProperties: false,
    properties: { type: { const: "mobile" }, code: { type: "string", minLength: 1 } },
  };
  schemas["session.Session"].required.push("token");
  schemas["session.Session"].properties.token = schemas["session.Registration"].properties.token;
  schemas["session.Session"].maxProperties = 3;
  document.paths["/api/v1/sessions"].post.responses["403"].description = "mobile_unsupported or permission_denied";
  // The proposed transport is Bearer; the production document must be a real export.
  for (const path of Object.values(document.paths) as any[]) {
    for (const operation of Object.values(path) as any[]) {
      if (operation && typeof operation === "object" && operation.parameters) {
        operation.parameters = operation.parameters.filter((item: any) => !["Origin", "Session-Id"].includes(item.name));
      }
    }
  }
  return document;
}
