import { readFileSync } from "node:fs";
import Ajv2020 from "ajv/dist/2020.js";
import addFormats from "ajv-formats";
import { describe, expect, it } from "vitest";
import {
  accountId,
  applicationMaterial,
} from "../../tests/application-material";

const schemas = JSON.parse(
  readFileSync(new URL("../../protocol/openapi.json", import.meta.url), "utf8"),
).components.schemas as Record<string, unknown>;

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

const ajv = new Ajv2020({ strict: true, allErrors: true });
addFormats(ajv);
ajv.addKeyword({ keyword: "example", valid: true });
const validateApplication = ajv.compile({
  $defs: rewrite(schemas),
  $ref: "#/$defs/application.Application",
});
const validateCancellation = ajv.compile({
  $defs: rewrite(schemas),
  $ref: "#/$defs/application.Cancellation",
});
const validateInput = ajv.compile({
  $defs: rewrite(schemas),
  $ref: "#/$defs/application.CancellationInput",
});

describe("the actual badge cancellation schemas", () => {
  const approval = {
    reviewerId: accountId,
    reviewerName: "林*",
    reviewedAt: "2026-10-31T01:15:00Z",
  };
  const fields = ["reviewerId", "reviewerName", "reviewedAt"] as const;

  it("accepts complete application projections with no approval or the whole prior approval", () => {
    const submitted = applicationMaterial();
    const approved = { ...submitted, ...approval, status: "approved" };
    const rejected = {
      ...approved,
      status: "rejected",
      rejectionReason: "Incorrect photograph",
    };
    for (const value of [
      submitted,
      approved,
      rejected,
      { ...submitted, status: "cancelled" },
      { ...approved, status: "cancelled" },
    ])
      expect(
        validateApplication(value),
        JSON.stringify(validateApplication.errors),
      ).toBe(true);

    for (let mask = 1; mask < 7; mask++) {
      const partial: Record<string, unknown> = {
        ...submitted,
        status: "cancelled",
      };
      fields.forEach((field, index) => {
        if (mask & (1 << index)) partial[field] = approval[field];
      });
      expect(validateApplication(partial), `partial approval ${mask}`).toBe(
        false,
      );
    }
    for (const field of fields) {
      const missing: Record<string, unknown> = { ...approved };
      delete missing[field];
      expect(validateApplication(missing), `approved requires ${field}`).toBe(
        false,
      );
      expect(
        validateApplication({
          ...approved,
          status: "cancelled",
          [field]: null,
        }),
      ).toBe(false);
    }
    const { rejectionReason: _, ...missingReason } = rejected;
    expect(validateApplication(missingReason)).toBe(false);
  });

  it("requires explicit mode, confirmed unfinished state and a manager reason", () => {
    for (const mode of ["self", "manager"]) {
      for (const expectedStatus of ["submitted", "approved", "ready"]) {
        const value = {
          mode,
          expectedStatus,
          ...(mode === "manager" ? { reason: "Duplicate" } : {}),
        };
        expect(validateInput(value)).toBe(true);
      }
    }
    for (const value of [
      { mode: "self" },
      { mode: "self", expectedStatus: "cancelled" },
      { mode: "self", expectedStatus: "submitted", reason: "Unexpected" },
      { mode: "manager", expectedStatus: "approved" },
      { mode: "manager", expectedStatus: "approved", reason: null },
    ])
      expect(validateInput(value), JSON.stringify(value)).toBe(false);
  });

  it("limits success to the cancelled identity without private or audit fields", () => {
    const application = { id: applicationMaterial().id, status: "cancelled" };
    expect(validateCancellation({ code: "ok", application })).toBe(true);
    for (const value of [
      { code: "ok", application: { id: application.id } },
      { code: "ok", application: { ...application, status: "approved" } },
      {
        code: "ok",
        application: { ...application, reason: "Private manager note" },
      },
      {
        code: "ok",
        application: {
          ...application,
          snapshot: applicationMaterial().snapshot,
        },
      },
    ])
      expect(validateCancellation(value)).toBe(false);
  });
});
