import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

import {
  composeCampaignReplay,
  composeCampaignReplayFromFile,
  isLocalArtifactPath,
  normalizeCampaignManifest,
  writeComposedReplay,
} from "./browser-campaign-replay.mjs";
import {
  parseArguments,
  run as runComposer,
} from "./compose-browser-campaign-replay.mjs";

function checkpoint(lid, draw, salt) {
  return {
    currentLid: lid,
    mountedLid: lid,
    retailDrawCount: draw,
    retailProcessDrawCount: draw + salt,
    retailRandomSeed: 0x1000_0000 + salt,
    retailRandomSeedB: 0x2000_0000 + salt,
    retailHardRestarts: 0,
    retailLoadStates: 0,
    retailDeathCameraFrames: 0,
    titleState: 15,
  };
}

function phase(id, fragment, entry, exit, extra = {}) {
  return { id, fragment, entry, exit, ...extra };
}

function fragment(entry, exit, segments, extra = {}) {
  const frames = segments.reduce((sum, segment) => sum + segment.frames, 0);
  const initialPad = extra.initialPad ?? pad(0);
  const finalPad =
    extra.finalPad ?? replayPadSnapshot(initialPad, segments);
  return {
    schema: 1,
    localDiagnosticOnly: true,
    canonicalCampaign: false,
    bootLid: entry.currentLid,
    unlockAll: false,
    level: entry.currentLid,
    initialDrawCount: entry.retailDrawCount,
    frames,
    surveyFrames: frames,
    context: "SessionGlobals",
    transition:
      entry.currentLid === exit.currentLid
        ? null
        : { frame: frames, lid: exit.currentLid },
    segments,
    settleFrames: extra.settleFrames ?? 0,
    expect: {
      currentLid: exit.currentLid,
      mountedLid: exit.mountedLid,
      minRetailExecutions: 1,
    },
    entryCheckpoint: structuredClone(entry),
    exitCheckpoint: structuredClone(exit),
    entryProgression:
      extra.entryProgression ?? progressionForCheckpoint(entry),
    exitProgression:
      extra.exitProgression ?? progressionForCheckpoint(exit),
    initialPad,
    finalPad,
  };
}

function progressionForCheckpoint(value) {
  return {
    gameState: value.currentLid << 8,
    titleState: value.titleState ?? 15,
    savedTitleState: value.titleState ?? 15,
    currentMapLevel: value.retailDrawCount & 0xff,
    levelCount: 1,
    levelsUnlocked: value.retailDrawCount & 0xff,
    islandCameraState: value.currentLid & 1,
  };
}

function progression(salt) {
  return {
    gameState: 0x300 + salt,
    titleState: 15,
    savedTitleState: 15,
    currentMapLevel: 1 + salt,
    levelCount: 1,
    levelsUnlocked: 1 + salt,
    islandCameraState: salt & 1,
  };
}

function pad(
  held,
  {
    tapped = 0,
    tappedPrevious = 0,
    heldPrevious = 0,
    heldPrevious2 = 0,
  } = {},
) {
  return {
    tapped,
    held,
    tappedPrevious,
    heldPrevious,
    heldPrevious2,
  };
}

function advancePadSnapshot(previous, held) {
  return {
    tapped: ((~previous.held & held) & 0xf9ff) >>> 0,
    held,
    tappedPrevious: previous.tapped,
    heldPrevious: previous.held,
    heldPrevious2: previous.heldPrevious,
  };
}

function replayPadSnapshot(initial, segments) {
  let snapshot = initial;
  for (const segment of segments) {
    for (let index = 0; index < Math.min(segment.frames, 3); index += 1) {
      snapshot = advancePadSnapshot(snapshot, segment.held);
    }
  }
  return snapshot;
}

