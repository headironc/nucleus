import { spawnSync } from "node:child_process";
import { repository, requireThat } from "./identity.ts";
import type { Record } from "./providers.ts";

export interface Api {
  get(path: string): Promise<any>;
  pages(path: string, key?: string): Promise<Record[]>;
  graphql(query: string, variables: Record): Promise<any>;
  receipt(
    artifact: Record,
    filename?: "receipt.json" | "operation.json" | "review-start.json",
  ): Promise<any>;
  post(path: string, body: Record): Promise<any>;
  put(path: string, body: Record): Promise<any>;
}
export class Github implements Api {
  constructor(
    private readToken: string,
    private writeToken?: string,
  ) {}
  private async request(path: string, method = "GET", body?: Record) {
    requireThat(
      path.startsWith(`/repos/${repository}/`) ||
        path === "/graphql" ||
        (method === "GET" && path === "/users/cladiron%5Bbot%5D"),
      "API target is outside repository",
    );
    const token =
      method === "GET" || path === "/graphql"
        ? this.readToken
        : this.writeToken;
    requireThat(token, "No scoped token for operation");
    const response = await fetch(`https://api.github.com${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "Content-Type": "application/json",
      },
      body: body ? JSON.stringify(body) : undefined,
      signal: AbortSignal.timeout(30000),
      redirect: "error",
    });
    requireThat(
      response.ok,
      `GitHub ${method} ${path.split("?")[0]} returned ${response.status}`,
    );
    return response.json() as Promise<any>;
  }
  get(path: string) {
    return this.request(path);
  }
  post(path: string, body: Record) {
    return this.request(path, "POST", body);
  }
  put(path: string, body: Record) {
    return this.request(path, "PUT", body);
  }
  async pages(path: string, key?: string): Promise<Record[]> {
    const all: Record[] = [];
    let total: number | undefined;
    // An incomplete page or API error never becomes an empty success.
    for (let page = 1; ; page++) {
      const result = await this.get(
        `${path}${path.includes("?") ? "&" : "?"}per_page=100&page=${page}`,
      );
      if (key)
        requireThat(
          result !== null &&
            typeof result === "object" &&
            !Array.isArray(result),
          "Missing keyed collection response",
        );
      const items = key ? result[key] : result;
      requireThat(Array.isArray(items), `Missing page array ${key ?? path}`);
      if (key) {
        requireThat(
          Number.isSafeInteger(result.total_count) && result.total_count >= 0,
          "Missing collection total_count",
        );
        requireThat(
          total === undefined || total === result.total_count,
          "Collection total_count changed between pages",
        );
        total = result.total_count;
      }
      all.push(...items);
      if (items.length < 100) {
        if (key) requireThat(all.length === total, "Truncated collection");
        const identities = all.map((v) => v.id ?? v.filename);
        requireThat(
          identities.every((v) =>
            typeof v === "number"
              ? Number.isSafeInteger(v) && v > 0
              : typeof v === "string" && v.length > 0,
          ) && new Set(identities).size === all.length,
          "Duplicate or unstable REST pages",
        );
        return all;
      }
    }
  }
  async graphql(query: string, variables: Record) {
    const result = await this.request("/graphql", "POST", { query, variables });
    requireThat(
      !result.errors && result.data,
      "GraphQL returned partial or missing evidence",
    );
    return result.data;
  }
  async receipt(
    artifact: Record,
    filename:
      "receipt.json" | "operation.json" | "review-start.json" = "receipt.json",
  ) {
    requireThat(
      Number.isSafeInteger(artifact.id) &&
        artifact.size_in_bytes > 0 &&
        artifact.size_in_bytes < 100000,
      "Invalid receipt artifact size/id",
    );
    const response = await fetch(
      `https://api.github.com/repos/${repository}/actions/artifacts/${artifact.id}/zip`,
      {
        headers: {
          Authorization: `Bearer ${this.readToken}`,
          "X-GitHub-Api-Version": "2022-11-28",
        },
        redirect: "manual",
        signal: AbortSignal.timeout(30000),
      },
    );
    requireThat(
      response.status === 302,
      "Receipt download did not return a signed location",
    );
    const url = new URL(response.headers.get("location") ?? "");
    requireThat(
      url.protocol === "https:" &&
        !url.username &&
        !url.password &&
        (url.hostname.endsWith(".blob.core.windows.net") ||
          url.hostname.endsWith(".githubusercontent.com")),
      "Unexpected artifact storage origin",
    );
    const archive = await fetch(url, {
      redirect: "error",
      signal: AbortSignal.timeout(30000),
    });
    requireThat(archive.ok, "Receipt storage download failed");
    const bytes = Buffer.from(await archive.arrayBuffer());
    requireThat(
      bytes.length === artifact.size_in_bytes && bytes.length < 100000,
      "Receipt archive size mismatch",
    );
    if (typeof artifact.digest === "string")
      requireThat(
        artifact.digest ===
          `sha256:${new Bun.CryptoHasher("sha256").update(bytes).digest("hex")}`,
        "Receipt artifact digest mismatch",
      );
    // Read the single bounded JSON member in memory; never extract artifact paths.
    const result = spawnSync(
      "python3",
      [
        "-c",
        "import io,sys,zipfile\nz=zipfile.ZipFile(io.BytesIO(sys.stdin.buffer.read()))\nassert z.namelist()==[sys.argv[1]]\ni=z.infolist()[0]\nassert 0<i.file_size<16384\nsys.stdout.buffer.write(z.read(i))",
        filename,
      ],
      { input: bytes, maxBuffer: 16384, timeout: 10000 },
    );
    requireThat(result.status === 0, "Cannot read CI receipt archive");
    return JSON.parse(result.stdout.toString("utf8"));
  }
}
