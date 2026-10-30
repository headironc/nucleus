import { afterEach, describe, expect, it, vi } from "vitest";
import { decode } from "./response";
import {
  current,
  login,
  validInput,
  validRegistration,
  validSession,
} from "./session";

const id = "f9bd2899-fb4d-4212-a831-15168b01e783";
const session = { id, expiresAt: "2035-01-01T00:00:00Z" };
const registration = {
  id,
  token: "A".repeat(43),
  expiresAt: "2035-01-01T00:00:00Z",
};
const input = {
  secret: "A".repeat(43),
  credential: {
    type: "message" as const,
    id,
    mobile: "13900000001",
    code: "001234",
  },
};
const account = {
  id,
  personId: id,
  revisionId: id,
  name: "Fixture Operator",
  idNumber: { suffix: "002X" },
  avatar: {
    revision: id,
    href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${Buffer.alloc(32, 255).toString("base64url")}`,
    expiresAt: session.expiresAt,
    mediaType: "image/webp",
    width: 512,
    height: 512,
  },
  binding: null,
};

afterEach(() => vi.unstubAllGlobals());

describe("actual R1 and current-account protocols", () => {
  it("keeps website inputs and delivered sessions within the cookie source", async () => {
    const mobile = {
      secret: input.secret,
      credential: { type: "mobile", code: "fixture-phone-code" },
    };
    expect(validInput(mobile)).toBe(false);
    expect(validSession({ ...session, token: "A".repeat(43) })).toBe(false);
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              code: "ok",
              session: { ...session, token: "A".repeat(43) },
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          ),
      ),
    );
    expect(await login(id, input, new AbortController().signal)).toEqual({
      kind: "untrusted",
    });
    vi.mocked(fetch).mockClear();
    expect(
      await login(
        id,
        mobile as unknown as typeof input,
        new AbortController().signal,
      ),
    ).toEqual({ kind: "untrusted" });
    expect(fetch).not.toHaveBeenCalled();
  });
  const malformedTokens = [
    "",
    "A".repeat(42),
    "A".repeat(44),
    "A".repeat(43) + "=",
    "A".repeat(42) + "=",
    "+" + "A".repeat(42),
    "/" + "A".repeat(42),
    " " + "A".repeat(42),
    "界" + "A".repeat(42),
    "A".repeat(42) + "B",
    "A".repeat(42) + "_",
    "!".repeat(43),
    "A".repeat(43) + "\n",
  ];

  it.each(malformedTokens)(
    "rejects a malformed original login secret: %j",
    (secret) => {
      expect(validInput({ ...input, secret })).toBe(false);
    },
  );

  it("accepts independently encoded 32-byte secrets and leading-zero codes", () => {
    for (const bytes of [
      Buffer.from(Array.from({ length: 32 }, (_, index) => index)),
      Buffer.alloc(32, 255),
      ...Array.from({ length: 16 }, (_, last) =>
        Buffer.concat([Buffer.alloc(31), Buffer.from([last])]),
      ),
    ]) {
      const secret = bytes.toString("base64url");
      expect(validInput({ ...input, secret })).toBe(true);
      expect(input.credential.code).toBe("001234");
    }
  });

  it.each([
    "13000000000", "13900000000", "14000000000", "14100000000",
    "14400000000", "14900000000", "15000000000", "15300000000",
    "15500000000", "15900000000", "16200000000", "16500000000",
    "16700000000", "17000000000", "17800000000", "18000000000",
    "18900000000", "19000000000", "19300000000", "19500000000",
    "19900000000",
  ])("accepts a documented domestic mobile boundary: %s", (mobile) => {
    expect(validInput({ ...input, credential: { ...input.credential, mobile } })).toBe(true);
  });

  it.each([
    "", "not-a-mobile", "23800000000", "1380000000a", "14200000000",
    "14300000000", "15400000000", "16000000000", "16100000000",
    "16300000000", "16400000000", "16800000000", "16900000000",
    "17900000000", "19400000000", "+8613800000000", "8613800000000",
    " 13800000000", "13800000000 ", "13900000001\n", "138-0000-0000",
    "138 0000 0000", "１３８００００００００", "1380000000", "138000000000",
  ])("rejects a mobile outside the existing HTTP contract: %j", (mobile) => {
    expect(validInput({ ...input, credential: { ...input.credential, mobile } })).toBe(false);
  });

  it.each(["secret", "credential"])("requires the outer %s field", (field) => {
    const value: Record<string, unknown> = { ...input };
    delete value[field];
    expect(validInput(value)).toBe(false);
    expect(validInput({ ...input, [field]: null })).toBe(false);
  });

  it.each(["", "00123", "0012345", "00123a", "００１２３４", "001234\n", "001234\r"])(
    "rejects a code outside the existing six-digit contract: %j",
    (code) => {
      expect(validInput({ ...input, credential: { ...input.credential, code } })).toBe(false);
    },
  );

  it.each(["type", "id", "mobile", "code"])(
    "requires the credential %s field even if an unknown field replaces it",
    (field) => {
      const credential: Record<string, unknown> = { ...input.credential };
      delete credential[field];
      expect(validInput({ ...input, credential })).toBe(false);
      expect(validInput({ ...input, credential: { ...credential, unknown: "fixture" } })).toBe(false);
      expect(validInput({ ...input, credential: { ...input.credential, [field]: null } })).toBe(false);
    },
  );

  it.each(["transport", "token", "unknown"])(
    "rejects the additional %s field at both input levels",
    (field) => {
      expect(validInput({ ...input, [field]: "fixture" })).toBe(false);
      expect(validInput({ ...input, credential: { ...input.credential, [field]: "fixture" } })).toBe(false);
    },
  );

  it.each(["mobile", "wechat", "website", "unmounted"])(
    "rejects the unmounted credential tag %s",
    (type) => {
      expect(validInput({ ...input, credential: { ...input.credential, type } })).toBe(false);
    },
  );

  it.each([null, true, 1, "input", [], {}, { ...input, credential: [] }])(
    "rejects a malformed original login record: %j",
    (value) => expect(validInput(value)).toBe(false),
  );

  it("rejects malformed registration tokens in saved projections", () => {
    for (const token of malformedTokens)
      expect(validRegistration({ ...registration, token })).toBe(false);
  });

  it("rejects malformed registration tokens in successful login responses", async () => {
    for (const token of malformedTokens) {
      const fetch = vi.fn().mockResolvedValue(Response.json({
        code: "ok", registration: { ...registration, token },
      }));
      vi.stubGlobal("fetch", fetch);
      expect(await login(id, input, new AbortController().signal)).toEqual({
        kind: "untrusted",
      });
      expect(fetch).toHaveBeenCalledOnce();
    }
  });

  it("accepts canonical encodings of 32 bytes including every valid final sextet", async () => {
    const tokens = [
      Buffer.from(Array.from({ length: 32 }, (_, index) => index)).toString("base64url"),
      Buffer.alloc(32, 251).toString("base64url"),
      ...Array.from({ length: 16 }, (_, last) =>
        Buffer.concat([Buffer.alloc(31), Buffer.from([last])]).toString("base64url"),
      ),
    ];
    for (const token of tokens) {
      const value = { ...registration, token };
      expect(validRegistration(value)).toBe(true);
      const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", registration: value }));
      vi.stubGlobal("fetch", fetch);
      expect(await login(id, input, new AbortController().signal)).toEqual({
        kind: "registration", registration: value,
      });
      expect(fetch).toHaveBeenCalledOnce();
    }
  });

  it.each([
    { kind: "session", session },
    { kind: "registration", registration },
  ])(
    "returns the validated $kind branch without choosing a session",
    async (result) => {
      const { kind, ...receipt } = result;
      const fetch = vi
        .fn()
        .mockResolvedValue(Response.json({ code: "ok", ...receipt }));
      vi.stubGlobal("fetch", fetch);
      expect(await login(id, input, new AbortController().signal)).toEqual(
        result,
      );
      expect(fetch).toHaveBeenCalledOnce();
    },
  );

  it("validates saved projections using the generated Session and Registration contracts", () => {
    expect(validSession(session)).toBe(true);
    expect(validRegistration(registration)).toBe(true);
    for (const value of [
      null,
      {},
      { id },
      { ...session, id: "invalid" },
      { ...session, expiresAt: "later" },
    ])
      expect(validSession(value)).toBe(false);
    for (const value of [
      null,
      session,
      { ...registration, expiresAt: "later" },
    ])
      expect(validRegistration(value)).toBe(false);
  });

  it.each([
    registration,
    { ...session, token: registration.token },
    { ...session, token: null },
    { ...session, unknown: "fixture" },
  ])("rejects an inexact website session projection: %j", async (value) => {
    expect(validSession(value)).toBe(false);
    const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", session: value }));
    vi.stubGlobal("fetch", fetch);
    expect(await login(id, input, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it.each([
    { ...registration, session },
    { ...registration, unknown: "fixture" },
  ])("rejects an inexact registration projection: %j", async (value) => {
    expect(validRegistration(value)).toBe(false);
    const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", registration: value }));
    vi.stubGlobal("fetch", fetch);
    expect(await login(id, input, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  const extraResults = [
    ["progress", { status: "pending", retryAfterSeconds: 5 }],
    ["authorization", { id, href: "/example", expiresAt: session.expiresAt }],
    ["message", { id, delivery: "sent", expiresAt: session.expiresAt }],
    ["session", session],
    ["registration", registration],
  ] as const;
  it.each(
    [{ session }, { registration }].flatMap((receipt) =>
      extraResults.filter(([field]) => !(field in receipt)).flatMap(([field, value]) =>
        [value, {}, null].map((extra) => ({ receipt, field, extra })),
      ),
    ),
  )("rejects a parallel $field result alongside $receipt: $extra", async ({ receipt, field, extra }) => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", ...receipt, [field]: extra }));
    vi.stubGlobal("fetch", fetch);
    expect(await login(id, input, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it("accepts only the delivered session or registration branch, including their mutual exclusion", async () => {
    for (const result of [{ session }, { registration }])
      expect(
        await decode(
          Response.json({ code: "ok", ...result }),
          "POST /api/v1/sessions",
        ),
      ).toBeDefined();
    for (const result of [
      { session, registration },
      { session: { expiresAt: session.expiresAt } },
      { session: { ...session, expiresAt: "later" } },
      { progress: { status: "pending", retryAfterSeconds: 5 } },
      { authorization: { id, href: "/example" } },
      { session: { token: "A".repeat(43), expiresAt: session.expiresAt } },
    ])
      expect(
        await decode(
          Response.json({ code: "ok", ...result }),
          "POST /api/v1/sessions",
        ),
      ).toBeUndefined();
  });

  it("preserves the exact original operation and leading zeroes without a transport retry", async () => {
    const fetch = vi
      .fn()
      .mockRejectedValue(new TypeError("fixture disconnect"));
    vi.stubGlobal("fetch", fetch);
    expect(validInput(input)).toBe(true);
    for (const code of ["12345", "1234567", "12345678", "１２３４５６", 123456])
      expect(
        validInput({ ...input, credential: { ...input.credential, code } }),
      ).toBe(false);
    expect(await login(id, input, new AbortController().signal)).toEqual({
      kind: "disconnected",
    });
    expect(fetch).toHaveBeenCalledTimes(1);
    const [path, options] = fetch.mock.calls[0];
    expect(path).toBe("/api/v1/sessions");
    expect(options.headers["Operation-Id"]).toBe(id);
    expect(options.credentials).toBe("same-origin");
    expect(JSON.parse(options.body)).toEqual(input);
  });

  it("reads only the explicitly selected cookie-backed session and never supplies a token", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(
        Response.json({ code: "authentication_invalid" }, { status: 401 }),
      );
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toMatchObject({
      kind: "failure",
      code: "authentication_invalid",
      status: 401,
    });
    expect(fetch).toHaveBeenCalledTimes(1);
    const [path, options] = fetch.mock.calls[0];
    expect(path).toBe("/api/v1/accounts/current");
    expect(options.credentials).toBe("same-origin");
    expect(options.headers["Session-Id"]).toBe(id);
    expect(options.headers).not.toHaveProperty("Authorization");
    expect(options.body).toBeUndefined();
  });

  it("does not interpret a missing required current-account field as empty data", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(Response.json({ code: "ok", account: { id } }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({
      kind: "untrusted",
    });
  });

  it("preserves the complete current account and an explicit unbound or bound value", async () => {
    for (const binding of [null, { revision: id }]) {
      const value = { ...account, binding };
      const fetch = vi
        .fn()
        .mockResolvedValue(Response.json({ code: "ok", account: value }));
      vi.stubGlobal("fetch", fetch);
      expect(await current(id, new AbortController().signal)).toEqual({
        kind: "account",
        account: value,
      });
      expect(fetch).toHaveBeenCalledOnce();
    }
  });

  it.each([
    "Á名", "𠀀".repeat(64), "A B", "A\u00a0B", "A\u2028B", "A\u200dB", "\uFEFFA\uFEFF", "\uFEFF",
  ])("preserves a canonical Unicode name: %j", async (name) => {
    const value = { ...account, name };
    const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", account: value }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "account", account: value });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it.each([
    "", "A".repeat(65), "𠀀".repeat(65), "A\u0301名", " 名", "名 ",
    "\u00a0名", "名\u00a0", "\u2000名", "名\u2028", "名\u2029", "名\u3000",
    "A\0B", "A\u007fB", "A\u009fB", "名\n", "名\r", "A\tB", "\ud800", "A\udfffB",
  ])("rejects a noncanonical current-account name: %j", async (name) => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", account: { ...account, name } }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it.each([1, 511, 512, 1024])("accepts a square avatar of %i pixels", async (size) => {
    const value = { ...account, avatar: { ...account.avatar, width: size, height: size } };
    const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", account: value }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "account", account: value });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it.each([
    { mediaType: "image/png" }, { mediaType: "" }, { width: 0 }, { width: 511 },
    { width: 1024 }, { width: 512.5 }, { height: 513 }, { revision: "invalid" },
    { width: 1025, height: 1025 }, { width: 0, height: 0 },
  ])("rejects unsupported avatar metadata: %j", async (change) => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", account: { ...account, avatar: { ...account.avatar, ...change } } }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  const signedPath = `/api/v1/avatars/${"a".repeat(64)}.webp`;
  const signature = Buffer.from(Array.from({ length: 32 }, (_, i) => i)).toString("base64url");
  const signedQuery = `key_id=7&expires_at=2051222400&signature=${signature}`;
  it.each([
    "", signedPath, `http://media.invalid${signedPath}?${signedQuery}`,
    `https://media.invalid/wrong/${"a".repeat(64)}.webp?${signedQuery}`,
    `https://media.invalid/api/v1/avatars/${"A".repeat(64)}.webp?${signedQuery}`,
    `https://user:password@media.invalid${signedPath}?${signedQuery}`,
    `https:/media.invalid${signedPath}?${signedQuery}`,
    `https://media.invalid:99999${signedPath}?${signedQuery}`,
    `https://media.invalid${signedPath}?${signedQuery}#`,
    `https://media.invalid${signedPath}?${signedQuery}#fragment`,
    ` https://media.invalid${signedPath}?${signedQuery}`,
    `https://media.invalid${signedPath}?${signedQuery}\n`,
    `https://media.invalid${signedPath}?${signedQuery}&unknown=1`,
    ...["key_id", "expires_at", "signature"].flatMap((field) => {
      const query = new URLSearchParams(signedQuery);
      query.delete(field);
      return [`https://media.invalid${signedPath}?${query}`, `https://media.invalid${signedPath}?${signedQuery}&${field}=0`];
    }),
    ...["-1", "-0", "+7", "07", "9223372036854775808", "1".repeat(20), "7.0", "7e0", "not-a-number"].map((key) =>
      `https://media.invalid${signedPath}?key_id=${key}&expires_at=2051222400&signature=${signature}`),
    ...["-1", "02051222400", "18446744073709551616", "1".repeat(21), "2051222401", "2051222400.0"].map((seconds) =>
      `https://media.invalid${signedPath}?key_id=7&expires_at=${seconds}&signature=${signature}`),
    ...["", "A".repeat(42), "A".repeat(44), "A".repeat(42) + "B", "A".repeat(42) + "=", "!".repeat(43)].map((tag) =>
      `https://media.invalid${signedPath}?key_id=7&expires_at=2051222400&signature=${tag}`),
    `https://media.invalid${signedPath}?key_id=%37&expires_at=2051222400&signature=${signature}`,
    `https://media.invalid${signedPath}?key_id=7&expires_at=2051222400&signature=${signature}&`,
  ])("rejects an invalid signed avatar projection: %j", async (href) => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", account: { ...account, avatar: { ...account.avatar, href } } }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it.each([
    { origin: "https://media.invalid:8443", key: "0", seconds: "2051222400", expiresAt: session.expiresAt },
    { origin: "https://[2001:db8::1]:8443", key: "9223372036854775807", seconds: "2051222400", expiresAt: session.expiresAt },
    { origin: "https://other.invalid", key: "9007199254740993", seconds: "0", expiresAt: "1970-01-01T00:00:00Z" },
  ])("preserves a valid independent avatar origin and canonical integers: $origin", async ({ origin, key, seconds, expiresAt }) => {
    const avatar = { ...account.avatar, href: `${origin}${signedPath}?signature=${signature}&expires_at=${seconds}&key_id=${key}`, expiresAt };
    const value = { ...account, avatar };
    const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", account: value }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "account", account: value });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it("rejects a subsecond avatar expiry that does not match the signed seconds", async () => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", account: { ...account, avatar: { ...account.avatar, expiresAt: "2035-01-01T00:00:00.0001Z" } } }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it("preserves transport and business failures without interpreting a private projection", async () => {
    const fetch = vi.fn().mockRejectedValueOnce(new TypeError("fixture disconnect"))
      .mockResolvedValueOnce(Response.json({ code: "dependency_unavailable" }, { status: 503 }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "disconnected" });
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "failure", status: 503, code: "dependency_unavailable", retryAfter: 0 });
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it.each([
    { code: "ok", account, progress: { status: "pending" } },
    { code: "ok", account, session },
    { code: "ok", account: { ...account, name: undefined } },
    { code: "ok", account: { ...account, id: "invalid" } },
    { code: "ok", account: { ...account, binding: undefined } },
    { code: "ok", account: { ...account, binding: {} } },
    { code: "ok", account: { ...account, binding: { revision: "" } } },
    { code: "ok", account: { ...account, binding: { revision: "invalid" } } },
    { code: "ok", account: { ...account, binding: { revision: null } } },
    { code: "ok", account: { ...account, binding: "bound" } },
    { code: "ok", account: { ...account, binding: [] } },
    { code: "ok", account: { ...account, token: "fixture" } },
    { code: "ok", account: { ...account, idNumber: { suffix: "002X", number: "11010519491231002X" } } },
    { code: "ok", account: { ...account, binding: { revision: id, unionId: "fixture" } } },
    { code: "ok", account: { ...account, binding: { revision: id, openId: "fixture" } } },
    { code: "ok", account: { ...account, avatar: { ...account.avatar, token: "fixture" } } },
  ])("rejects an inexact or malformed private projection: %j", async (receipt) => {
    const fetch = vi.fn().mockResolvedValue(Response.json(receipt));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it.each([
    "", "0", "001", "00010", "11010519491231002X", "002x", "X020",
    "0X20", "00X0", "002A", "００２Ｘ", " 002X", "002X ", "002X\n",
    "002X\r", "002X\r\n", null, 10, true, [], {},
  ])("rejects an untrusted current-account identity suffix: %j", async (suffix) => {
    const fetch = vi.fn().mockResolvedValue(Response.json({
      code: "ok", account: { ...account, idNumber: { suffix } },
    }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it("rejects a current-account identity projection without its suffix", async () => {
    const fetch = vi.fn().mockResolvedValue(Response.json({
      code: "ok", account: { ...account, idNumber: {} },
    }));
    vi.stubGlobal("fetch", fetch);
    expect(await current(id, new AbortController().signal)).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it.each(["0010", "002X", "0000"])(
    "preserves the normalized identity suffix and leading zeros: %s",
    async (suffix) => {
      const value = { ...account, idNumber: { suffix } };
      const fetch = vi.fn().mockResolvedValue(Response.json({ code: "ok", account: value }));
      vi.stubGlobal("fetch", fetch);
      expect(await current(id, new AbortController().signal)).toEqual({
        kind: "account", account: value,
      });
      expect(fetch).toHaveBeenCalledOnce();
    },
  );

  it.each(["login", "current"])(
    "rejects media, status, code and JSON contradictions for %s",
    async (adapter) => {
      const receipt = adapter === "login" ? { session } : { account };
      for (const response of [
        new Response(JSON.stringify({ code: "ok", ...receipt }), {
          headers: { "Content-Type": "text/html" },
        }),
        Response.json({ code: "ok", ...receipt }, { status: 201 }),
        Response.json({ code: "rate_limited" }, { status: 403 }),
        Response.json({ code: "invented" }, { status: 503 }),
        new Response("{", { headers: { "Content-Type": "application/json" } }),
      ]) {
        const fetch = vi.fn().mockResolvedValue(response);
        vi.stubGlobal("fetch", fetch);
        const signal = new AbortController().signal;
        expect(
          await (adapter === "login"
            ? login(id, input, signal)
            : current(id, signal)),
        ).toEqual({ kind: "untrusted" });
        expect(fetch).toHaveBeenCalledOnce();
      }
    },
  );

  it.each([
    { adapter: "login", status: 403, code: "verification_invalid", field: "session", value: session },
    { adapter: "login", status: 503, code: "outcome_unknown", field: "registration", value: registration },
    { adapter: "login", status: 403, code: "verification_invalid", field: "session", value: null },
    { adapter: "login", status: 401, code: "authentication_invalid", field: "registration", value: {} },
    { adapter: "login", status: 429, code: "rate_limited", field: "progress", value: { status: "pending", retryAfterSeconds: 5 } },
    { adapter: "current", status: 401, code: "authentication_invalid", field: "account", value: account },
    { adapter: "current", status: 403, code: "permission_denied", field: "authorization", value: { id, href: "/example" } },
    { adapter: "current", status: 503, code: "dependency_unavailable", field: "account", value: null },
    { adapter: "current", status: 429, code: "rate_limited", field: "unknown", value: "fixture" },
  ])("rejects $adapter $status errors containing $field", async ({ adapter, status, code, field, value }) => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ code, [field]: value }, { status }));
    vi.stubGlobal("fetch", fetch);
    const signal = new AbortController().signal;
    expect(await (adapter === "login" ? login(id, input, signal) : current(id, signal))).toEqual({ kind: "untrusted" });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it.each([
    { adapter: "login", status: 403, code: "verification_invalid" },
    { adapter: "login", status: 401, code: "authentication_invalid" },
    { adapter: "login", status: 429, code: "rate_limited" },
    { adapter: "login", status: 503, code: "outcome_unknown" },
    { adapter: "current", status: 403, code: "permission_denied" },
    { adapter: "current", status: 401, code: "authentication_invalid" },
    { adapter: "current", status: 429, code: "rate_limited" },
    { adapter: "current", status: 503, code: "dependency_unavailable" },
  ])("preserves a code-only $adapter $status failure and header metadata", async ({ adapter, status, code }) => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ code }, { status, headers: { "Retry-After": "3" } }));
    vi.stubGlobal("fetch", fetch);
    const signal = new AbortController().signal;
    expect(await (adapter === "login" ? login(id, input, signal) : current(id, signal))).toEqual({ kind: "failure", status, code, retryAfter: 3000 });
    expect(fetch).toHaveBeenCalledOnce();
  });

  it.each(["login", "current"])(
    "uses the bounded shared retry delay for %s failures",
    async (adapter) => {
      const fetch = vi
        .fn()
        .mockResolvedValue(
          Response.json(
            { code: "rate_limited" },
            { status: 429, headers: { "Retry-After": "9".repeat(400) } },
          ),
        );
      vi.stubGlobal("fetch", fetch);
      const signal = new AbortController().signal;
      expect(
        await (adapter === "login"
          ? login(id, input, signal)
          : current(id, signal)),
      ).toEqual({
        kind: "failure",
        status: 429,
        code: "rate_limited",
        retryAfter: 9007199254740991,
      });
      expect(fetch).toHaveBeenCalledOnce();
    },
  );
});