function syntheticCampaign() {
  const beachEntry = checkpoint(0x09, 11, 1);
  const completionEntry = checkpoint(0x2d, 101, 2);
  const titleEntry = checkpoint(0x19, 121, 3);
  const jungleEntry = checkpoint(0x0c, 151, 4);
  const finalCompletion = checkpoint(0x2d, 301, 5);
  const manifest = {
    schema: 1,
    localDiagnosticOnly: true,
    canonicalCampaign: false,
    bootLid: 0x09,
    unlockAll: false,
    traceFromPhase: "map-to-jungle",
    phases: [
      phase(
        "n-sanity",
        "./n-sanity.json",
        beachEntry,
        completionEntry,
      ),
      phase(
        "jungle-rollers",
        "./jungle.json",
        jungleEntry,
        finalCompletion,
      ),
    ],
    titleMapHandoffs: [
      {
        kind: "title-map",
        after: "n-sanity",
        before: "jungle-rollers",
        phases: [
          phase(
            "n-sanity-complete",
            "./complete.json",
            completionEntry,
            titleEntry,
          ),
          phase(
            "map-to-jungle",
            "./map.json",
            titleEntry,
            jungleEntry,
            { settleFrames: 3 },
          ),
        ],
      },
    ],
  };
  let currentPad = pad(0);
  const capturedFragment = (entry, exit, segments, extra = {}) => {
    const document = fragment(entry, exit, segments, {
      ...extra,
      initialPad: currentPad,
    });
    currentPad = document.finalPad;
    if (entry.currentLid !== exit.currentLid) {
      currentPad = advancePadSnapshot(currentPad, currentPad.held);
    }
    return document;
  };
  const fragments = new Map([
    [
      "./n-sanity.json",
      capturedFragment(
        beachEntry,
        completionEntry,
        [
          { frames: 2, held: 0x0040 },
          { frames: 1, held: 0 },
        ],
      ),
    ],
    [
      "./complete.json",
      capturedFragment(
        completionEntry,
        titleEntry,
        [{ frames: 2, held: 0x0800 }],
      ),
    ],
    [
      "./map.json",
      capturedFragment(
        titleEntry,
        jungleEntry,
        [
          { frames: 3, held: 0x1000 },
          { frames: 1, held: 0x0040 },
        ],
        { settleFrames: 2 },
      ),
    ],
    [
      "./jungle.json",
      capturedFragment(
        jungleEntry,
        finalCompletion,
        [{ frames: 4, held: 0x0010 }],
      ),
    ],
  ]);
  return {
    manifest,
    fragments,
    checkpoints: {
      beachEntry,
      completionEntry,
      titleEntry,
      jungleEntry,
      finalCompletion,
    },
  };
}

