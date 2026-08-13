import { expect, test } from "bun:test";
import { Kiteframe, type LocalOptions, type SessionPolicy } from "../src/client";

const emptyPolicy: SessionPolicy = { version: 1, rules: [] };

test("local initializes ACP, persists typed policy, and starts a workflow", async () => {
  const script = `
    const reader = Bun.stdin.stream().getReader();
    const decoder = new TextDecoder(); let pending = "";
    for (;;) {
      const { done, value } = await reader.read(); if (done) break;
      pending += decoder.decode(value, { stream: true });
      let newline; while ((newline = pending.indexOf("\\n")) >= 0) {
        const message = JSON.parse(pending.slice(0, newline)); pending = pending.slice(newline + 1);
        let result;
        if (message.method === "initialize") result = { protocolVersion: 1, agentInfo: { name: "kiteframe", version: "0.1.0" }, _meta: message.params._meta };
        else if (message.method === "session/new") result = { sessionId: "019ff3d8-9d86-7de0-9070-0f0185d5f413" };
        else if (message.method === "_kiteframe/session/policy/update") result = { revision: 1 };
        else if (message.method === "_kiteframe/workflow/publish") result = { id: "019ff3d8-9d86-7de0-9070-0f0185d5f414", name: "support", version: 1, definitionHash: "abc" };
        else if (message.method === "_kiteframe/run/start") result = { id: "019ff3d8-9d86-7de0-9070-0f0185d5f415", sessionId: "019ff3d8-9d86-7de0-9070-0f0185d5f413", workflowName: "support", version: 1, state: "runnable", revision: 0 };
        else if (message.method === "_kiteframe/run/event") result = { events: [{ sequence: 4, kind: "run.started", metadata: {} }] };
        else result = {};
        console.log(JSON.stringify({ jsonrpc: "2.0", id: message.id, result }));
      }
    }`;
  const options: LocalOptions = {
    config: {
      databaseUrl: "postgres://test",
      principal: "typescript-test",
      responsesBaseUrl: "http://responses.test/v1",
      responsesApiKey: "not-a-secret",
      responsesModel: "test-model",
      command: [process.execPath, "-e", script],
    },
    policy: emptyPolicy,
  };

  const client = await Kiteframe.local(options);
  const published = await client.publish("workflow support() -> string { start t() }");
  const run = await client.start("support", {}, { idempotencyKey: "typescript-start-1" });
  const events = await client.events(run.id, { after: 3, limit: 10 });
  await client.close();

  expect(published.name).toBe("support");
  expect(run.sessionId).toBe(client.sessionId);
  expect(events.cursor).toBe(4);
});

test("remote connects over WebSocket before considering HTTP fallback", async () => {
  const server = Bun.serve({
    port: 0,
    fetch(request, server) {
      if (new URL(request.url).pathname === "/acp") return server.upgrade(request) ? undefined : new Response("upgrade failed", { status: 500 });
      return new Response("not found", { status: 404 });
    },
    websocket: {
      message(socket, data) {
        const message = JSON.parse(String(data));
        let result: object;
        if (message.method === "initialize") result = { protocolVersion: 1, agentInfo: { name: "kiteframe", version: "0.1.0" }, _meta: message.params._meta };
        else if (message.method === "session/new") result = { sessionId: "019ff3d8-9d86-7de0-9070-0f0185d5f413" };
        else if (message.method === "_kiteframe/session/policy/update") result = { revision: 1 };
        else result = { id: "019ff3d8-9d86-7de0-9070-0f0185d5f414", name: "support", version: 1, definitionHash: "abc" };
        socket.send(JSON.stringify({ jsonrpc: "2.0", id: message.id, result }));
      },
    },
  });
  try {
    const url = new URL(server.url.toString().replace("http://", "ws://") + "acp");
    const client = await Kiteframe.connect(url, { token: "test-token", policy: emptyPolicy });
    expect((await client.publish("source")).name).toBe("support");
    await client.close();
  } finally {
    server.stop(true);
  }
});

test("remote falls back to Streamable HTTP only after a WebSocket upgrade rejection", async () => {
  const server = Bun.serve({
    port: 0,
    async fetch(request) {
      if (request.method !== "POST" || new URL(request.url).pathname !== "/acp") return new Response("not found", { status: 404 });
      const message = await request.json() as { id: number; method: string; params: Record<string, unknown> };
      let result: object;
      if (message.method === "initialize") result = { protocolVersion: 1, agentInfo: { name: "kiteframe", version: "0.1.0" }, _meta: message.params._meta };
      else if (message.method === "session/new") result = { sessionId: "019ff3d8-9d86-7de0-9070-0f0185d5f413" };
      else if (message.method === "_kiteframe/session/policy/update") result = { revision: 1 };
      else result = { id: "019ff3d8-9d86-7de0-9070-0f0185d5f414", name: "support", version: 1, definitionHash: "abc" };
      return Response.json({ jsonrpc: "2.0", id: message.id, result }, { headers: { "Acp-Connection-Id": "connection-1" } });
    },
  });
  try {
    const client = await Kiteframe.connect(new URL(server.url.toString() + "acp"), { token: "test-token", policy: emptyPolicy });
    expect((await client.publish("source")).name).toBe("support");
    await client.close();
  } finally {
    server.stop(true);
  }
});

