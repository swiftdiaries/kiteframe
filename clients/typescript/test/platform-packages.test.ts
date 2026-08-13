import { expect, test } from "bun:test";

const packages = ["darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64", "win32-x64"];

test("every supported platform package contains exactly one harness declaration", async () => {
  for (const platform of packages) {
    const manifest = await Bun.file(new URL(`../npm/${platform}/package.json`, import.meta.url)).json() as { files: string[]; bin: Record<string, string> };
    expect(manifest.files).toHaveLength(1);
    expect(manifest.bin["kiteframe-harness"]).toBe(manifest.files[0]);
  }
});