test("composer accepts ordered native title capture metadata and exact mount pad history", async () => {
  const titleEntry = {
    ...checkpoint(0x19, 0, 1),
    titleState: 1,
  };
  const beachEntry = {
    ...checkpoint(0x09, 3, 2),
    titleState: 15,
  };
  const completionEntry = {
    ...checkpoint(0x2d, 5, 3),
    titleState: 15,
  };
  const titleProgression = progression(0);
  const beachProgression = progression(1);
  const completionProgression = progression(2);
  const titleInitialPad = pad(0);
  const titleFinalPad = pad(0x0040, { tapped: 0x0040 });
  const beachInitialPad = pad(0x0040, {
    tappedPrevious: 0x0040,
    heldPrevious: 0x0040,
  });
  const beachFinalPad = pad(0x0040, {
    tapped: 0x0040,
    heldPrevious: 0,
    heldPrevious2: 0x0040,
  });
  const manifest = {
    schema: 1,
    localDiagnosticOnly: true,
    canonicalCampaign: false,
    bootLid: 0x19,
    unlockAll: false,
    phases: [
      phase("publisher-title", "./publisher.json", titleEntry, beachEntry),
      phase("n-sanity", "./beach.json", beachEntry, completionEntry),
    ],
  };
  const fragments = new Map([
    [
      "./publisher.json",
      {
        ...fragment(
          titleEntry,
          beachEntry,
          [
            { frames: 2, held: 0, inputKind: "physical" },
            { frames: 1, held: 0x0040, inputKind: "physical" },
          ],
        ),
        entryCheckpoint: titleEntry,
        exitCheckpoint: beachEntry,
        entryProgression: titleProgression,
        exitProgression: beachProgression,
        initialPad: titleInitialPad,
        finalPad: titleFinalPad,
      },
    ],
    [
      "./beach.json",
      {
        ...fragment(
          beachEntry,
          completionEntry,
          [
            { frames: 1, held: 0, inputKind: "physical" },
            { frames: 1, held: 0x0040, inputKind: "physical" },
          ],
        ),
        entryCheckpoint: beachEntry,
        exitCheckpoint: completionEntry,
        entryProgression: beachProgression,
        exitProgression: completionProgression,
        initialPad: beachInitialPad,
        finalPad: beachFinalPad,
      },
    ],
  ]);

  const replay = await composeCampaignReplay(
    manifest,
    async (reference) => fragments.get(reference),
  );

  assert.deepEqual(replay.composition.phaseIds, [
    "publisher-title",
    "n-sanity",
  ]);
  assert.deepEqual(
    replay.segments.map(({ frames, held }) => ({ frames, held })),
    [
      { frames: 2, held: 0 },
      { frames: 1, held: 0x0040 },
      { frames: 1, held: 0 },
      { frames: 1, held: 0x0040 },
    ],
    "native capture run order must survive composition",
  );

  const wrongCheckpoint = structuredClone(fragments);
  wrongCheckpoint
    .get("./publisher.json")
    .exitCheckpoint.retailRandomSeedB += 1;
  await assert.rejects(
    composeCampaignReplay(
      manifest,
      async (reference) => wrongCheckpoint.get(reference),
    ),
    /exitCheckpoint does not match the exact manifest checkpoint.*retailRandomSeedB/s,
  );

  const wrongFinalPad = structuredClone(fragments);
  wrongFinalPad.get("./publisher.json").finalPad.heldPrevious = 0x0040;
  await assert.rejects(
    composeCampaignReplay(
      manifest,
      async (reference) => wrongFinalPad.get(reference),
    ),
    /finalPad does not match its ordered input segments.*heldPrevious/s,
  );

  const discontinuousPad = structuredClone(fragments);
  discontinuousPad.get("./beach.json").initialPad.heldPrevious2 = 0x0040;
  await assert.rejects(
    composeCampaignReplay(
      manifest,
      async (reference) => discontinuousPad.get(reference),
    ),
    /physical-pad history is discontinuous.*heldPrevious2/s,
  );

  const discontinuousProgression = structuredClone(fragments);
  discontinuousProgression.get("./beach.json").entryProgression = {
    ...discontinuousProgression.get("./beach.json").entryProgression,
    levelsUnlocked:
      discontinuousProgression.get("./beach.json").entryProgression
        .levelsUnlocked + 1,
  };
  await assert.rejects(
    composeCampaignReplay(
      manifest,
      async (reference) => discontinuousProgression.get(reference),
    ),
    /captured progression is discontinuous.*levelsUnlocked/s,
  );
});

