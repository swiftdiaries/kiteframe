export class HarnessError extends Error {}

export interface HarnessResult {
  kind: string;
  [key: string]: unknown;
}

export class KiteframeClient {
  readonly #sessionId: string;
  readonly #process: ReturnType<typeof Bun.spawn>;
  readonly #stdin: ReturnType<typeof Bun.spawn>["stdin"];
  readonly #reader: ReadableStreamDefaultReader<Uint8Array>;
  #pending = "";
  #initialized = false;

  constructor(
    sessionId: string,
    command: string[] = [Bun.env.KITEFRAME_HARNESS ?? "kiteframe-harness"],
    environment: Record<string, string | undefined> = {},
  ) {
    this.#sessionId = sessionId;
    this.#process = Bun.spawn(command, { stdin: "pipe", stdout: "pipe", stderr: "pipe", env: { ...process.env, ...environment } });
    this.#stdin = this.#process.stdin;
    this.#reader = this.#process.stdout.getReader();
  }

  async close(): Promise<void> {
    this.#stdin.end();
    this.#process.kill();
    await this.#process.exited;
  }

  async call(method: string, payload: Record<string, unknown>): Promise<HarnessResult> {
    if (!this.#initialized && method !== "initialize") {
      await this.#callRaw("initialize", {});
      this.#initialized = true;
    }
    const result = await this.#callRaw(method, payload);
    if (method === "initialize") this.#initialized = true;
    return result;
  }

  async #callRaw(method: string, payload: Record<string, unknown>): Promise<HarnessResult> {
    this.#stdin.write(`${JSON.stringify({ session_id: this.#sessionId, method, payload })}\n`);
    this.#stdin.flush();
    const line = await this.#nextLine();
    let response: { ok?: boolean; result?: HarnessResult; error?: unknown };
    try {
      response = JSON.parse(line);
    } catch {
      throw new HarnessError("harness returned invalid NDJSON");
    }
    if (!response.ok || !response.result) {
      throw new HarnessError(String(response.error ?? "unknown harness error"));
    }
    return response.result;
  }

  async #nextLine(): Promise<string> {
    while (!this.#pending.includes("\n")) {
      const { done, value } = await this.#reader.read();
      if (done) {
        const stderr = await new Response(this.#process.stderr).text();
        throw new HarnessError(`harness exited without a response: ${stderr.trim()}`);
      }
      this.#pending += new TextDecoder().decode(value, { stream: true });
    }
    const newline = this.#pending.indexOf("\n");
    const line = this.#pending.slice(0, newline);
    this.#pending = this.#pending.slice(newline + 1);
    return line;
  }
}
