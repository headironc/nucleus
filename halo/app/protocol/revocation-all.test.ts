import * as vitest from "vitest";
import * as revocation from "./revocation";

const operation = `b4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const original = `a4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const second = `c4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const third = `d4e3bb16-fec5-4d86-9d2b-d172b8ea5000`;
const revokedAt = `2026-10-23T00:00:00Z`;
const later = `2026-10-23T00:00:01Z`;
const signal = new AbortController().signal;
vitest.afterEach(() => vitest.vi.unstubAllGlobals());

function respond(body: unknown, status = 200, type = `application/json`) {
  const fetch = vitest.vi.fn(
    async () =>
      new Response(JSON.stringify(body), {
        status,
        headers: { [`Content-Type`]: type, [`Retry-After`]: `19` },
      }),
  );
  vitest.vi.stubGlobal(`fetch`, fetch);
  return fetch;
}

vitest.describe(`the all-session logout protocol consumer`, () => {
  vitest.it.each([
    undefined,
    null,
    {},
    { scope: "other", initiatorId: original },
    { scope: "all" },
    { scope: "all", initiatorId: "invalid" },
    { scope: "all", initiatorId: second },
  ])(
    `rejects a missing or contradictory original proof %j`,
    async (binding) => {
      respond({
        code: "ok",
        original: binding,
        count: 1,
        revocations: [{ id: original, revokedAt }],
      });
      vitest
        .expect(
          await revocation.readAllRevocations(
            operation,
            original,
            second,
            signal,
          ),
        )
        .toEqual({ kind: "untrusted" });
    },
  );
  vitest.it.each([
    {
      original: { scope: "all", initiatorId: original },
      count: 0,
      revocations: [],
    },
    {
      original: { scope: "current", initiatorId: original },
      count: 2,
      revocations: [
        { id: original, revokedAt },
        { id: second, revokedAt },
      ],
    },
  ])(
    `rejects a proof inconsistent with the returned set %j`,
    async (collection) => {
      respond({ code: "ok", ...collection });
      vitest
        .expect(
          await revocation.readAllRevocations(
            operation,
            original,
            second,
            signal,
          ),
        )
        .toEqual({ kind: "untrusted" });
    },
  );
  vitest.it(
    `sends one bodyless DELETE bound to the operation and selected session`,
    async () => {
      const completion = { id: operation, count: 3, revokedAt };
      const fetch = respond({ code: `ok`, completion });
      vitest
        .expect(await revocation.revokeAll(operation, original, signal))
        .toEqual({ kind: `complete`, completion });
      vitest.expect(fetch).toHaveBeenCalledExactlyOnceWith(`/api/v1/sessions`, {
        method: `DELETE`,
        headers: {
          [`Operation-Id`]: operation,
          [`Session-Id`]: original,
          [`Accept`]: `application/json`,
        },
        credentials: `same-origin`,
        cache: `no-store`,
        redirect: `error`,
        signal,
      });
    },
  );
  vitest.it.each([
    { id: second, count: 3, revokedAt },
    { id: operation, count: 0, revokedAt },
    { id: operation, count: -1, revokedAt },
    { id: operation, count: 1.5, revokedAt },
    { id: operation, count: 3 },
    { id: operation, count: 3, revokedAt: `yesterday` },
  ])(
    `rejects an unbound or malformed all completion %j`,
    async (completion) => {
      respond({ code: `ok`, completion });
      vitest
        .expect(await revocation.revokeAll(operation, original, signal))
        .toEqual({ kind: `untrusted` });
    },
  );
  vitest.it.each([
    [200, `unknown`],
    [401, `ok`],
    [503, `invented_error`],
  ])(`rejects status %s with code %s`, async (status, code) => {
    respond({ code }, Number(status));
    vitest
      .expect(await revocation.revokeAll(operation, original, signal))
      .toEqual({ kind: `untrusted` });
  });
  vitest.it(`preserves a valid refusal without retry`, async () => {
    const fetch = respond({ code: `rate_limited` }, 429);
    vitest
      .expect(await revocation.revokeAll(operation, original, signal))
      .toEqual({
        kind: `failure`,
        status: 429,
        code: `rate_limited`,
        retryAfter: 19_000,
      });
    vitest.expect(fetch).toHaveBeenCalledTimes(1);
  });
  vitest.it(
    `accepts a complete ordered collection containing the initiating session`,
    async () => {
      const collection = {
        code: `ok`,
        original: { scope: "all" as const, initiatorId: original },
        count: 2,
        revocations: [
          { id: original, revokedAt },
          { id: second, revokedAt },
        ],
      };
      respond(collection);
      vitest
        .expect(
          await revocation.readAllRevocations(
            operation,
            original,
            second,
            signal,
          ),
        )
        .toEqual({ kind: `collection`, collection });
    },
  );
  vitest.it(
    `uses a fresh query identifier and the original operation in the query string`,
    async () => {
      const collection = {
        code: `ok`,
        original: { scope: "all" as const, initiatorId: original },
        count: 1,
        revocations: [{ id: original, revokedAt }],
      };
      const fetch = respond(collection);
      await revocation.readAllRevocations(operation, original, second, signal);
      const [url, options] = fetch.mock.calls[0] as unknown as [
        string,
        RequestInit,
      ];
      vitest.expect(url).toBe(`/api/v1/revocations?operation_id=${operation}`);
      vitest.expect(options.method).toBeUndefined();
      const headers = options.headers as Record<string, string>;
      vitest.expect(headers[`Session-Id`]).toBe(second);
      vitest.expect(headers[`Operation-Id`]).not.toBe(operation);
    },
  );
  vitest.it(`accepts an absent original without proving failure`, async () => {
    const collection = {
      code: `ok`,
      original: null,
      count: 0,
      revocations: [],
    };
    respond(collection);
    vitest
      .expect(
        await revocation.readAllRevocations(
          operation,
          original,
          second,
          signal,
        ),
      )
      .toEqual({ kind: `collection`, collection });
  });
  vitest.it(
    `accepts another initiator's prior record without claiming a matching binding`,
    async () => {
      const collection = {
        code: `ok`,
        original: { scope: "all" as const, initiatorId: second },
        count: 2,
        revocations: [
          { id: second, revokedAt },
          { id: third, revokedAt },
        ],
      };
      respond(collection);
      vitest
        .expect(
          await revocation.readAllRevocations(
            operation,
            original,
            second,
            signal,
          ),
        )
        .toEqual({ kind: `collection`, collection });
    },
  );
  vitest.it(
    `normalizes retained server identities for ordering and membership`,
    async () => {
      const collection = {
        code: `ok`,
        original: { scope: "all" as const, initiatorId: original },
        count: 2,
        revocations: [
          { id: `urn:uuid:${original.toUpperCase()}`, revokedAt },
          { id: second, revokedAt },
        ],
      };
      respond(collection);
      vitest
        .expect(
          await revocation.readAllRevocations(
            operation,
            original,
            second,
            signal,
          ),
        )
        .toEqual({ kind: `collection`, collection });
    },
  );
  vitest.it.each([
    { count: 2, revocations: [{ id: original, revokedAt }] },
    { count: -1, revocations: [] },
    {
      count: 2,
      revocations: [
        { id: original, revokedAt },
        { id: original, revokedAt },
      ],
    },
    {
      count: 2,
      revocations: [
        { id: second, revokedAt },
        { id: original, revokedAt },
      ],
    },
    {
      count: 2,
      revocations: [
        { id: original, revokedAt },
        { id: second, revokedAt: later },
      ],
    },
    {
      count: 2,
      revocations: [
        { id: original, revokedAt: `yesterday` },
        { id: second, revokedAt },
      ],
    },
    { count: 1, revocations: [{ id: second, revokedAt }] },
  ])(
    `rejects an incomplete, duplicated, unordered or foreign collection %j`,
    async (collection) => {
      respond({
        code: `ok`,
        original: { scope: "all", initiatorId: original },
        ...collection,
      });
      vitest
        .expect(
          await revocation.readAllRevocations(
            operation,
            original,
            second,
            signal,
          ),
        )
        .toEqual({ kind: `untrusted` });
    },
  );
});
