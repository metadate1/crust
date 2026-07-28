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
  };
}

function phase(id, fragment, entry, exit, extra = {}) {
  return { id, fragment, entry, exit, ...extra };
}

function fragment(entry, exit, segments, extra = {}) {
  const frames = segments.reduce((sum, segment) => sum + segment.frames, 0);
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
  };
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
  const fragments = new Map([
    [
      "./n-sanity.json",
      fragment(
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
      fragment(
        completionEntry,
        titleEntry,
        [{ frames: 2, held: 0x0800 }],
      ),
    ],
    [
      "./map.json",
      fragment(
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
      fragment(
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
        key.startsWith("retail") || key.endsWith("Lid"),
      ),
    ),
    checkpoints.completionEntry,
  );
  assert.deepEqual(
    Object.fromEntries(
      Object.entries(replay.segments[4].expect).filter(([key]) =>
        key.startsWith("retail") || key.endsWith("Lid"),
      ),
    ),
    checkpoints.jungleEntry,
  );
  assert.equal(replay.segments[4].expect.minRetailExecutions, 1);
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
});

test("a phase can explicitly opt a local 32-bit fragment into recorded stepping", async () => {
  const { manifest, fragments } = syntheticCampaign();
  manifest.phases[0].inputKind = "recorded";
  fragments.get("./n-sanity.json").segments[0].held = 0x0010_0040;
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
