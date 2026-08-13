import { chmod, copyFile, mkdir, stat } from "node:fs/promises";
import { join } from "node:path";

const ARTIFACTS: Readonly<Record<string, string>> = {
  "darwin-arm64": "kiteframe-harness",
  "darwin-x64": "kiteframe-harness",
  "linux-arm64": "kiteframe-harness",
  "linux-x64": "kiteframe-harness",
  "win32-x64": "kiteframe-harness.exe",
};

async function main(): Promise<void> {
  const [platform, source] = Bun.argv.slice(2);
  if (!platform || !source || Bun.argv.length !== 4) {
    throw new Error("usage: bun run package-platform -- <platform> <harness-binary>");
  }
  const artifact = ARTIFACTS[platform];
  if (!artifact) {
    throw new Error(`unsupported Kiteframe platform package: ${platform}`);
  }
  const sourceStat = await stat(source);
  if (!sourceStat.isFile() || sourceStat.size === 0) {
    throw new Error(`harness binary is missing or empty: ${source}`);
  }
  const destination = join(import.meta.dir, "..", "npm", platform, artifact);
  await mkdir(join(import.meta.dir, "..", "npm", platform), { recursive: true });
  await copyFile(source, destination);
  if (process.platform !== "win32") {
    await chmod(destination, 0o755);
  }
}

await main();
