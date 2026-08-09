import assert from "node:assert/strict";
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

import { isLocalArtifactPath } from "./browser-campaign-replay.mjs";
import {
  loadExportedCampaignFragments,
  parseArguments,
  run,
} from "./discover-browser-campaign-replay.mjs";

function checkpoint(lid, serial) {
  return {
    currentLid: lid,
    mountedLid: lid,
    retailDrawCount: serial * 10,
    retailProcessDrawCount: serial * 10 + 1,
    retailRandomSeed: 0x1000_0000 + serial,
    retailRandomSeedB: 0x2000_0000 + serial,
    retailHardRestarts: 0,
    retailLoadStates: 0,
    retailDeathCameraFrames: 0,
    titleState: 15,
  };
}

function progression(serial) {
  return {
    gameState: 0x300 + serial,
    titleState: 15,
    savedTitleState: 15,
    currentMapLevel: serial,
    levelCount: 1,
    levelsUnlocked: serial + 1,
    islandCameraState: serial & 1,
    gemCount: serial & 0x1f,
    keyCount: serial & 1,
    itemPool1: 0x1000_0000 + serial,
    itemPool2: 0x2000_0000 + serial,
  };
}

function stablePad(held = 0) {
  return {
    tapped: 0,
    held,
    tappedPrevious: 0,
    heldPrevious: held,
    heldPrevious2: held,
  };
}

function capture({
  entry,
  exit,
  entryProgression,
  exitProgression,
  initialPad = stablePad(),
  finalPad = stablePad(),
  frames = 1,
  inputProfile,
}) {
  return {
    schema: 1,
    localDiagnosticOnly: true,
    canonicalCampaign: false,
    inputProfile,
    frames,
    entryCheckpoint: structuredClone(entry),
    exitCheckpoint: structuredClone(exit),
    entryProgression: structuredClone(entryProgression),
    exitProgression: structuredClone(exitProgression),
    initialPad: structuredClone(initialPad),
    finalPad: structuredClone(finalPad),
  };
}

async function fixtureDirectory(context, name) {
  const root = await mkdtemp(join(tmpdir(), `crust-${name}-`));
  const fragments = join(root, "fragments");
  await mkdir(fragments);
  context.after(() => rm(root, { recursive: true, force: true }));
  return { root, fragments };
}

async function writeCapture(directory, name, document) {
  const path = join(directory, name);
  await writeFile(path, `${JSON.stringify(document, null, 2)}\n`);
  return path;
}

test("discovery CLI orders unordered captures and supports check and output modes", async (context) => {
  const { root, fragments } = await fixtureDirectory(context, "campaign-order");
  const opening = checkpoint(0x19, 0);
  const beach = checkpoint(0x09, 1);
  const completion = checkpoint(0x2d, 2);
  const openingProgression = progression(0);
  const beachProgression = progression(1);
  const completionProgression = progression(2);
  const openingName =
    "lid-19-draw-00000000-publisher-title-to-09.json";
  const beachName =
    "lid-09-draw-00000010-n-sanity-completion-route-to-2d.json";

  // Write the destination phase first and give it the lexically earlier LID.
  await writeCapture(
    fragments,
    beachName,
    capture({
      entry: beach,
      exit: completion,
      entryProgression: beachProgression,
      exitProgression: completionProgression,
      frames: 3,
      inputProfile: "n-sanity-completion-route",
    }),
  );
  await writeCapture(
    fragments,
    openingName,
    capture({
      entry: opening,
      exit: beach,
      entryProgression: openingProgression,
      exitProgression: beachProgression,
      frames: 2,
      inputProfile: "publisher-title",
    }),
  );

  const output = join(root, "manifest.json");
  const checked = await run({
    fragments,
    output: undefined,
    traceInputProfile: "n-sanity-completion-route",
    check: true,
    force: false,
  });
  assert.deepEqual(checked, {
    fragments: 2,
    phases: 2,
    frames: 5,
    bootLid: 0x19,
    finalLid: 0x2d,
    checked: true,
  });
  await assert.rejects(access(output));

  const written = await run({
    fragments,
    output,
    traceInputProfile: "n-sanity-completion-route",
    check: false,
    force: false,
  });
  assert.equal(written.output, output);
  const manifest = JSON.parse(await readFile(output, "utf8"));
  assert.deepEqual(
    manifest.phases.map(({ fragment }) => fragment),
    [
      join("fragments", openingName),
      join("fragments", beachName),
    ],
  );
  assert.equal(manifest.traceFromPhase, manifest.phases[1].id);
  assert.ok(
    manifest.phases.every(
      (phase) =>
        phase.inputKind === undefined
        && phase.settleFrames === undefined,
    ),
    "discovery must not add or alter controller input",
  );

  await assert.rejects(
    run({
      fragments,
      output,
      traceInputProfile: undefined,
      check: false,
      force: false,
    }),
    /already exists/,
  );
  const replaced = await run({
    fragments,
    output,
    traceInputProfile: undefined,
    check: false,
    force: true,
  });
  assert.equal(replaced.output, output);
});