test("composer inserts authored title-map fragments with guards and exact exits", async () => {
  const { manifest, fragments, checkpoints } = syntheticCampaign();
  const replay = await composeCampaignReplay(
    manifest,
    async (reference) => fragments.get(reference),
  );

  assert.deepEqual(replay.composition.phaseIds, [
    "n-sanity",
    "n-sanity-complete",
    "map-to-jungle",
    "jungle-rollers",
  ]);
  assert.deepEqual(replay.composition.insertedHandoffs, [
    {
      kind: "title-map",
      after: "n-sanity",
      before: "jungle-rollers",
      phaseIds: ["n-sanity-complete", "map-to-jungle"],
    },
  ]);
  assert.equal(replay.traceFromSegment, 4);
  assert.equal(replay.segments.length, 6);
  assert.deepEqual(replay.segments[0].while, {
    currentLid: 0x09,
    mountedLid: 0x09,
  });
  assert.deepEqual(replay.segments[2].while, {
    currentLid: 0x2d,
    mountedLid: 0x2d,
  });
  assert.deepEqual(replay.segments[3].while, {
    currentLid: 0x19,
    mountedLid: 0x19,
  });
  assert.deepEqual(replay.segments[5].while, {
    currentLid: 0x0c,
    mountedLid: 0x0c,
  });
  assert.deepEqual(
    Object.fromEntries(
      Object.entries(replay.segments[1].expect).filter(([key]) =>
        key.startsWith("retail") || key.endsWith("Lid") || key === "titleState",
      ),
    ),
    checkpoints.completionEntry,
  );
  assert.deepEqual(
    Object.fromEntries(
      Object.entries(replay.segments[4].expect).filter(([key]) =>
        key.startsWith("retail") || key.endsWith("Lid") || key === "titleState",
      ),
    ),
    checkpoints.jungleEntry,
  );
  assert.equal(
    replay.segments[4].expect.minRetailExecutions,
    undefined,
    "the map handoff stops at its exact destination checkpoint without consuming Jungle frame 1",
  );
  assert.equal(
    replay.composition.crossLidExitPolicy,
    "exact-checkpoint-at-destination-mount",
  );
  assert.equal(
    replay.segments[4].settleFrames,
    3,
    "manifest phase settle override replaces the fragment root settle budget",
  );
  assert.deepEqual(replay.expect, checkpoints.finalCompletion);
  assert.equal(replay.localDiagnosticOnly, true);
  assert.equal(replay.canonicalCampaign, false);
  assert.ok(
    replay.segments.every((segment) => segment.inputKind === "physical"),
  );
});

test("composer consumes destination frames only when the exact exit checkpoint requires them", async () => {
  const { manifest, fragments } = syntheticCampaign();
  const replay = await composeCampaignReplay(
    manifest,
    async (reference) => fragments.get(reference),
  );
  for (const segmentIndex of [1, 2, 4, 5]) {
    assert.equal(
      replay.segments[segmentIndex].expect.minRetailExecutions,
      undefined,
      `cross-LID segment ${segmentIndex + 1} must not consume the next phase's frame 1`,
    );
  }

  const idle = checkpoint(0x09, 11, 1);
  const idleManifest = {
    schema: 1,
    localDiagnosticOnly: true,
    canonicalCampaign: false,
    bootLid: 0x09,
    unlockAll: false,
    phases: [
      phase("same-mount-idle", "./idle.json", idle, idle),
    ],
  };
  const idleReplay = await composeCampaignReplay(
    idleManifest,
    async () => fragment(idle, idle, [{ frames: 1, held: 0 }]),
  );
  assert.equal(
    idleReplay.segments[0].expect.minRetailExecutions,
    1,
    "a phase that stays on the same mount retains its standalone execution proof",
  );
});

test("composer rejects exact checkpoint discontinuity before loading fragments", async () => {
  const { manifest, fragments } = syntheticCampaign();
  manifest.phases[1].entry = {
    ...manifest.phases[1].entry,
    retailRandomSeedB: 0xdead_beef,
  };
  await assert.rejects(
    composeCampaignReplay(
      manifest,
      async (reference) => fragments.get(reference),
    ),
    /discontinuous.*retailRandomSeedB/s,
  );
});

