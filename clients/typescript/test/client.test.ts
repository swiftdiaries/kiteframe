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
