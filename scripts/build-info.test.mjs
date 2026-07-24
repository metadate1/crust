import assert from "node:assert/strict";
import { copyFile, mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { verifyBuildInfo, writeBuildInfo } from "./build-info.mjs";

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), "crust-build-info-"));
  await Promise.all([
    mkdir(join(root, "crates", "web", "src"), { recursive: true }),
    mkdir(join(root, "web"), { recursive: true }),
    mkdir(join(root, "scripts"), { recursive: true }),
    mkdir(join(root, "dist", "pkg"), { recursive: true }),
  ]);
  const files = {
    "Cargo.toml": "[workspace]\n",
    "package.json": "{}\n",
    "crates/web/src/lib.rs": "pub fn boot() {}\n",
    "web/bootstrap.js": "start();\n",
    "scripts/build-web.sh": "#!/bin/sh\n",
    "scripts/build-info.mjs": "// fingerprint fixture\n",
    "scripts/serve.mjs": "// server fixture\n",
    "dist/bootstrap.js": "start();\n",
    "dist/index.html": "<canvas></canvas>\n",
    "dist/styles.css": "canvas {}\n",
    "dist/pkg/crust_web.js": "export default async function init() {}\n",
    "dist/pkg/crust_web_bg.wasm": "synthetic-wasm-fixture",
  };
  await Promise.all(
    Object.entries(files).map(([name, contents]) =>
      writeFile(join(root, name), contents),
    ),
  );
  return root;
}

test("verified metadata binds source and generated artifacts", async (context) => {
  const root = await fixture();
  context.after(() => rm(root, { recursive: true, force: true }));
  const identity = { commit: "a".repeat(40), dirty: false };
  const written = await writeBuildInfo(root, identity);
  const verified = await verifyBuildInfo(root, identity);
  assert.equal(verified.build_id, written.build_id);
  assert.match(verified.build_id, /^a{12}-[0-9a-f]{12}-[0-9a-f]{12}-clean$/);
});

test("verified metadata supports an isolated browser-harness distribution", async (context) => {
  const root = await fixture();
  context.after(() => rm(root, { recursive: true, force: true }));
  const harness = join(root, "target", "browser-test-dist");
  await mkdir(join(harness, "pkg"), { recursive: true });
  for (const artifact of [
    "bootstrap.js",
    "index.html",
    "styles.css",
    "pkg/crust_web.js",
    "pkg/crust_web_bg.wasm",
  ]) {
    await copyFile(join(root, "dist", artifact), join(harness, artifact));
  }
  const identity = { commit: "9".repeat(40), dirty: true };

  const written = await writeBuildInfo(root, identity, harness);
  const verified = await verifyBuildInfo(root, identity, harness);

  assert.equal(verified.build_id, written.build_id);
  assert.match(verified.build_id, /^9{12}-[0-9a-f]{12}-[0-9a-f]{12}-dirty$/);
});

test("source changes make an existing distribution stale", async (context) => {
  const root = await fixture();
  context.after(() => rm(root, { recursive: true, force: true }));
  await writeBuildInfo(root, { commit: "b".repeat(40), dirty: true });
  await writeFile(join(root, "web", "bootstrap.js"), "changed();\n");
  await assert.rejects(
    verifyBuildInfo(root),
    /source fingerprint differs/,
  );
});

test("generated artifact changes are rejected", async (context) => {
  const root = await fixture();
  context.after(() => rm(root, { recursive: true, force: true }));
  await writeBuildInfo(root, { commit: "c".repeat(40), dirty: false });
  await writeFile(join(root, "dist", "pkg", "crust_web.js"), "tampered();\n");
  await assert.rejects(
    verifyBuildInfo(root),
    /dist\/pkg\/crust_web\.js differs/,
  );
});

test("fingerprints depend on contents rather than mtimes", async (context) => {
  const root = await fixture();
  context.after(() => rm(root, { recursive: true, force: true }));
  const first = await writeBuildInfo(root, {
    commit: "d".repeat(40),
    dirty: false,
  });
  const second = await writeBuildInfo(root, {
    commit: "d".repeat(40),
    dirty: false,
  });
  assert.equal(second.source_sha256, first.source_sha256);
  assert.equal(second.build_id, first.build_id);
});

test("a missing build manifest is rejected", async (context) => {
  const root = await fixture();
  context.after(() => rm(root, { recursive: true, force: true }));
  await assert.rejects(verifyBuildInfo(root), /metadata is missing or invalid/);
});

test("changing generated JavaScript changes the build identity", async (context) => {
  const root = await fixture();
  context.after(() => rm(root, { recursive: true, force: true }));
  const identity = { commit: "e".repeat(40), dirty: true };
  const first = await writeBuildInfo(root, identity);
  await writeFile(join(root, "dist", "pkg", "crust_web.js"), "changed();\n");
  const second = await writeBuildInfo(root, identity);
  assert.notEqual(second.build_id, first.build_id);
});

test("a forged build identity is rejected", async (context) => {
  const root = await fixture();
  context.after(() => rm(root, { recursive: true, force: true }));
  const identity = { commit: "f".repeat(40), dirty: false };
  const info = await writeBuildInfo(root, identity);
  info.build_id = `0${info.build_id.slice(1)}`;
  await writeFile(
    join(root, "dist", "build-info.json"),
    `${JSON.stringify(info, null, 2)}\n`,
  );
  await assert.rejects(
    verifyBuildInfo(root, identity),
    /invalid build identity/,
  );
});

test("a different Git state is rejected", async (context) => {
  const root = await fixture();
  context.after(() => rm(root, { recursive: true, force: true }));
  const identity = { commit: "1".repeat(40), dirty: false };
  await writeBuildInfo(root, identity);
  await assert.rejects(
    verifyBuildInfo(root, { commit: "2".repeat(40), dirty: false }),
    /different Git state/,
  );
});