test("manifest requires complete exact checkpoints and an authored title phase", () => {
  const { manifest } = syntheticCampaign();
  delete manifest.phases[0].entry.retailProcessDrawCount;
  assert.throws(
    () => normalizeCampaignManifest(manifest),
    /missing exact continuity fields: retailProcessDrawCount/,
  );

  const missingTitleState = syntheticCampaign().manifest;
  delete missingTitleState.phases[0].entry.titleState;
  assert.throws(
    () => normalizeCampaignManifest(missingTitleState),
    /missing exact continuity fields: titleState/,
  );

  const withoutTitle = syntheticCampaign().manifest;
  withoutTitle.titleMapHandoffs[0].phases =
    withoutTitle.titleMapHandoffs[0].phases.filter(
      ({ id }) => id !== "map-to-jungle",
    );
  assert.throws(
    () => normalizeCampaignManifest(withoutTitle),
    /does not contain an authored Title.*0x19/,
  );

  const malformedHandoffs = syntheticCampaign().manifest;
  malformedHandoffs.titleMapHandoffs = {};
  assert.throws(
    () => normalizeCampaignManifest(malformedHandoffs),
    /titleMapHandoffs must be an array/,
  );
});

test("title-map handoffs can connect only adjacent ordered base phases", () => {
  const { manifest, checkpoints } = syntheticCampaign();
  manifest.phases.push(
    phase(
      "great-gate",
      "./great-gate.json",
      checkpoints.finalCompletion,
      checkpoint(0x2d, 401, 6),
    ),
  );
  manifest.titleMapHandoffs[0].before = "great-gate";
  assert.throws(
    () => normalizeCampaignManifest(manifest),
    /must connect adjacent ordered phases/,
  );
});

test("composer rejects non-local fragments and inconsistent export metadata", async () => {
  const localFlags = syntheticCampaign();
  localFlags.fragments.get("./n-sanity.json").localDiagnosticOnly = false;
  await assert.rejects(
    composeCampaignReplay(
      localFlags.manifest,
      async (reference) => localFlags.fragments.get(reference),
    ),
    /localDiagnosticOnly must equal true/,
  );

  const wrongDraw = syntheticCampaign();
  wrongDraw.fragments.get("./map.json").initialDrawCount += 1;
  await assert.rejects(
    composeCampaignReplay(
      wrongDraw.manifest,
      async (reference) => wrongDraw.fragments.get(reference),
    ),
    /initialDrawCount does not match/,
  );

  const wrongGuard = syntheticCampaign();
  wrongGuard.fragments.get("./jungle.json").segments[0].while = {
    currentLid: 0x09,
    mountedLid: 0x09,
  };
  await assert.rejects(
    composeCampaignReplay(
      wrongGuard.manifest,
      async (reference) => wrongGuard.fragments.get(reference),
    ),
    /LID guard that does not match/,
  );

  const incompleteCapture = syntheticCampaign();
  delete incompleteCapture.fragments.get("./complete.json").initialPad;
  await assert.rejects(
    composeCampaignReplay(
      incompleteCapture.manifest,
      async (reference) => incompleteCapture.fragments.get(reference),
    ),
    /missing exact capture metadata: initialPad/,
  );
});

test("a phase can explicitly opt a local 32-bit fragment into recorded stepping", async () => {
  const { manifest, fragments } = syntheticCampaign();
  manifest.phases = [manifest.phases[0]];
  manifest.titleMapHandoffs = [];
  delete manifest.traceFromPhase;
  manifest.phases[0].inputKind = "recorded";
  const recordedFragment = fragments.get("./n-sanity.json");
  recordedFragment.segments[0].held = 0x0010_0040;
  recordedFragment.finalPad = replayPadSnapshot(
    recordedFragment.initialPad,
    recordedFragment.segments,
  );
  const replay = await composeCampaignReplay(
    manifest,
    async (reference) => fragments.get(reference),
  );
  assert.equal(replay.segments[0].inputKind, "recorded");
  assert.equal(replay.segments[0].held, 0x0010_0040);

  delete manifest.phases[0].inputKind;
  await assert.rejects(
    composeCampaignReplay(
      manifest,
      async (reference) => fragments.get(reference),
    ),
    /0 through 65535/,
  );
});

