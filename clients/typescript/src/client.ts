const CLIENT_VERSION = "0.1.0";
const EXTENSION_VERSION = 1;

export type Primitive = string | number | boolean | null;
export type PolicyRule = {
  ruleId: string;
  effect: "permit" | "forbid";
  target: { type: "capability" | "tool"; names: readonly string[] };
  input?: Readonly<Record<string, { oneOf: readonly Primitive[] }>>;
};
export type SessionPolicy = { version: 1; rules: readonly PolicyRule[] };
export type HarnessConfig = {
  databaseUrl: string;
  principal: string;
  responsesBaseUrl: string;
  responsesApiKey: string;
  responsesModel: string;
  command?: readonly string[];
  cwd?: string;
  enableAcpV2?: boolean;
};
export type PublishedWorkflow = { id: string; name: string; version: number; definitionHash: string };
export type Run = { id: string; sessionId: string; workflowName: string; version: number; state: string; revision: number };
export type RunEvent = { sequence: number; kind: string; metadata: unknown };
export type EventPage = { events: readonly RunEvent[]; cursor: number };
export type PermissionChoice = "allow-once" | "allow-session" | "deny-once" | "deny-session";
export type PermissionRequest = { title: string; options: readonly { optionId: PermissionChoice }[] };
export type PermissionHandler = (request: PermissionRequest) => PermissionChoice | Promise<PermissionChoice>;
export type LocalOptions = { config: HarnessConfig; policy: SessionPolicy; permissionHandler?: PermissionHandler };
export type RemoteOptions = { token: string; policy: SessionPolicy; enableAcpV2?: boolean; permissionHandler?: PermissionHandler };

export class HarnessError extends Error {}
class RpcError extends HarnessError {
  constructor(readonly code: number, message: string) { super(message); }
}

type Json = Record<string, unknown>;

interface JsonRpcTransport {
  request(method: string, params: Json): Promise<Json>;
  close(): Promise<void>;
  openSession?(sessionId: string): Promise<void>;
}

type Pending = { resolve: (value: Json) => void; reject: (reason: Error) => void };

abstract class AsyncJsonRpcTransport implements JsonRpcTransport {
  #nextId = 1;
  #pending = new Map<number | string, Pending>();
  #closed = false;

  constructor(private readonly permissionHandler?: PermissionHandler) {}

  async request(method: string, params: Json): Promise<Json> {
    if (this.#closed) throw new HarnessError("Kiteframe transport is closed");
    const id = this.#nextId++;
    const response = new Promise<Json>((resolve, reject) => this.#pending.set(id, { resolve, reject }));
    await this.send({ jsonrpc: "2.0", id, method, params });
    return response;
  }

  protected async dispatch(raw: string): Promise<void> {
    const message = JSON.parse(raw) as { id?: number | string; method?: string; params?: Json; result?: Json; error?: { code?: number; message?: string } };
    if (message.method && message.id !== undefined) {
      if (message.method !== "session/request_permission") {
        await this.send({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "Method not found" } });
        return;
      }
      const request = message.params as PermissionRequest | undefined;
      const offered = new Set(request?.options?.map(option => option.optionId) ?? []);
      let selected: PermissionChoice = "deny-once";
      try {
        if (!request || !this.permissionHandler) throw new Error("no permission handler");
        selected = await this.permissionHandler(request);
        if (!offered.has(selected)) throw new Error("unoffered permission choice");
      } catch { selected = "deny-once"; }
      const outcome = offered.has(selected) ? { outcome: { outcome: "selected", optionId: selected } } : { outcome: { outcome: "cancelled" } };
      await this.send({ jsonrpc: "2.0", id: message.id, result: outcome });
      return;
    }
    if (message.id === undefined) return;
    const pending = this.#pending.get(message.id);
    if (!pending) return;
    this.#pending.delete(message.id);
    if (message.error) pending.reject(new RpcError(message.error.code ?? -32603, message.error.message ?? "request failed"));
    else if (!message.result || typeof message.result !== "object") pending.reject(new HarnessError("harness returned an invalid JSON-RPC result"));
    else pending.resolve(message.result);
  }

  protected abstract send(message: Json): Promise<void>;
  abstract close(): Promise<void>;

  protected finish(error = new HarnessError("Kiteframe transport closed")): void {
    this.#closed = true;
    for (const pending of this.#pending.values()) pending.reject(error);
    this.#pending.clear();
  }
}

class StdioTransport extends AsyncJsonRpcTransport {
  readonly #process: ReturnType<typeof Bun.spawn>;
  readonly #stdin: Bun.FileSink;
  readonly #reader: ReadableStreamDefaultReader<Uint8Array>;
  #pending = "";

  constructor(command: readonly string[], environment: Record<string, string>, permissionHandler?: PermissionHandler) {
    super(permissionHandler);
    this.#process = Bun.spawn([...command], {
      stdin: "pipe",
      stdout: "pipe",
      stderr: "pipe",
      env: environment,
    });
    this.#stdin = this.#process.stdin as Bun.FileSink;
    this.#reader = (this.#process.stdout as ReadableStream<Uint8Array>).getReader();
    void this.#read();
  }

