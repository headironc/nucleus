import { expect, test } from "bun:test";
import { ApiFailure, type Api } from "./merge-credentials";
import {
  appId,
  ownerId,
  repository,
  repositoryId,
  workflow,
  devWorkflow,
  type Ledger,
  type Protection,
} from "./merge-deployment";
import {
  history,
  OperationStore,
  operation,
  originalEnded,
  execute,
  recover,
  type Entry,
  type Operation,
} from "./merge-operation";
import {
  array,
  field,
  object,
  wire,
  type Json,
  type Record,
} from "./review-json";

const config: Ledger & Protection = {
  bot: 99n,
  issue: 1n,
  issueId: "issue-node",
  updateRule: 7n,
  rules: [7n, 8n],
};
const value: Operation = {
  version: 1n,
  stage: "intent",
  previous: null,
  intent: null,
  commit: null,
  pr: 42n,
  prId: "pull-node",
  dev: "d".repeat(40),
  base: "b".repeat(40),
  head: "a".repeat(40),
  tree: "e".repeat(40),
  lifecycle: null,
  source: "f".repeat(40),
  run: 12n,
  attempt: 1n,
  installation: 17n,
  repository: repositoryId,
  actor: ownerId,
  branch: "main",
};
const merged = "c".repeat(40);

function records(
  options: {
    lost?: boolean;
    effect?: boolean;
    pageSize?: number;
    mutate?: (nodes: Record[], page: number) => void;
  } = {},
) {
  const entries: Entry[] = [],
    writes: Json[] = [];
  const read: Api = async (path, method, body): Promise<Json> => {
    if (path === "graphql") {
      const cursor = field(field(body!, "variables"), "cursor"),
        page = cursor === null ? 0 : Number(cursor);
      const size = options.pageSize ?? 100,
        end = page + size;
      const nodes: Record[] = entries.slice(page, end).map((entry) => ({
        databaseId: entry.id,
        body:
          (entry.value.version === 1n
            ? "Nucleus promotion v1\n"
            : "Nucleus merge v2\n") + wire(entry.value),
        lastEditedAt: null,
        editor: null,
        author: {
          __typename: "Bot",
          login: "cladiron",
          databaseId: config.bot,
        },
      }));
      options.mutate?.(nodes, page);
      return {
        data: {
          repository: {
            databaseId: repositoryId,
            issue: {
              id: config.issueId,
              number: config.issue,
              comments: {
                totalCount: BigInt(entries.length),
                nodes,
                pageInfo: {
                  hasNextPage: end < entries.length,
                  endCursor: String(end),
                },
              },
            },
          },
        },
      };
    }
    expect(path).toBe(`repos/${repository}/issues/1/comments`);
    expect(method).toBe("POST");
    writes.push(body!);
    if (options.effect !== false) {
      // Preserve the exact submitted bytes rather than sharing the caller's object.
      const { parse } = await import("./review-json");
      entries.push({
        id: BigInt(entries.length + 10),
        value: operation(
          parse(String(field(body!, "body")).split("\n").slice(1).join("\n")),
        ),
      });
    }
    if (options.lost) throw new Error("response lost");
    return {};
  };
  return { store: new OperationStore(read, config), entries, writes };
}
function resultApi(
  options: {
    effect?: boolean;
    error?: Error;
    mutate?: (pull: Record, commit: Record) => void;
  } = {},
) {
  let applied = options.effect === true,
    sends = 0;
  const read: Api = async (path, method, body): Promise<Json> => {
    if (method === "PUT") {
      sends++;
      expect(body).toEqual({ sha: value.head, merge_method: "merge" });
      applied = options.effect === true;
      if (options.error) throw options.error;
      return { merged: applied };
    }
    const pull: Record = {
      number: value.pr,
      node_id: value.prId,
      url: `https://api.github.com/repos/${repository}/pulls/42`,
      base: { repo: { id: repositoryId }, ref: "main" },
      head: { repo: { id: repositoryId }, sha: value.head, ref: "release/one" },
      merged: applied,
      merge_commit_sha: merged,
    };
    const commit: Record = {
      sha: merged,
      parents: [{ sha: value.base }, { sha: value.head }],
      commit: { tree: { sha: value.tree }, verification: { verified: true } },
    };
    options.mutate?.(pull, commit);
    if (path.endsWith("/pulls/42")) return pull;
    if (path.endsWith(`/commits/${merged}`)) return commit;
    if (path.endsWith("/git/ref/heads/main"))
      return { ref: "refs/heads/main", object: { sha: merged } };
    if (path.endsWith(`/compare/${merged}...${merged}`))
      return { base_commit: { sha: merged }, status: "identical" };
    throw new Error(`Unexpected read ${path}`);
  };
  return { read, sends: () => sends };
}
const actions: Api = async () => ({
  id: value.run,
  run_attempt: value.attempt,
  repository: { id: repositoryId },
  path: workflow,
  head_sha: value.source,
  head_branch: "dev",
  actor: { id: ownerId },
  triggering_actor: { id: ownerId },
  event: "workflow_dispatch",
  status: "completed",
});