test("composer rejects impossible live-pad directions unless explicitly recorded", async () => {
  const { manifest, fragments } = syntheticCampaign();
  manifest.phases = [manifest.phases[0]];
  manifest.titleMapHandoffs = [];
  delete manifest.traceFromPhase;
  fragments.get("./n-sanity.json").segments[0].held = 0x5040;
  await assert.rejects(
    composeCampaignReplay(
      manifest,
      async (reference) => fragments.get(reference),
    ),
    /segment 1\.held contains opposing physical directions/,
  );

  fragments.get("./n-sanity.json").segments[0].settleHeld = 0xa000;
  fragments.get("./n-sanity.json").segments[0].held = 0x0040;
  await assert.rejects(
    composeCampaignReplay(
      manifest,
      async (reference) => fragments.get(reference),
    ),
    /segment 1\.settleHeld contains opposing physical directions/,
  );

  manifest.phases[0].inputKind = "recorded";
  const recordedFragment = fragments.get("./n-sanity.json");
  recordedFragment.segments[0].held = 0x5040;
  recordedFragment.segments[0].settleHeld = 0xa000;
  recordedFragment.finalPad = replayPadSnapshot(
    recordedFragment.initialPad,
    recordedFragment.segments,
  );
  const replay = await composeCampaignReplay(
    manifest,
    async (reference) => fragments.get(reference),
  );
  assert.equal(replay.segments[0].inputKind, "recorded");
  assert.equal(replay.segments[0].held, 0x5040);
  assert.equal(replay.segments[0].settleHeld, 0xa000);
});

test("file composition resolves fragments beside an ignored local manifest", async (context) => {
  const root = await mkdtemp(join(tmpdir(), "crust-campaign-composer-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const { manifest, fragments } = syntheticCampaign();
  const manifestPath = join(root, "manifest.json");
  await writeFile(manifestPath, JSON.stringify(manifest));
  for (const [reference, document] of fragments) {
    await writeFile(join(root, reference), JSON.stringify(document));
  }

  const composed = await composeCampaignReplayFromFile(manifestPath);
  assert.equal(composed.replay.segments.length, 6);
  assert.equal(composed.fragmentPaths.length, 4);

  const outputPath = join(root, "campaign.replay.json");
  const result = await runComposer({
    manifest: manifestPath,
    output: outputPath,
    check: false,
    force: false,
  });
  assert.equal(result.phases, 4);
  assert.equal(result.handoffs, 1);
  assert.equal(result.segments, 6);
  assert.equal(result.output, outputPath);
  const written = JSON.parse(await readFile(outputPath, "utf8"));
  assert.deepEqual(written.expect, composed.replay.expect);

  await assert.rejects(
    writeComposedReplay(outputPath, composed.replay),
    /already exists/,
  );
  await writeComposedReplay(outputPath, composed.replay, { force: true });
  await assert.rejects(
    writeComposedReplay(manifestPath, composed.replay, {
      force: true,
      protectedPaths: [manifestPath],
    }),
    /must not overwrite an input file/,
  );
});

test("CLI arguments require an explicit local output unless checking", () => {
  assert.deepEqual(parseArguments(["--manifest", "./manifest.json", "--check"]), {
    manifest: resolve("./manifest.json"),
    output: undefined,
    check: true,
    force: false,
    help: false,
  });
  assert.throws(
    () => parseArguments(["--manifest", "./manifest.json"]),
    /--output is required/,
  );
  assert.throws(
    () =>
      parseArguments([
        "--manifest",
        "./manifest.json",
        "--output",
        "./out.json",
        "--check",
      ]),
    /cannot be combined/,
  );
});

test("repository-local outputs are confined to ignored artifact roots", () => {
  assert.equal(
    isLocalArtifactPath(
      resolve(new URL("../target/campaign.replay.json", import.meta.url).pathname),
    ),
    true,
  );
  assert.equal(
    isLocalArtifactPath(
      resolve(new URL("../docs/campaign.replay.json", import.meta.url).pathname),
    ),
    false,
  );
  assert.equal(isLocalArtifactPath(join(tmpdir(), "campaign.replay.json")), true);
});
