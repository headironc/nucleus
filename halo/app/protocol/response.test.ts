import { expect, it, vi } from "vitest";
import { decode } from "./response";

it("rejects a wrong media type without consuming its body", async () => {
  const response = new Response("not JSON", { status: 200, headers: { "content-type": "text/html" } });
  const json = vi.spyOn(response, "json");
  expect(await decode(response, "GET /api/v1/accounts/current")).toBeUndefined();
  expect(json).not.toHaveBeenCalled();
});