  protected async send(message: Json): Promise<void> {
    this.#stdin.write(`${JSON.stringify(message)}\n`);
    await this.#stdin.flush();
  }

  async close(): Promise<void> {
    this.finish();
    this.#stdin.end();
    this.#process.kill();
    await this.#process.exited;
  }

  async #read(): Promise<void> {
    try {
      for (;;) {
      const { done, value } = await this.#reader.read();
      if (done) {
        const stderr = await new Response(this.#process.stderr as ReadableStream<Uint8Array>).text();
        throw new HarnessError(`harness exited without a response: ${stderr.trim()}`);
      }
      this.#pending += new TextDecoder().decode(value, { stream: true });
      let newline: number;
      while ((newline = this.#pending.indexOf("\n")) >= 0) {
        const line = this.#pending.slice(0, newline);
        this.#pending = this.#pending.slice(newline + 1);
        await this.dispatch(line);
      }
    }
    } catch (error) {
      this.finish(error instanceof Error ? error : new HarnessError("Kiteframe transport failed"));
    }
  }
}

class WebSocketTransport extends AsyncJsonRpcTransport {
  readonly #socket: WebSocket;
  #closed = false;
  #failure: Error | undefined;

  private constructor(socket: WebSocket, permissionHandler?: PermissionHandler) {
    super(permissionHandler);
    this.#socket = socket;
    socket.addEventListener("message", event => { void this.dispatch(String(event.data)); });
    socket.addEventListener("error", () => { this.#failure = new HarnessError("WebSocket transport failed"); });
    socket.addEventListener("close", () => { if (!this.#closed) this.#failure = new HarnessError("WebSocket transport closed"); });
  }

  static async open(url: URL, token: string, permissionHandler?: PermissionHandler): Promise<WebSocketTransport> {
    const socket = new WebSocket(
      url,
      { headers: { Authorization: `Bearer ${token}` } } as unknown as string[],
    );
    await new Promise<void>((resolve, reject) => {
      socket.addEventListener("open", () => resolve(), { once: true });
      socket.addEventListener("error", () => reject(new HarnessError("WebSocket connection failed")), { once: true });
    });
    return new WebSocketTransport(socket, permissionHandler);
  }

  protected async send(message: Json): Promise<void> {
    if (this.#failure) throw this.#failure;
    this.#socket.send(JSON.stringify(message));
  }

  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    this.#socket.close();
    this.finish();
  }
}

class HttpTransport extends AsyncJsonRpcTransport {
  readonly #url: URL;
  readonly #token: string;
  #connectionId: string | undefined;
  #closed = false;
  #streams = new Set<AbortController>();

  constructor(url: URL, token: string, permissionHandler?: PermissionHandler) {
    super(permissionHandler);
    this.#url = new URL(url);
    this.#url.protocol = this.#url.protocol === "wss:" ? "https:" : "http:";
    if (!this.#url.pathname.endsWith("/acp")) this.#url.pathname = `${this.#url.pathname.replace(/\/$/, "")}/acp`;
    this.#token = token;
  }

  protected async send(message: Json): Promise<void> {
    const headers: Record<string, string> = {
      Authorization: `Bearer ${this.#token}`,
      Accept: "application/json",
      "Content-Type": "application/json",
    };
    if (this.#connectionId) headers["Acp-Connection-Id"] = this.#connectionId;
    const parameters = message.params as Json | undefined;
    if (typeof parameters?.sessionId === "string") headers["Acp-Session-Id"] = parameters.sessionId;
    const response = await fetch(this.#url, { method: "POST", headers, body: JSON.stringify(message) });
    if (response.status === 401 || response.status === 403) throw new HarnessError(`Kiteframe authentication failed with HTTP ${response.status}`);
    if (!response.ok) throw new HarnessError(`Streamable HTTP request failed with HTTP ${response.status}`);
    if (!this.#connectionId) {
      const connectionId = response.headers.get("Acp-Connection-Id");
      if (!connectionId) throw new HarnessError("Streamable HTTP initialize omitted Acp-Connection-Id");
      this.#connectionId = connectionId;
    }
    await this.dispatch(JSON.stringify(await response.json()));
  }

  async openSession(sessionId: string): Promise<void> {
    await this.#openStream();
    await this.#openStream(sessionId);
  }

  async #openStream(sessionId?: string): Promise<void> {
    if (!this.#connectionId) throw new HarnessError("Streamable HTTP is not initialized");
    const controller = new AbortController();
    this.#streams.add(controller);
    const headers: Record<string, string> = {
      Authorization: `Bearer ${this.#token}`,
      Accept: "text/event-stream",
      "Acp-Connection-Id": this.#connectionId,
    };
    if (sessionId) headers["Acp-Session-Id"] = sessionId;
    const response = await fetch(this.#url, { headers, signal: controller.signal });
    if (!response.ok) throw new HarnessError(`Streamable HTTP event stream failed with HTTP ${response.status}`);
    void response.text().then(async text => {
      for (const event of text.split("\n\n")) {
        const data = event.split("\n").filter(line => line.startsWith("data:")).map(line => line.slice(5).trimStart()).join("\n");
        if (data) await this.dispatch(data);
      }
    }).finally(() => this.#streams.delete(controller));
  }

  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    for (const stream of this.#streams) stream.abort();
    this.#streams.clear();
    if (this.#connectionId) await fetch(this.#url, { method: "DELETE", headers: { Authorization: `Bearer ${this.#token}`, "Acp-Connection-Id": this.#connectionId } });
    this.finish();
  }
}

export class Kiteframe {
  readonly sessionId: string;
  readonly #transport: JsonRpcTransport;

  private constructor(transport: JsonRpcTransport, sessionId: string) {
    this.#transport = transport;
    this.sessionId = sessionId;
  }

  static async local({ config, policy, permissionHandler }: LocalOptions): Promise<Kiteframe> {
    const command = resolveHarness(config.command);
    const transport = new StdioTransport(command, {
      ...process.env,
      KITEFRAME_DATABASE_URL: config.databaseUrl,
      KITEFRAME_PRINCIPAL: config.principal,
      KITEFRAME_RESPONSES_BASE_URL: config.responsesBaseUrl,
      KITEFRAME_RESPONSES_API_KEY: config.responsesApiKey,
      KITEFRAME_RESPONSES_MODEL: config.responsesModel,
      KITEFRAME_ENABLE_ACP_V2: config.enableAcpV2 ? "true" : "false",
    }, permissionHandler);
    try {
      return await Kiteframe.#initialize(transport, policy, config.cwd ?? process.cwd(), config.enableAcpV2 ?? false);
    } catch (error) {
      await transport.close();
      throw error;
    }
  }

  static async connect(url: URL, options: RemoteOptions): Promise<Kiteframe> {
    let transport: JsonRpcTransport;
    try {
      const websocketUrl = new URL(url);
      websocketUrl.protocol = websocketUrl.protocol === "https:" ? "wss:" : "ws:";
      transport = await WebSocketTransport.open(websocketUrl, options.token, options.permissionHandler);
    } catch (error) {
      if (!(error instanceof HarnessError) || error.message !== "WebSocket connection failed") throw error;
      await rejectAuthenticationFailure(url, options.token);
      transport = new HttpTransport(url, options.token, options.permissionHandler);
    }
    try {
      return await Kiteframe.#initialize(transport, options.policy, process.cwd(), options.enableAcpV2 ?? false);
    } catch (error) {
      await transport.close();
      throw error;
    }
  }

  async publish(source: string): Promise<PublishedWorkflow> {
    return this.#transport.request("_kiteframe/workflow/publish", { source }) as Promise<PublishedWorkflow>;
  }

  async start(name: string, input: unknown, options: { idempotencyKey: string }): Promise<Run> {
    return this.#transport.request("_kiteframe/run/start", { sessionId: this.sessionId, workflowName: name, input, idempotencyKey: options.idempotencyKey }) as Promise<Run>;
  }

  async get(runId: string): Promise<Run> {
    return this.#transport.request("_kiteframe/run/get", { runId }) as Promise<Run>;
  }

  async events(runId: string, options: { after?: number; limit?: number } = {}): Promise<EventPage> {
    const after = options.after ?? 0;
    const limit = options.limit ?? 64;
    if (!Number.isInteger(after) || after < 0) throw new RangeError("event cursor must be a non-negative integer");
    if (!Number.isInteger(limit) || limit < 1 || limit > 256) throw new RangeError("event limit must be between 1 and 256");
    const result = await this.#transport.request("_kiteframe/run/event", { runId, after, limit });
    if (!Array.isArray(result.events)) throw new HarnessError("event response must contain an events array");
    const events = result.events as RunEvent[];
    return { events, cursor: events.length ? events[events.length - 1].sequence : after };
  }

  async close(): Promise<void> { await this.#transport.close(); }

  static async #initialize(transport: JsonRpcTransport, policy: SessionPolicy, cwd: string, enableAcpV2: boolean): Promise<Kiteframe> {
    const metadata = { kiteframe: { clientVersion: CLIENT_VERSION, extensionVersion: EXTENSION_VERSION } };
    let protocolVersion = enableAcpV2 ? 2 : 1;
    let initialized: Json;
    try {
      initialized = await transport.request("initialize", protocolVersion === 2
        ? { protocolVersion, capabilities: {}, info: { name: "kiteframe-typescript", version: CLIENT_VERSION }, _meta: metadata }
        : { protocolVersion, clientCapabilities: {}, clientInfo: { name: "kiteframe-typescript", version: CLIENT_VERSION }, _meta: metadata });
    } catch (error) {
      if (!(enableAcpV2 && error instanceof RpcError && error.code === -32602)) throw error;
      protocolVersion = 1;
      initialized = await transport.request("initialize", { protocolVersion, clientCapabilities: {}, clientInfo: { name: "kiteframe-typescript", version: CLIENT_VERSION }, _meta: metadata });
    }
    const agent = (protocolVersion === 1 ? initialized.agentInfo : initialized.info) as Json | undefined;
    if (initialized.protocolVersion !== protocolVersion || agent?.name !== "kiteframe" || agent?.version !== CLIENT_VERSION || JSON.stringify(initialized._meta) !== JSON.stringify(metadata)) {
      throw new HarnessError("Kiteframe harness failed ACP version negotiation");
    }
    const session = await transport.request("session/new", protocolVersion === 1 ? { cwd, mcpServers: [] } : { cwd });
    if (typeof session.sessionId !== "string") throw new HarnessError("harness returned an invalid session ID");
    await transport.openSession?.(session.sessionId);
    await transport.request("_kiteframe/session/policy/update", { expectedRevision: 0, idempotencyKey: crypto.randomUUID(), policy });
    return new Kiteframe(transport, session.sessionId);
  }
}

async function rejectAuthenticationFailure(url: URL, token: string): Promise<void> {
  const endpoint = new URL(url);
  endpoint.protocol = endpoint.protocol === "wss:" ? "https:" : "http:";
  const timeout = AbortSignal.timeout(2_000);
  try {
    const response = await fetch(endpoint, { headers: { Authorization: `Bearer ${token}` }, signal: timeout });
    if (response.status === 401 || response.status === 403) throw new HarnessError(`Kiteframe authentication failed with HTTP ${response.status}`);
    response.body?.cancel();
  } catch (error) {
    if (error instanceof HarnessError) throw error;
  }
}
import { resolveHarness } from "./binary";
