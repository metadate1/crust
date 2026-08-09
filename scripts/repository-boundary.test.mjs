import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("repository boundary rejects mixed-case retail recordings and disc data", async () => {
  const [ignore, workflow] = await Promise.all([
    readFile(new URL(".gitignore", root), "utf8"),
    readFile(new URL(".github/workflows/ci.yml", root), "utf8"),
  ]);

  assert.match(ignore, /^\*\.\[pP\]\[bB\]\[aA\]\[kK\]$/m);
  assert.ok(
    ignore.split("\n").includes(".DS_Store"),
    "Finder metadata must be ignored at every repository depth",
  );
  for (const pattern of [
    "*.[sS][aA][vV]",
    "*.[mM][cC][rR]",
    "*.[sS][rR][mM]",
    "*.[sS][tT][aA][tT][eE]",
    "*.[sS][aA][vV][eE]",
    "*.[hH][aA][rR]",
    "*.[wW][eE][bB][mM]",
    "*.[mM][pP][4]",
    "*.[mM][oO][vV]",
    "*.[pP][1][2]",
    "*.[pP][fF][xX]",
    "*.[jJ][kK][sS]",
    "*.[kK][eE][yY][sS][tT][oO][rR][eE]",
  ]) {
    assert.ok(
      ignore.split("\n").includes(pattern),
      `${pattern} must be ignored locally`,
    );
  }
  assert.match(workflow, /^\s+shopt -s nocasematch$/m);

  const envExemption = workflow.match(
    /^\s+if (\[ "\$\{path##\*\/\}" = "\.env\.example" \]); then$/m,
  )?.[1];
  assert.ok(envExemption, "CI must exempt only an exact-case .env.example basename");

  const exemptionResult = (path) => {
    const result = spawnSync(
      "bash",
      [
        "-c",
        `shopt -s nocasematch
path=$1
if ${envExemption}; then
  printf exempt
else
  printf scanned
fi`,
        "repository-boundary-test",
        path,
      ],
      { encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    return result.stdout;
  };
  assert.equal(exemptionResult(".env.example"), "exempt");
  assert.equal(exemptionResult("config/.env.example"), "exempt");
  assert.equal(exemptionResult(".ENV.EXAMPLE"), "scanned");
  assert.equal(exemptionResult("config/.Env.Example"), "scanned");

  const prohibited = workflow.match(/^\s+prohibited='([^']+)'$/m)?.[1];
  assert.ok(prohibited, "CI must expose one auditable prohibited-path expression");
  const boundary = new RegExp(prohibited, "i");

  for (const path of [
    "disc.BIN",
    "streams/S0000019.NSF",
    "recordings/demo.PBAK",
    "captures/playthrough.MP4",
    ".ENV.EXAMPLE",
    "config/.Env.Example",
  ]) {
    assert.match(path, boundary, `${path} must be rejected`);
  }
  for (const path of [
    "crates/formats/src/stream/pbak.rs",
    "docs/BROWSER_CAMPAIGN_REPLAY.md",
    "web/assets/crust-wordmark.png",
  ]) {
    assert.doesNotMatch(path, boundary, `${path} must remain valid source material`);
  }
});
