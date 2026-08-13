import { expect, test } from "bun:test";
import { resolveHarness } from "../src/binary";

test("uses an explicit harness command before platform package resolution", () => {
  expect(resolveHarness(["/tmp/kiteframe-harness"])).toEqual(["/tmp/kiteframe-harness"]);
});

test("reports the unsupported platform when no packaged harness is installed", () => {
  expect(() => resolveHarness()).toThrow(`no Kiteframe harness package is available for ${process.platform}/${process.arch}`);
});