test("discovery CLI fails closed for ambiguous exact paths", async (context) => {
  const { fragments } = await fixtureDirectory(context, "campaign-ambiguous");
  const idle = checkpoint(0x19, 0);
  const idleProgression = progression(0);
  for (const [name, profile] of [
    ["lid-19-draw-00000000-idle-a-to-19.json", "idle-a"],
    ["lid-19-draw-00000000-idle-b-to-19.json", "idle-b"],
  ]) {
    await writeCapture(
      fragments,
      name,
      capture({
        entry: idle,
        exit: idle,
        entryProgression: idleProgression,
        exitProgression: idleProgression,
        inputProfile: profile,
      }),
    );
  }

  await assert.rejects(
    run({
      fragments,
      output: undefined,
      traceInputProfile: undefined,
      check: true,
      force: false,
    }),
    /fragment graph is ambiguous/,
  );
});

test("discovery CLI rejects a disconnected one-word checkpoint seam", async (context) => {
  const { fragments } = await fixtureDirectory(context, "campaign-disconnected");
  const opening = checkpoint(0x19, 0);
  const beach = checkpoint(0x09, 1);
  const mismatchedBeach = {
    ...beach,
    retailRandomSeedB: beach.retailRandomSeedB + 1,
  };
  const completion = checkpoint(0x2d, 2);
  const openingProgression = progression(0);
  const beachProgression = progression(1);
  const completionProgression = progression(2);
  const heldPad = stablePad(0x0010);

  await writeCapture(
    fragments,
    "lid-19-draw-00000000-opening-to-09.json",
    capture({
      entry: opening,
      exit: beach,
      entryProgression: openingProgression,
      exitProgression: beachProgression,
      finalPad: heldPad,
      frames: 2,
      inputProfile: "opening",
    }),
  );
  await writeCapture(
    fragments,
    "lid-09-draw-00000010-beach-to-2d.json",
    capture({
      entry: mismatchedBeach,
      exit: completion,
      entryProgression: beachProgression,
      exitProgression: completionProgression,
      initialPad: heldPad,
      finalPad: heldPad,
      frames: 1,
      inputProfile: "beach",
    }),
  );

  await assert.rejects(
    run({
      fragments,
      output: undefined,
      traceInputProfile: undefined,
      check: true,
      force: false,
    }),
    /fragment graph is disconnected.*1 of 2 fragments/s,
  );
});

test("discovery CLI confines repository paths to ignored artifact roots", async (context) => {
  assert.equal(
    isLocalArtifactPath(resolve("target/local-campaign/fragments")),
    true,
  );
  assert.equal(
    isLocalArtifactPath(resolve("docs/local-campaign-fragments")),
    false,
  );
  await assert.rejects(
    loadExportedCampaignFragments(resolve("docs")),
    /fragment directory must be outside the repository or under an ignored/,
  );

  const { root, fragments } = await fixtureDirectory(context, "campaign-paths");
  const idle = checkpoint(0x19, 0);
  const idleProgression = progression(0);
  const fragmentPath = await writeCapture(
    fragments,
    "lid-19-draw-00000000-idle-to-19.json",
    capture({
      entry: idle,
      exit: idle,
      entryProgression: idleProgression,
      exitProgression: idleProgression,
      inputProfile: "idle",
    }),
  );
  await assert.rejects(
    run({
      fragments,
      output: resolve("docs/discovered-campaign.json"),
      traceInputProfile: undefined,
      check: false,
      force: false,
    }),
    /manifest output must be outside the repository or under an ignored/,
  );
  await assert.rejects(
    run({
      fragments,
      output: fragmentPath,
      traceInputProfile: undefined,
      check: false,
      force: true,
    }),
    /manifest output must not overwrite an input file/,
  );

  const externalOutput = join(root, "manifest.json");
  const result = await run({
    fragments,
    output: externalOutput,
    traceInputProfile: undefined,
    check: false,
    force: false,
  });
  assert.equal(result.output, externalOutput);
});

test("discovery CLI arguments require a fragment directory and explicit output", () => {
  assert.deepEqual(
    parseArguments([
      "--fragments",
      "./target/local-campaign/fragments",
      "--trace-input-profile",
      "jungle-rollers-completion-route",
      "--check",
    ]),
    {
      fragments: resolve("./target/local-campaign/fragments"),
      output: undefined,
      traceInputProfile: "jungle-rollers-completion-route",
      check: true,
      force: false,
      help: false,
    },
  );
  assert.throws(() => parseArguments(["--check"]), /--fragments is required/);
  assert.throws(
    () => parseArguments(["--fragments", "./target/local-campaign/fragments"]),
    /--output is required/,
  );
  assert.throws(
    () =>
      parseArguments([
        "--fragments",
        "./target/local-campaign/fragments",
        "--output",
        "./target/manifest.json",
        "--check",
      ]),
    /cannot be combined/,
  );
});
