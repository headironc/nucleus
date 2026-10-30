// Bounded incremental parsing of a Cargo/libtest stream. The runner keeps the
// last tail for diagnostics and a small line buffer for exact result lines, so
// a huge unterminated line cannot grow memory or hide a later proof line.

export type StreamName = "stdout" | "stderr";

const TAIL_BYTES = 512 * 1024;
const MAX_PARTIAL = 256 * 1024;
const OUTPUT_LINES = 300;
const RESULT_LINE = /^test .+ \.\.\. (?:ok|FAILED|ignored)$/;
const SUMMARY_LINE = /^test result:/;

function escape(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export class OutputCapture {
  private readonly tails: Record<StreamName, string> = { stdout: "", stderr: "" };
  private readonly partials: Record<StreamName, string> = {
    stdout: "",
    stderr: "",
  };
  private readonly overflowing: Record<StreamName, boolean> = {
    stdout: false,
    stderr: false,
  };
  readonly observedResults: string[] = [];

  constructor(private readonly expectedTest: string) {}

  push(chunk: Buffer | string, stream: StreamName): void {
    let text = typeof chunk === "string" ? chunk : chunk.toString("utf8");
    this.tails[stream] += text;
    if (this.tails[stream].length > TAIL_BYTES)
      this.tails[stream] = this.tails[stream].slice(-TAIL_BYTES);
    if (this.overflowing[stream]) {
      const newline = text.indexOf("\n");
      if (newline === -1) return;
      this.overflowing[stream] = false;
      text = text.slice(newline + 1);
    }
    let buffer = this.partials[stream] + text;
    for (;;) {
      const newline = buffer.indexOf("\n");
      if (newline === -1) break;
      const line = buffer.slice(0, newline).replace(/\r$/, "");
      buffer = buffer.slice(newline + 1);
      if (RESULT_LINE.test(line) || SUMMARY_LINE.test(line)) {
        this.observedResults.push(line.slice(0, 400));
        if (this.observedResults.length > OUTPUT_LINES)
          this.observedResults.shift();
      }
    }
    if (buffer.length > MAX_PARTIAL) {
      this.partials[stream] = "";
      this.overflowing[stream] = true;
    } else {
      this.partials[stream] = buffer;
    }
  }

  tail(stream: StreamName): string {
    return this.tails[stream];
  }

  /** Exact stdout-only libtest proof, including a final line without newline. */
  completed(): boolean {
    const proof = new RegExp(
      `test ${escape(this.expectedTest)} \\.\\.\\. ok\\r?\\n\\r?\\ntest result: ok\\. 1 passed; 0 failed; 0 ignored; 0 measured; \\d+ filtered out; finished in \\d+(?:\\.\\d+)?s$`,
    );
    return proof.test(this.tails.stdout.trimEnd());
  }
}