test("intent append reads back a lost response once and never recreates an unknown append", async () => {
  const success = records({ lost: true });
  expect((await success.store.append(value)).value).toEqual(value);
  expect(success.writes).toHaveLength(1);
  const unknown = records({ lost: true, effect: false });
  await expect(unknown.store.append(value)).rejects.toThrow(
    "append outcome is unknown",
  );
  expect(unknown.writes).toHaveLength(1);
});

test("complete issue pagination preserves chains and rejects edits, wrong authors, duplicate and mismatched results", async () => {
  const data = records({ pageSize: 1 });
  const intent = await data.store.append(value);
  await data.store.finish(intent, "not-sent");
  expect(await data.store.read()).toHaveLength(2);
  expect(history(await data.store.read())).toBeNull();
  for (const mutate of [
    (nodes: Record[]) => {
      nodes[0].lastEditedAt = "2026-10-26T00:00:00Z";
    },
    (nodes: Record[]) => {
      object(nodes[0].author).databaseId = 1n;
    },
    (nodes: Record[]) => {
      nodes[0].body = "untrusted note";
    },
  ]) {
    const edited = records({ mutate });
    edited.entries.push(...data.entries);
    await expect(edited.store.read()).rejects.toThrow();
  }
  expect(() => history([intent, intent])).toThrow();
  const terminal = data.entries[1];
  expect(() =>
    history([
      intent,
      { ...terminal, value: { ...terminal.value, head: value.base } },
    ]),
  ).toThrow("original intent");
  expect(() => history([terminal])).toThrow();
  expect(() => operation({ ...value, unexpected: true })).toThrow();
  expect(() => operation({ ...value, installation: null })).toThrow();
});

test("successful merge and lost merge response both send exactly once and record the verified result", async () => {
  for (const error of [undefined, new Error("response lost")]) {
    const data = records(),
      api = resultApi({ effect: true, error });
    expect(await execute(data.store, api.read, value, async () => {})).toBe(
      merged,
    );
    expect(api.sends()).toBe(1);
    expect(data.entries.at(-1)?.value.stage).toBe("merged");
    expect(history(await data.store.read())).toBeNull();
  }
});

test("unknown merge blocks later requests and can only be recovered from its original completed run", async () => {
  const data = records(),
    api = resultApi({ error: new Error("timeout") });
  await expect(
    execute(data.store, api.read, value, async () => {}),
  ).rejects.toThrow("unknown");
  await expect(
    execute(data.store, api.read, value, async () => {}),
  ).rejects.toThrow("earlier merge");
  expect(api.sends()).toBe(1);
  await expect(
    recover(data.store, api.read, actions, value.installation),
  ).rejects.toThrow("unknown");
  await expect(recover(data.store, api.read, actions, 99n)).rejects.toThrow(
    "another installation",
  );
  const running: Api = async (...args) => ({
    ...object(await actions(...args)),
    status: "in_progress",
  });
  await expect(
    recover(
      data.store,
      resultApi({ effect: true }).read,
      running,
      value.installation,
    ),
  ).rejects.toThrow("has not ended");
  expect(
    await recover(
      data.store,
      resultApi({ effect: true }).read,
      actions,
      value.installation,
    ),
  ).toBe(true);
  expect(await recover(data.store, api.read, actions, value.installation)).toBe(
    false,
  );
  expect(api.sends()).toBe(1);
});

test("final evidence drift or cancellation before send is not-sent, while definite rejection is failed", async () => {
  const data = records(),
    api = resultApi();
  await expect(
    execute(data.store, api.read, value, async () => {
      throw new Error("cancelled");
    }),
  ).rejects.toThrow("cancelled");
  expect(api.sends()).toBe(0);
  expect(data.entries.at(-1)?.value.stage).toBe("not-sent");
  for (const status of [405, 409, 422, 403, 500]) {
    const data = records(),
      api = resultApi({ error: new ApiFailure(status) });
    await expect(
      execute(data.store, api.read, value, async () => {}),
    ).rejects.toThrow();
    expect(api.sends()).toBe(1);
    expect(data.entries.at(-1)?.value.stage).toBe(
      [405, 409, 422].includes(status) ? "failed" : "intent",
    );
  }
});

test("recovery rejects incorrect parent order, tree, signature, PR identity and original actor", async () => {
  for (const mutate of [
    (_pull: Record, commit: Record) => {
      commit.parents = [{ sha: value.head }, { sha: value.base }];
    },
    (_pull: Record, commit: Record) => {
      object(field(commit.commit, "tree")).sha = value.base;
    },
    (_pull: Record, commit: Record) => {
      object(field(commit.commit, "verification")).verified = false;
    },
    (pull: Record) => {
      pull.node_id = "another-pull";
    },
  ]) {
    const data = records();
    await data.store.append(value);
    await expect(
      recover(
        data.store,
        resultApi({ effect: true, mutate }).read,
        actions,
        value.installation,
      ),
    ).rejects.toThrow();
    expect(data.entries).toHaveLength(1);
  }
  const badActor: Api = async (...args) => ({
    ...object(await actions(...args)),
    triggering_actor: { id: appId },
  });
  await expect(originalEnded(badActor, { id: 1n, value })).rejects.toThrow(
    "identity",
  );
});

