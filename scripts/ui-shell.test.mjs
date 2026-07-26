import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

async function readRustSources(directory) {
  const sources = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const url = new URL(entry.name + (entry.isDirectory() ? "/" : ""), directory);
    if (entry.isDirectory()) {
      sources.push(...(await readRustSources(url)));
    } else if (entry.isFile() && entry.name.endsWith(".rs")) {
      sources.push({ path: url.pathname, text: await readFile(url, "utf8") });
    }
  }
  return sources;
}

test("launcher keeps the intentionally simple full-game-first hierarchy", async () => {
  const html = await readFile(new URL("web/index.html", root), "utf8");

  const insert = html.indexOf('id="dropzone"');
  const eject = html.indexOf('id="clearData"');
  const launch = html.indexOf('id="launch"');
  const unlock = html.indexOf('id="unlockAll"');
  const level = html.indexOf('id="bootLevel"');

  assert.notEqual(insert, -1, "the local BIN/ISO action must remain present");
  assert.notEqual(eject, -1, "the local-data eject action must remain present");
  assert.notEqual(launch, -1, "the launch action must remain present");
  assert.notEqual(unlock, -1, "the all-levels option must remain present");
  assert.notEqual(level, -1, "the optional direct-level selector must remain present");
  assert.ok(insert < launch, "Insert BIN/ISO must precede Launch game");
  assert.ok(eject < launch, "the secondary eject action must stay out of launch step two");
  assert.ok(launch < unlock, "the all-levels option must stay below Launch game");
  assert.ok(unlock < level, "the optional level selector must stay below the all-levels option");
  assert.match(html, /Insert BIN \/ ISO/);
  assert.match(html, />\s*Launch game\s*</);
  assert.match(html, /All levels unlocked/);
  assert.match(html, /Includes 999 lives for this session/);
  assert.match(html, /resume and memory card stay untouched/);
  assert.match(html, /Leave on “full game” to begin at the opening/);
  assert.doesNotMatch(html, /Local-data bay|Runtime monitor|Launch Rust runtime/);
  assert.doesNotMatch(html, /standby-glyph|>◆</);
});

test("manual browser stepping stays behind an off-by-default Cargo feature", async () => {
  const [
    manifest,
    packageJson,
    productionBuild,
    harnessBuild,
    app,
    bootstrap,
  ] = await Promise.all([
    readFile(new URL("crates/web/Cargo.toml", root), "utf8"),
    readFile(new URL("package.json", root), "utf8"),
    readFile(new URL("scripts/build-web.sh", root), "utf8"),
    readFile(new URL("scripts/build-browser-harness.sh", root), "utf8"),
    readFile(new URL("crates/web/src/app.rs", root), "utf8"),
    readFile(new URL("web/bootstrap.js", root), "utf8"),
  ]);

  assert.match(manifest, /\[features\]\s+default = \[\]/);
  assert.match(
    manifest,
    /browser-test-harness = \["crust-sim\/browser-test-harness"\]/,
  );
  assert.match(packageJson, /"build:browser-harness"/);
  assert.match(packageJson, /"serve:browser-harness"/);
  assert.match(harnessBuild, /--features browser-test-harness/);
  assert.match(harnessBuild, /target\/browser-test-dist/);
  assert.match(
    harnessBuild,
    /CARGO_OUTPUT="\$\{CARGO_TARGET_DIR:-\$ROOT\/target\}"/,
    "the harness builder must package the artifact from Cargo's selected target directory",
  );
  assert.match(
    harnessBuild,
    /\$CARGO_OUTPUT\/wasm32-unknown-unknown\/release\/crust_web\.wasm/,
  );
  for (const build of [productionBuild, harnessBuild]) {
    assert.match(
      build,
      /CARGO_OUTPUT="\$\{CARGO_TARGET_DIR:-\$ROOT\/target\}"/,
      "each web builder must honor Cargo's selected target directory",
    );
    assert.match(
      build,
      /\$CARGO_OUTPUT\/wasm32-unknown-unknown\/release\/crust_web\.wasm/,
    );
    assert.doesNotMatch(
      build,
      /"\$ROOT\/target\/wasm32-unknown-unknown\/release\/crust_web\.wasm"/,
      "no web builder may silently package a stale default-target artifact",
    );
  }
  assert.doesNotMatch(
    harnessBuild,
    /["']\$ROOT\/dist["']/,
    "the harness builder must not replace the production distribution",
  );
  assert.match(
    app,
    /#\[cfg\(feature = "browser-test-harness"\)\]\s+install_browser_test_harness/,
  );
  assert.match(
    app,
    /#\[cfg\(not\(feature = "browser-test-harness"\)\)\]\s+start_animation_loop/,
  );
  assert.match(
    app,
    /#\[cfg\(feature = "browser-test-harness"\)\]\s+fn browser_test_live_objects_value/,
  );
  assert.match(
    app,
    /#\[cfg\(feature = "browser-test-harness"\)\]\s+fn step_browser_test_frame/,
  );
  assert.match(app, /BrowserTestPadInput::recorded/);
  assert.match(app, /"stepRecorded"/);
  assert.match(app, /app\.frame\(timestamp_ms\)/);
  assert.match(app, /load_level_pair\(Rc::clone\(app\), level\)/);
  assert.doesNotMatch(
    bootstrap,
    /__crustTest|browser-test-harness/,
    "the production bootstrap must not opt into or expose the harness",
  );
});

test("browser runtime cannot restore the retired synthetic triangle game", async () => {
  const sources = (
    await Promise.all([
      readRustSources(new URL("crates/sim/src/", root)),
      readRustSources(new URL("crates/web/src/", root)),
    ])
  ).flat();
  const forbidden = [
    ["synthetic GameFlow authority", /\bpub\s+struct\s+GameFlow\b/],
    ["synthetic PlayerState authority", /\bpub\s+struct\s+PlayerState\b/],
    ["host-authored level goal", /\bfn\s+handle_level_goal\s*\(/],
    ["diagnostic triangle vertex generator", /\bfn\s+scene_vertices\s*\(/],
    ["diagnostic scene submission", /\bfn\s+submit_diagnostic_scene\s*\(/],
    ["diagnostic VisualState player coordinate", /\bplayer_x\s*:\s*f32\b/],
  ];

  for (const [description, pattern] of forbidden) {
    const offenders = sources
      .filter((source) => pattern.test(source.text))
      .map((source) => source.path);
    assert.deepEqual(
      offenders,
      [],
      `${description} must not return to authored runtime source`,
    );
  }

  const [app, webgl] = await Promise.all([
    readFile(new URL("crates/web/src/app.rs", root), "utf8"),
    readFile(new URL("crates/web/src/webgl.rs", root), "utf8"),
  ]);
  assert.match(app, /RetailRuntime::new_for_level\s*\(/);
  assert.match(app, /authored_scene_runtime_active\s*\(/);
  assert.match(webgl, /update_retail_scene\s*\(/);
  assert.match(webgl, /state\.show_retail_scene/);
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
    "unlockAll",
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