test("remote never falls back after an authentication failure", async () => {
  let posts = 0;
  const server = Bun.serve({
    port: 0,
    fetch(request) {
      if (request.method === "POST") posts += 1;
      return new Response("unauthorized", { status: 401 });
    },
  });
  try {
    await expect(Kiteframe.connect(new URL(server.url.toString() + "acp"), { token: "bad-token", policy: emptyPolicy })).rejects.toThrow("HTTP 401");
    expect(posts).toBe(0);
  } finally {
    server.stop(true);
  }
});

test("local offers runtime-enabled ACP v2 then negotiates v1 on a negotiation error", async () => {
  const script = `
    const reader = Bun.stdin.stream().getReader(); const decoder = new TextDecoder(); let pending = ""; let offeredV2 = false;
    for (;;) { const { done, value } = await reader.read(); if (done) break; pending += decoder.decode(value, { stream: true });
      let newline; while ((newline = pending.indexOf("\\n")) >= 0) { const message = JSON.parse(pending.slice(0, newline)); pending = pending.slice(newline + 1);
        if (message.method === "initialize" && message.params.protocolVersion === 2) { offeredV2 = true; console.log(JSON.stringify({ jsonrpc: "2.0", id: message.id, error: { code: -32602, message: "v2 disabled" } })); }
        else if (message.method === "initialize" && offeredV2) console.log(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { protocolVersion: 1, agentInfo: { name: "kiteframe", version: "0.1.0" }, _meta: message.params._meta } }));
        else if (message.method === "initialize") console.log(JSON.stringify({ jsonrpc: "2.0", id: message.id, error: { code: -32602, message: "must offer v2 first" } }));
        else if (message.method === "session/new") console.log(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { sessionId: "019ff3d8-9d86-7de0-9070-0f0185d5f413" } }));
        else console.log(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { revision: 1 } }));
      }
    }`;
  const client = await Kiteframe.local({ config: { databaseUrl: "postgres://test", principal: "typescript-test", responsesBaseUrl: "http://responses.test/v1", responsesApiKey: "not-a-secret", responsesModel: "test-model", enableAcpV2: true, command: [process.execPath, "-e", script] }, policy: emptyPolicy });
  await client.close();
  expect(client.sessionId).toBe("019ff3d8-9d86-7de0-9070-0f0185d5f413");
});

test("local maps an unsolicited permission request through its typed handler", async () => {
  const script = `
    const reader = Bun.stdin.stream().getReader(); const decoder = new TextDecoder(); let pending = ""; let permissionSent = false;
    for (;;) { const { done, value } = await reader.read(); if (done) break; pending += decoder.decode(value, { stream: true });
      let newline; while ((newline = pending.indexOf("\\n")) >= 0) { const message = JSON.parse(pending.slice(0, newline)); pending = pending.slice(newline + 1);
        if (message.method === "initialize") console.log(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { protocolVersion: 1, agentInfo: { name: "kiteframe", version: "0.1.0" }, _meta: message.params._meta } }));
        else if (message.method === "session/new") console.log(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { sessionId: "019ff3d8-9d86-7de0-9070-0f0185d5f413" } }));
        else if (message.method === "_kiteframe/session/policy/update") console.log(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { revision: 1 } }));
        else if (message.method === "_kiteframe/workflow/publish" && !permissionSent) { permissionSent = true; console.log(JSON.stringify({ jsonrpc: "2.0", id: "permission-1", method: "session/request_permission", params: { title: "Allow", options: [{ optionId: "allow-once" }, { optionId: "deny-once" }] } })); }
        else if (message.id === "permission-1" && message.result?.outcome?.optionId === "allow-once") console.log(JSON.stringify({ jsonrpc: "2.0", id: 4, result: { id: "019ff3d8-9d86-7de0-9070-0f0185d5f414", name: "support", version: 1, definitionHash: "abc" } }));
      }
    }`;
  const client = await Kiteframe.local({
    config: { databaseUrl: "postgres://test", principal: "typescript-test", responsesBaseUrl: "http://responses.test/v1", responsesApiKey: "not-a-secret", responsesModel: "test-model", command: [process.execPath, "-e", script] },
    policy: emptyPolicy,
    permissionHandler: request => {
      expect(request.title).toBe("Allow");
      return "allow-once";
    },
  });
  expect((await client.publish("source")).name).toBe("support");
  await client.close();
});
