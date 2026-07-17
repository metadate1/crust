import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("launcher keeps the intentionally simple full-game-first hierarchy", async () => {
  const html = await readFile(new URL("web/index.html", root), "utf8");

  const insert = html.indexOf('id="dropzone"');
  const eject = html.indexOf('id="clearData"');
  const launch = html.indexOf('id="launch"');
  const level = html.indexOf('id="bootLevel"');

  assert.notEqual(insert, -1, "the local BIN/ISO action must remain present");
  assert.notEqual(eject, -1, "the local-data eject action must remain present");
  assert.notEqual(launch, -1, "the launch action must remain present");
  assert.notEqual(level, -1, "the optional direct-level selector must remain present");
  assert.ok(insert < launch, "Insert BIN/ISO must precede Launch game");
  assert.ok(eject < launch, "the secondary eject action must stay out of launch step two");
  assert.ok(launch < level, "the optional level selector must stay below Launch game");
  assert.match(html, /Insert BIN \/ ISO/);
  assert.match(html, />\s*Launch game\s*</);
  assert.match(html, /Leave on “full game” to begin at the opening/);
  assert.doesNotMatch(html, /Local-data bay|Runtime monitor|Launch Rust runtime/);
});

test("the Rust DOM contract and original interface artwork stay packaged", async () => {
  const [html, wordmark, frame, provenance] = await Promise.all([
    readFile(new URL("web/index.html", root), "utf8"),
    readFile(new URL("web/assets/crust-wordmark.png", root)),
    readFile(new URL("web/assets/crust-game-frame.png", root)),
    readFile(new URL("artwork/PROVENANCE.md", root), "utf8"),
  ]);

  const requiredIds = [
    "runtimeStatus",
    "screen",
    "canvas",
    "dropzone",
    "gameFiles",
    "gameFolder",
    "chooseFiles",
    "chooseFolder",
    "fileCount",
    "pairCount",
    "byteCount",
    "assetMessage",
    "bootLevel",
    "launch",
    "clearData",
    "importProgress",
    "progressBar",
    "progressLabel",
    "progressValue",
    "pause",
    "mute",
    "fullscreen",
    "simState",
    "currentLevel",
    "audioState",
    "cardState",
    "runtimeLog",
    "gameOverlay",
    "gameOverline",
    "gameTitle",
    "gameSubtitle",
    "gameMenu",
  ];
  for (const id of requiredIds) {
    assert.equal(
      html.match(new RegExp(`id="${id}"`, "g"))?.length,
      1,
      `expected exactly one #${id}`,
    );
  }

  const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  assert.deepEqual(wordmark.subarray(0, pngSignature.length), pngSignature);
  assert.deepEqual(frame.subarray(0, pngSignature.length), pngSignature);
  assert.ok(wordmark.length > 100_000, "wordmark should be the generated production asset");
  assert.ok(frame.length > 100_000, "frame should be the generated production asset");
  assert.match(provenance, /No Crash Bandicoot game data/);
  assert.match(provenance, /crust-wordmark/);
  assert.match(provenance, /crust-game-frame/);
});
