import { expect, test } from "bun:test";
import { KiteframeClient } from "../src/client";

test("sends a session-bound NDJSON envelope", async () => {
  const script = "const reader=Bun.stdin.stream().getReader(); const {value}=await reader.read(); const r=JSON.parse(new TextDecoder().decode(value)); console.log(JSON.stringify({ok:true,result:{kind:'ack',session:r.session_id}}));";
  const client = new KiteframeClient("019ff3d8-9d86-7de0-9070-0f0185d5f413", [process.execPath, "-e", script]);
  const result = await client.call("session/list", {});
  await client.close();
  expect(result.kind).toBe("ack");
  expect(result.session).toBe("019ff3d8-9d86-7de0-9070-0f0185d5f413");
});

test("calls the compiled Rust harness", async () => {
  if (!Bun.env.DATABASE_URL) return;
  const harness = new URL("../../../target/debug/kiteframe-harness", import.meta.url).pathname;
  if (!(await Bun.file(harness).exists())) return;
  const environment = {
    KITEFRAME_DATABASE_URL: Bun.env.DATABASE_URL,
    KITEFRAME_PRINCIPAL: "typescript-smoke",
    KITEFRAME_RESPONSES_BASE_URL: "https://api.example.test/v1",
    KITEFRAME_RESPONSES_API_KEY: "not-a-real-key",
    KITEFRAME_RESPONSES_MODEL: "test-model",
  };
  try {
    const client = new KiteframeClient("019ff3d8-9d86-7de0-9070-0f0185d5f413", [harness], environment);
    const result = await client.call("session/list", {});
    await client.close();
    expect(result.kind).toBe("sessions");
  } finally {
    // The override is passed only to this child process.
  }
});