test("an invalid merge-state read cannot close a rejected operation as confirmed failure", async () => {
  for (const mergedState of [null, "false", 0n]) {
    const data = records(),
      api = resultApi({
        error: new ApiFailure(409),
        mutate(pull) {
          pull.merged = mergedState;
        },
      });
    await expect(
      execute(data.store, api.read, value, async () => {}),
    ).rejects.toThrow("merge state is unknown");
    expect(api.sends()).toBe(1);
    expect(data.entries).toHaveLength(1);
    expect(history(await data.store.read())?.value.stage).toBe("intent");
  }
});

const { dev: _acceptedDev, ...binding } = value as Extract<
  Operation,
  { version: 1n }
>;
const dev: Operation = { ...binding, version: 2n, branch: "dev" };
const devActions: Api = async (...args) => ({
  ...object(await actions(...args)),
  path: devWorkflow,
});
function devResult(
  options: {
    applied?: boolean;
    parents?: Json[];
    signature?: boolean;
    reachable?: boolean;
    tree?: string;
  } = {},
) {
  let sends = 0;
  const read: Api = async (path, method, body): Promise<Json> => {
    if (method === "PUT") {
      sends++;
      expect(body).toEqual({ sha: dev.head, merge_method: "squash" });
      throw new Error("lost response");
    }
    if (path.endsWith("/pulls/42"))
      return {
        number: dev.pr,
        node_id: dev.prId,
        url: `https://api.github.com/repos/${repository}/pulls/42`,
        head: {
          repo: { id: repositoryId },
          sha: dev.head,
          ref: "feature/work",
        },
        base: { repo: { id: repositoryId }, ref: "dev" },
        merged: options.applied ?? true,
        merge_commit_sha: merged,
      };
    if (path.endsWith(`/commits/${merged}`))
      return {
        sha: merged,
        parents: options.parents ?? [{ sha: dev.base }],
        commit: {
          tree: { sha: options.tree ?? dev.tree },
          verification: { verified: options.signature ?? true },
        },
      };
    if (path.endsWith("/git/ref/heads/dev"))
      return { ref: "refs/heads/dev", object: { sha: merged } };
    if (path.includes("/compare/"))
      return {
        base_commit: { sha: merged },
        status: options.reachable === false ? "diverged" : "identical",
      };
    throw new Error(`Unexpected path ${path}`);
  };
  return { read, sends: () => sends };
}

test("dev uses one squash and confirms a signed single-parent candidate tree after response loss", async () => {
  const data = records(),
    api = devResult();
  expect(await execute(data.store, api.read, dev, async () => {})).toBe(merged);
  expect(api.sends()).toBe(1);
  expect(data.entries.at(-1)?.value.stage).toBe("merged");
  expect(() => operation({ ...dev, dev: value.head })).toThrow();
  expect(() => operation({ ...dev, branch: "main" })).toThrow();
});

test("one ledger preserves historical main records and blocks either target behind an unknown dev send", async () => {
  const data = records({ pageSize: 1 }),
    api = devResult({ applied: false });
  const first = await data.store.append(value);
  await data.store.finish(first, "not-sent");
  const next = { ...dev, previous: data.entries.at(-1)!.id };
  await expect(
    execute(data.store, api.read, next, async () => {}),
  ).rejects.toThrow("unknown");
  for (const later of [dev, value])
    await expect(
      execute(data.store, api.read, later, async () => {}),
    ).rejects.toThrow("earlier merge");
  expect(api.sends()).toBe(1);
  await expect(
    recover(data.store, devResult().read, actions, dev.installation),
  ).rejects.toThrow("identity");
  const running: Api = async (...args) => ({
    ...object(await devActions(...args)),
    status: "in_progress",
  });
  await expect(
    recover(data.store, devResult().read, running, dev.installation),
  ).rejects.toThrow("has not ended");
  expect(
    await recover(data.store, devResult().read, devActions, dev.installation),
  ).toBe(true);
  expect(history(await data.store.read())).toBeNull();
});

test("dev result mismatch stays unresolved and cannot free the shared writer", async () => {
  for (const options of [
    { parents: [{ sha: dev.base }, { sha: dev.head }] },
    { parents: [{ sha: dev.head }] },
    { signature: false },
    { tree: dev.base },
    { reachable: false },
  ]) {
    const data = records(),
      api = devResult(options);
    await expect(
      execute(data.store, api.read, dev, async () => {}),
    ).rejects.toThrow();
    expect(api.sends()).toBe(1);
    expect(history(await data.store.read())?.value.stage).toBe("intent");
  }
});
