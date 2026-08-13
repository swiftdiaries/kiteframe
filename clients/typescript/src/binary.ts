import { dirname, join } from "node:path";

const PLATFORM_PACKAGES: Readonly<Record<string, string>> = {
  "darwin-arm64": "@kiteframe/darwin-arm64",
  "darwin-x64": "@kiteframe/darwin-x64",
  "linux-arm64": "@kiteframe/linux-arm64",
  "linux-x64": "@kiteframe/linux-x64",
  "win32-x64": "@kiteframe/win32-x64",
};

export function resolveHarness(command?: readonly string[]): readonly string[] {
  if (command?.length) return command;
  const platform = `${process.platform}/${process.arch}`;
  const packageName = PLATFORM_PACKAGES[`${process.platform}-${process.arch}`];
  if (!packageName) throw new Error(`no Kiteframe harness package is available for ${platform}`);
  try {
    const manifest = Bun.resolveSync(`${packageName}/package.json`, import.meta.dir);
    const executable = join(dirname(manifest), process.platform === "win32" ? "kiteframe-harness.exe" : "kiteframe-harness");
    if (!Bun.file(executable).size) throw new Error("missing executable");
    return [executable];
  } catch {
    throw new Error(`no Kiteframe harness package is available for ${platform}`);
  }
}
