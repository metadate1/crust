import assert from "node:assert/strict";
import test from "node:test";

import {
  applyTerminalProgressionRequirements,
  allLevelsFailures,
  allLevelsStorageFailures,
  appendedRuntimeLogLines,
  cardRoundTripStorageFailures,
  destinationMountReady,
  directBonusReturnAuditFailures,
  expectationFailures,
  liveObjectExpectationFailures,
  nextReplayBatchFrameCount,
  showcaseWindowBatchFrameCount,
  normalizeReplay,
  parseArguments,
  parseVideoWindow,
  presentationFailures,
  parseRetailPbakEvidence,
  parseStorageSeedJson,
  resumeRoundTripStorageFailures,
  replayLidConditionKnown,
  replayLidConditionMatches,
  replayStepMethod,
  retailGameplayReadyAfterMount,
  retailPbakAuditFailures,
  retailPbakAuditTitleReady,
  retailExecutionObserved,
  snapshotFailures,
  summarizeReplayHostCallbacks,
  syntheticCookedIsoImportFailures,
  validateReplayBatchExecution,
} from "./browser-harness-smoke.mjs";
import { expectedSyntheticCookedIsoBlobRanges } from "./synthetic-retail-iso.mjs";

const STORAGE_PAYLOAD = Buffer.alloc(128).toString("base64");

function pbakLogEntry(line, stepCount, overrides = {}) {
  return {
    line,
    stepCount,
    currentLid: 0x0c,
    mountedLid: 0x0c,
    retailFrame: stepCount,
    retailDrawCount: stepCount,
    retailProcessDrawCount: stepCount,
    retailHardRestarts: 1,
    retailLoadStates: 0,
    retailDeathCameraFrames: 0,
    retailExecutions: stepCount * 3,
    retailExecutionErrors: 0,
    retailFaultedObjects: 0,
    retailZoneEventFailures: 0,
    retailRandomSeed: 0x1234,
    retailRandomSeedB: 0x5678,
    ...overrides,
  };
}

test("bounded runtime-log history retains only genuinely appended lines", () => {
  assert.deepEqual(appendedRuntimeLogLines("", "\n> first\n> second"), [
    "> first",
    "> second",
  ]);
  const previous = Array.from({ length: 81 }, (_, index) => `> line ${index}`);
  const current = [...previous.slice(1), "> new line"];
  assert.deepEqual(
    appendedRuntimeLogLines(previous.join("\n"), current.join("\n")),
    ["> new line"],
  );
  assert.deepEqual(appendedRuntimeLogLines(current.join("\n"), current.join("\n")), []);
  assert.throws(() => appendedRuntimeLogLines(null, ""), /must be strings/);
});

test("retail PBAK log evidence requires one exact arm/start/finish sequence", () => {
  const evidence = parseRetailPbakEvidence([
    pbakLogEntry(
      "> Armed retail PBAK pb0cB (Standard, 217 recorded frames); input remains locked until Crash is live.",
      40,
    ),
    pbakLogEntry(
      "> Retail PBAK pb0cB physical open is waiting for the in-flight CD page transfer.",
      40,
    ),
    pbakLogEntry(
      "> Started retail PBAK pb0cB; created caption controller RuntimeObjectHandle and restored its checked camera/player snapshot and gameplay RNG.",
      41,
      { retailHardRestarts: 2 },
    ),
    pbakLogEntry(
      "> Retail PBAK input ended (Finished); caption RuntimeObjectHandle received event 0xE00 (acknowledged: true) and retained the authored return lock.",
      258,
      { retailHardRestarts: 3, retailLoadStates: 1 },
    ),
    pbakLogEntry(
      "> Retail LEVEL_END resolved 25 to 25 (bonus return: false).",
      270,
      { currentLid: 0x19, mountedLid: 0x0c },
    ),
  ]);
  assert.equal(evidence.length, 1);
  assert.deepEqual(
    {
      eid: evidence[0].eid,
      layout: evidence[0].layout,
      recordedFrames: evidence[0].recordedFrames,
      finishReason: evidence[0].finishReason,
      wallFrames: evidence[0].wallFrames,
      hardRestartsDuringPlayback: evidence[0].hardRestartsDuringPlayback,
      loadStatesDuringPlayback: evidence[0].loadStatesDuringPlayback,
      pagerWaits: evidence[0].pagerWaits.length,
      targetLid: evidence[0].transition.targetLid,
    },
    {
      eid: "pb0cB",
      layout: "Standard",
      recordedFrames: 217,
      finishReason: "Finished",
      wallFrames: 218,
      hardRestartsDuringPlayback: 1,
      loadStatesDuringPlayback: 1,
      pagerWaits: 1,
      targetLid: 25,
    },
  );

  assert.throws(
    () => parseRetailPbakEvidence([
      pbakLogEntry("> Started retail PBAK pb0cB; snapshot restored.", 1),
    ]),
    /without being armed/,
  );
  assert.throws(
    () => parseRetailPbakEvidence([
      pbakLogEntry(
        "> Armed retail PBAK pb0cB (Standard, 217 recorded frames); input remains locked until Crash is live.",
        1,
      ),
    ]),
    /armed without finishing/,
  );
  assert.throws(
    () => parseRetailPbakEvidence([
      pbakLogEntry(
        "> Armed retail PBAK pb0cB (Standard, 217 recorded frames); input remains locked until Crash is live.",
        1,
      ),
      pbakLogEntry("> Started retail PBAK pb0eB; snapshot restored.", 2),
    ]),
    /while pb0cB was armed/,
  );
  assert.deepEqual(
    parseRetailPbakEvidence(
      [
        pbakLogEntry(
          "> Armed retail PBAK pb0cB (SpawnWords304, 1348 recorded frames); input remains locked until Crash is live.",
          1,
        ),
        pbakLogEntry("> Started retail PBAK pb0cB; snapshot restored.", 2),
      ],
      { allowIncomplete: true },
    ),
    [],
  );
  assert.throws(
    () => parseRetailPbakEvidence([
      pbakLogEntry(
        "> Armed retail PBAK pb0cB (SpawnWords304, 1348 recorded frames); input remains locked until Crash is live.",
        1,
      ),
      pbakLogEntry("> Started retail PBAK pb0cB; snapshot restored.", 2),
      pbakLogEntry(
        "> Retail PBAK input ended (Released); no caption controller retained the authored return lock.",
        3,
      ),
    ]),
    /exact successful caption acknowledgement/,
  );
  const completeThenRepeat = [
    pbakLogEntry(
      "> Armed retail PBAK pb0cB (SpawnWords304, 1348 recorded frames); input remains locked until Crash is live.",
      1,
    ),
    pbakLogEntry("> Started retail PBAK pb0cB; snapshot restored.", 2),
    pbakLogEntry(
      "> Retail PBAK input ended (Finished); caption RuntimeObjectHandle received event 0xE00 (acknowledged: true) and retained the authored return lock.",
      1_349,
    ),
    pbakLogEntry(
      "> Retail LEVEL_END resolved 12 to 0x19 (bonus return: false).",
      1_350,
    ),
    pbakLogEntry(
      "> Armed retail PBAK pb0cB (SpawnWords304, 1348 recorded frames); input remains locked until Crash is live.",
      2_000,
    ),
  ];
  assert.equal(
    parseRetailPbakEvidence(completeThenRepeat, {
      allowTrailingRepeat: true,
    }).length,
    1,
  );
  assert.throws(
    () => parseRetailPbakEvidence(completeThenRepeat),
    /armed without finishing/,
  );
  assert.throws(
    () => parseRetailPbakEvidence([
      ...completeThenRepeat,
      pbakLogEntry("> Started retail PBAK pb0cB; snapshot restored.", 2_001),
    ], {
      allowTrailingRepeat: true,
    }),
    /started without finishing/,
  );
  assert.throws(
    () => parseRetailPbakEvidence(completeThenRepeat.slice(-1), {
      allowTrailingRepeat: true,
    }),
    /armed without finishing/,
  );
  assert.throws(
    () => parseRetailPbakEvidence([], { allowIncomplete: "yes" }),
    /must be a boolean/,
  );
  assert.throws(
    () => parseRetailPbakEvidence([], { allowTrailingRepeat: "yes" }),
    /must be a boolean/,
  );
});

test("retail PBAK audit requires the exact nine-recording census and Title returns", () => {
  const profiles = [
    ["pb0aB", "SpawnWords304", 872],
    ["pb0cB", "SpawnWords304", 1_348],
    ["pb0eB", "SpawnWords304", 990],
    ["pb0fB", "SpawnWords511", 934],
    ["pb0iB", "SpawnWords304", 1_240],
    ["pb0sB", "SpawnWords304", 998],
    ["pb0tB", "SpawnWords304", 1_804],
    ["pb0wB", "SpawnWords304", 1_878],
    ["pb0FB", "SpawnWords304", 902],
  ];
  const clean = profiles.map(([eid, layout, recordedFrames]) => ({
    eid,
    layout,
    recordedFrames,
    finishReason: "Finished",
    finished: {
      retailExecutionErrors: 0,
      retailFaultedObjects: 0,
      retailZoneEventFailures: 0,
    },
    transition: { targetLid: 0x19, bonusReturn: false },
  }));
  assert.deepEqual(retailPbakAuditFailures(clean), []);
  const naturalEids = [
    "pb0aB",
    "pb0cB",
    "pb0eB",
    "pb0iB",
    "pb0tB",
    "pb0wB",
    "pb0FB",
  ];
  assert.deepEqual(
    retailPbakAuditFailures(
      clean.filter((run) => naturalEids.includes(run.eid)),
      { expectedEids: naturalEids },
    ),
    [],
  );
  assert.match(
    retailPbakAuditFailures(clean.slice(1)).join("\n"),
    /did not complete pb0aB/,
  );
  assert.match(
    retailPbakAuditFailures([
      ...clean,
      { ...clean[0], eid: "wrong" },
    ]).join("\n"),
    /unknown EID/,
  );
  assert.match(
    retailPbakAuditFailures([
      { ...clean[0], recordedFrames: 871, transition: undefined },
    ]).join("\n"),
    /reports 871 frames.*no observed LEVEL_END/s,
  );
  assert.throws(
    () => retailPbakAuditFailures([], { requireAll: "yes" }),
    /must be a boolean/,
  );
});

test("retail PBAK audit waits for a fully mounted running Title pair", () => {
  const ready = {
    runtimeState: "running",
    runtimeStatus: "Rust runtime active",
    harness: { lastRequestedLid: null },
    debug: {
      currentLid: 0x19,
      mountedLid: 0x19,
      mountedPages: 17,
      mountedEntries: 42,
    },
  };
  assert.equal(retailPbakAuditTitleReady(ready), true);
  for (const notReady of [
    { runtimeState: "loading" },
    { runtimeStatus: "Reading local NSD/NSF pair" },
    { harness: { lastRequestedLid: 0x19 } },
    { debug: { currentLid: 0x0f } },
    { debug: { mountedLid: 0x0f } },
    { debug: { mountedPages: 0 } },
    { debug: { mountedEntries: 0 } },
  ]) {
    assert.equal(
      retailPbakAuditTitleReady({
        ...ready,
        ...notReady,
        harness: { ...ready.harness, ...notReady.harness },
        debug: { ...ready.debug, ...notReady.debug },
      }),
      false,
    );
  }
});

function resumeStorageEnvelope(overrides = {}) {
  return {
    schema: "c1-browser-resume",
    version: 1,
    payload: STORAGE_PAYLOAD,
    updatedAt: 7,
    ...overrides,
  };
}

function cardStorageEnvelope(overrides = {}) {
  const slots = Array.from({ length: 15 }, () => null);
  slots[0] = { payload: STORAGE_PAYLOAD, updatedAt: 5 };
  return {
    schema: "c1-virtual-memory-card",
    version: 1,
    slots,
    updatedAt: 9,
    ...overrides,
  };
}

test("destination mount acknowledgement waits for the requested stream pair", () => {
  assert.equal(
    destinationMountReady(
      {
        runtimeState: "running",
        debug: { mountedLid: 0x19 },
      },
      0x09,
    ),
    false,
  );
  assert.equal(
    destinationMountReady(
      {
        runtimeState: "loading",
        debug: { mountedLid: 0x09 },
      },
      0x09,
    ),
    false,
  );
  assert.equal(
    destinationMountReady(
      {
        runtimeState: "running",
        debug: { mountedLid: 0x09 },
      },
      0x09,
    ),
    true,
  );
  const previousLog = "> Mounted destination 0x09: old mount.\n";
  assert.equal(
    destinationMountReady(
      {
        runtimeState: "running",
        runtimeLog: previousLog,
        debug: { mountedLid: 0x19 },
      },
      0x09,
      previousLog,
    ),
    false,
  );
  assert.equal(
    destinationMountReady(
      {
        runtimeState: "running",
        runtimeLog:
          `${previousLog}> Mounted destination 0x2D: validated replacement.\n`,
        debug: { mountedLid: 0x19 },
      },
      0x2d,
      previousLog,
    ),
    true,
  );
  const retainedTail = previousLog.slice(-18);
  assert.equal(
    destinationMountReady(
      {
        runtimeState: "running",
        runtimeLog:
          `${retainedTail}> Mounted destination 0x2D: bounded-log replacement.\n`,
        debug: { mountedLid: 0x19 },
      },
      0x2d,
      previousLog,
    ),
    true,
  );
  assert.equal(
    destinationMountReady(
      {
        runtimeState: "running",
        runtimeLog: "> Mounted destination 0x2D: stale unrelated visit.\n",
        debug: { mountedLid: 0x19 },
      },
      0x2d,
      previousLog,
    ),
    false,
  );
  assert.equal(
    replayLidConditionMatches(
      { currentLid: 0x2d, mountedLid: 0x2d },
      0x2d,
      0x2d,
    ),
    true,
  );
  assert.equal(
    replayLidConditionMatches(
      { currentLid: 0x2d, mountedLid: 0x2d },
      0x19,
      0x19,
    ),
    false,
  );
});

test("replay LID guards wait for their first observable runtime frame", () => {
  assert.equal(
    replayLidConditionKnown(
      { currentLid: 0x22, mountedLid: 0x22 },
      0x22,
      0x22,
    ),
    true,
  );
  assert.equal(
    replayLidConditionKnown(
      { currentLid: 0x22, mountedLid: 0x22 },
      undefined,
      undefined,
    ),
    false,
  );
  assert.equal(
    replayLidConditionKnown(
      { currentLid: 0x22 },
      0x22,
      undefined,
    ),
    true,
  );
});

test("replay batches cap constant-held runs and can isolate the launch frame", () => {
  assert.equal(nextReplayBatchFrameCount(1), 1);
  assert.equal(nextReplayBatchFrameCount(127), 127);
  assert.equal(nextReplayBatchFrameCount(128), 128);
  assert.equal(nextReplayBatchFrameCount(129), 128);
  assert.equal(
    nextReplayBatchFrameCount(10_000, { isolateFirstFrame: true }),
    1,
  );
  assert.throws(() => nextReplayBatchFrameCount(0), /positive safe integer/);
  assert.throws(
    () =>
      nextReplayBatchFrameCount(1, {
        isolateFirstFrame: "yes",
      }),
    /must be a boolean/,
  );
});

test("showcase batches stay below the CDP timeout and land on window edges", () => {
  assert.equal(showcaseWindowBatchFrameCount(0, 128, [100]), 32);
  assert.equal(showcaseWindowBatchFrameCount(80, 128, [100]), 20);
  assert.equal(showcaseWindowBatchFrameCount(100, 128, [100], {
    needsCapture: true,
  }), 1);
  assert.equal(showcaseWindowBatchFrameCount(120, 17, [100]), 17);
  assert.throws(
    () => showcaseWindowBatchFrameCount(-1, 128, [100]),
    /nonnegative safe integer/,
  );
});

test("replay host callbacks consume only cooperative steps and bound pager waits", () => {
  assert.deepEqual(
    summarizeReplayHostCallbacks(
      [false, false, true, false, true],
      2,
      { zeroStepLimit: 2 },
    ),
    {
      executed: 2,
      hostCallbacks: 5,
      consecutiveZeroSteps: 0,
      maximumConsecutiveZeroSteps: 2,
    },
  );
  assert.deepEqual(
    summarizeReplayHostCallbacks([false, false], 1, { zeroStepLimit: 2 }),
    {
      executed: 0,
      hostCallbacks: 2,
      consecutiveZeroSteps: 2,
      maximumConsecutiveZeroSteps: 2,
    },
  );
  assert.throws(
    () => summarizeReplayHostCallbacks(
      [false, false, false],
      1,
      { zeroStepLimit: 2 },
    ),
    /exceeded 2 consecutive zero-step host callbacks/,
  );
  assert.throws(
    () => summarizeReplayHostCallbacks([true, true], 1),
    /exceeded the requested steps/,
  );
  assert.throws(
    () => summarizeReplayHostCallbacks([0], 1),
    /results must be booleans/,
  );
});

test("a transition-only callback mounts without consuming replay input", () => {
  assert.equal(validateReplayBatchExecution(1), 1);
  assert.equal(
    validateReplayBatchExecution(0, { mountedDestination: true }),
    0,
  );
  assert.throws(
    () => validateReplayBatchExecution(0),
    /did not execute a cooperative simulation step/,
  );
  assert.throws(
    () => validateReplayBatchExecution(-1),
    /nonnegative safe integer/,
  );
});

test("gameplay readiness requires a post-mount execution and live player", () => {
  const ready = {
    runtimeState: "running",
    debug: {
      currentLid: 0x11,
      mountedLid: 0x11,
      retailExecutions: 8,
      browserTestObjects: [{ player: true, faulted: false }],
    },
  };
  assert.equal(retailGameplayReadyAfterMount(ready, 0x11, 7), true);
  for (const notReady of [
    { runtimeState: "idle" },
    { debug: { currentLid: 0x19 } },
    { debug: { mountedLid: 0x19 } },
    { debug: { retailExecutions: 7 } },
    { debug: { browserTestObjects: [] } },
    { debug: { browserTestObjects: [{ player: true, faulted: true }] } },
  ]) {
    assert.equal(
      retailGameplayReadyAfterMount(
        {
          ...ready,
          ...notReady,
          debug: { ...ready.debug, ...notReady.debug },
        },
        0x11,
        7,
      ),
      false,
    );
  }
  assert.throws(
    () => retailGameplayReadyAfterMount(ready, -1, 7),
    /expectedLid/,
  );
  assert.throws(
    () => retailGameplayReadyAfterMount(ready, 0x11, -1),
    /mountExecutions/,
  );
});

test("direct-bonus browser audit requires the classified LoadState and mounted Main Menu", () => {
  const clean = {
    runtimeState: "running",
    runtimeLog: [
      "> Completed a directly selected bonus without a parent snapshot; returning to the Main Menu.",
      "> Mounted destination 0x19: Title pair.",
    ].join("\n"),
    harness: {
      lastError: null,
      directBonusStateBoundary: 32,
    },
    consoleErrors: [],
    debug: {
      currentLid: 0x19,
      mountedLid: 0x19,
      titleState: 5,
      retailLoadStates: 1,
      glError: 0,
      retailFaultedObjects: 0,
      retailExecutionErrors: 0,
      retailZoneEventFailures: 0,
      retailRuntimeError: null,
      retailRuntimeWarning: null,
    },
  };
  assert.deepEqual(directBonusReturnAuditFailures(clean), []);

  const failures = directBonusReturnAuditFailures({
    ...clean,
    runtimeLog: "> Mounted destination 0x24: Tawna Bonus 1 pair.",
    debug: {
      ...clean.debug,
      currentLid: 0x24,
      mountedLid: 0x24,
      titleState: 0,
      retailLoadStates: 0,
    },
  }).join("\n");
  assert.match(failures, /currentLid/);
  assert.match(failures, /mountedLid/);
  assert.match(failures, /titleState/);
  assert.match(failures, /retailLoadStates/);
  assert.match(failures, /completion classification/);
  assert.match(failures, /Title destination mount/);
});

test("campaign execution evidence survives a destination mount at frame zero", () => {
  assert.equal(retailExecutionObserved(false, undefined), false);
  assert.equal(
    retailExecutionObserved(false, {
      debug: { retailExecutions: 0 },
    }),
    false,
  );
  assert.equal(
    retailExecutionObserved(false, {
      debug: { retailExecutions: 37 },
    }),
    true,
  );
  assert.equal(
    retailExecutionObserved(true, {
      debug: { retailExecutions: 0 },
    }),
    true,
  );
  assert.throws(
    () => retailExecutionObserved("yes", { debug: { retailExecutions: 1 } }),
    /must be a boolean/,
  );
});

test("browser smoke arguments keep the harness local and assets explicit", () => {
  const parsed = parseArguments(
    [
      "--asset",
      "./local-data/s0000019.nsd",
      "--asset",
      "./local-data/s0000019.nsf",
      "--lid",
      "0x19",
      "--frames",
      "64",
      "--expect-final-key-count",
      "2",
      "--expect-final-item-pool-2",
      "0x00100400",
      "--unlock-all",
      "--seed-card",
      "./local-data/card.json",
      "--seed-resume",
      "./local-data/resume.json",
    ],
    {},
  );

  assert.equal(parsed.url, "http://127.0.0.1:4175/");
  assert.equal(parsed.bootLid, 0x19);
  assert.equal(parsed.frames, 64);
  assert.equal(parsed.expectFinalKeyCount, 2);
  assert.equal(parsed.expectFinalItemPool2, 0x0010_0400);
  assert.equal(parsed.unlockAll, true);
  assert.equal(parsed.cardStorageSeed.endsWith("/local-data/card.json"), true);
  assert.equal(parsed.resumeStorageSeed.endsWith("/local-data/resume.json"), true);
  assert.equal(parsed.assets.length, 2);
  const video = parseArguments(
    [
      "--asset",
      "./disc.bin",
      "--replay",
      "./campaign.json",
      "--video",
      "./target/campaign.mp4",
      "--chapters",
      "./target/campaign-chapters.json",
    ],
    {},
  );
  assert.equal(video.video.endsWith("/target/campaign.mp4"), true);
  assert.equal(
    video.chapters.endsWith("/target/campaign-chapters.json"),
    true,
  );
  assert.throws(
    () => parseArguments(["--video", "./target/campaign.mp4"], {}),
    /requires --replay/,
  );
  assert.throws(
    () => parseArguments(["--replay", "./campaign.json", "--video", "./target/campaign.mp4"], {}),
    /requires --chapters/,
  );
  const audit = parseArguments(
    ["--asset", "./disc.bin", "--frames", "1000000", "--audit-retail-pbaks"],
    {},
  );
  assert.equal(audit.auditRetailPbaks, true);
  const isolated = parseArguments(
    [
      "--asset",
      "./disc.bin",
      "--frames",
      "10000",
      "--audit-isolated-retail-pbak",
      "0x0f",
    ],
    {},
  );
  assert.equal(isolated.auditIsolatedRetailPbakLid, 0x0f);
  const cardRoundTrip = parseArguments(
    ["--asset", "./disc.bin", "--audit-card-round-trip"],
    {},
  );
  assert.equal(cardRoundTrip.auditCardRoundTrip, true);
  const directBonus = parseArguments(
    ["--asset", "./disc.bin", "--audit-direct-bonus-return"],
    {},
  );
  assert.equal(directBonus.auditDirectBonusReturn, true);
  assert.equal(directBonus.bootLid, 0x24);
  assert.equal(
    parseArguments([
      "--asset",
      "./disc.bin",
      "--lid",
      "0x24",
      "--audit-direct-bonus-return",
    ], {}).bootLid,
    0x24,
  );
  assert.throws(
    () => parseArguments(["--audit-retail-pbaks", "--lid", "0x0a"], {}),
    /requires Title boot LID/,
  );
  assert.throws(
    () => parseArguments(["--audit-retail-pbaks", "--replay", "route.json"], {}),
    /cannot be combined with --replay/,
  );
  assert.throws(
    () => parseArguments([
      "--audit-retail-pbaks",
      "--audit-isolated-retail-pbak",
      "0x1c",
    ], {}),
    /cannot be combined/,
  );
  assert.throws(
    () => parseArguments(["--audit-isolated-retail-pbak", "0x0a"], {}),
    /accepts only Upstream/,
  );
  for (const incompatible of [
    ["--audit-card-round-trip", "--replay", "route.json"],
    ["--audit-card-round-trip", "--audit-retail-pbaks"],
    ["--audit-card-round-trip", "--unlock-all"],
    ["--audit-card-round-trip", "--seed-card", "card.json"],
  ]) {
    assert.throws(() => parseArguments(incompatible, {}), /cannot be combined/);
  }
  assert.throws(
    () => parseArguments(["--audit-card-round-trip", "--lid", "0x09"], {}),
    /requires Title boot LID/,
  );
  assert.throws(
    () => parseArguments([
      "--audit-direct-bonus-return",
      "--lid",
      "0x25",
    ], {}),
    /requires Tawna Bonus 1 boot LID 0x24/,
  );
  for (const incompatible of [
    ["--audit-direct-bonus-return", "--replay", "route.json"],
    ["--audit-direct-bonus-return", "--audit-retail-pbaks"],
    ["--audit-direct-bonus-return", "--unlock-all"],
    ["--audit-direct-bonus-return", "--seed-resume", "resume.json"],
  ]) {
    assert.throws(() => parseArguments(incompatible, {}), /cannot be combined/);
  }
  assert.throws(
    () => parseArguments(["--asset", "./x.nsd", "--url", "https://example.com/"], {}),
    /loopback HTTP URL/,
  );

  const synthetic = parseArguments(["--synthetic-cooked-iso-import"], {});
  assert.equal(synthetic.syntheticCookedIsoImport, true);
  assert.deepEqual(synthetic.assets, []);
  for (const incompatible of [
    ["--synthetic-cooked-iso-import", "--asset", "./disc.iso"],
    ["--synthetic-cooked-iso-import", "--frames", "1"],
    ["--synthetic-cooked-iso-import", "--unlock-all"],
    ["--synthetic-cooked-iso-import", "--replay", "./route.json"],
    ["--synthetic-cooked-iso-import", "--seed-card", "./card.json"],
    ["--synthetic-cooked-iso-import", "--seed-resume", "./resume.json"],
  ]) {
    assert.throws(
      () => parseArguments(incompatible, {}),
      /cannot be combined/,
    );
  }
  assert.throws(
    () => parseArguments(["--seed-card", "a.json", "--seed-card", "b.json"], {}),
    /only once/,
  );
  assert.throws(
    () =>
      parseArguments(
        ["--seed-resume", "a.json", "--seed-resume", "b.json"],
        {},
      ),
    /only once/,
  );
  assert.throws(
    () => parseArguments([
      "--expect-final-key-count",
      "1",
      "--expect-final-key-count",
      "2",
    ], {}),
    /only once/,
  );
  assert.throws(
    () => parseArguments(["--expect-final-item-pool-2", "0x100000000"], {}),
    /0 through 4294967295/,
  );
});

test("showcase windows are repeatable, bounded, and retain retail defaults", () => {
  const defaults = parseArguments(["--asset", "./disc.bin"], {});
  assert.deepEqual(
    {
      outputAspect: defaults.outputAspect,
      renderResolution: defaults.renderResolution,
      cameraZoom: defaults.cameraZoom,
      smoothMotion: defaults.smoothMotion,
      extendedWorld: defaults.extendedWorld,
      framesExplicit: defaults.framesExplicit,
      videoWindows: defaults.videoWindows,
    },
    {
      outputAspect: "4:3",
      renderResolution: "native",
      cameraZoom: "100",
      smoothMotion: false,
      extendedWorld: false,
      framesExplicit: false,
      videoWindows: [],
    },
  );

  const showcase = parseArguments(
    [
      "--asset", "./disc.bin",
      "--replay", "./campaign.json",
      "--frames", "88700",
      "--chapters", "./target/showcase/metadata.json",
      "--output-aspect", "21:9",
      "--render-resolution", "1080",
      "--camera-zoom", "55",
      "--smooth-motion",
      "--extended-world",
      "--video-window", "opening:730:2680:./target/showcase/opening.mp4",
      "--video-window", "heavy:86500:88600:./target/showcase/heavy.mp4",
    ],
    {},
  );
  assert.equal(showcase.framesExplicit, true);
  assert.equal(showcase.outputAspect, "21:9");
  assert.equal(showcase.renderResolution, "1080");
  assert.equal(showcase.cameraZoom, "55");
  assert.equal(showcase.smoothMotion, true);
  assert.equal(showcase.extendedWorld, true);
  assert.deepEqual(
    showcase.videoWindows.map(({ name, startFrame, endFrame }) => ({
      name,
      startFrame,
      endFrame,
    })),
    [
      { name: "opening", startFrame: 730, endFrame: 2680 },
      { name: "heavy", startFrame: 86500, endFrame: 88600 },
    ],
  );
  assert.throws(
    () => parseArguments(
      [
        "--replay", "./campaign.json",
        "--frames", "88599",
        "--chapters", "./metadata.json",
        "--video-window", "heavy:86500:88600:./heavy.mp4",
      ],
      {},
    ),
    /stops before the final video window closes/,
  );
  assert.throws(
    () => parseVideoWindow("bad:12:12:clip.mp4"),
    /must exceed STARTFRAME/,
  );
});

test("presentation assertions enforce defaults and retarget explicit showcase values", () => {
  const retail = {
    smoothMotion: false,
    extendedWorld: false,
    cameraZoom: "100",
    outputAspect: "4:3",
    renderResolution: "native",
    canvasWidth: 640,
    canvasHeight: 480,
    rect: { x: 0, y: 0, width: 640, height: 480 },
  };
  assert.deepEqual(
    presentationFailures(retail, {
      smoothMotion: false,
      extendedWorld: false,
      cameraZoom: "100",
      outputAspect: "4:3",
      renderResolution: "native",
    }),
    [],
  );
  assert.match(
    presentationFailures(retail, {
      smoothMotion: true,
      extendedWorld: true,
      cameraZoom: "55",
      outputAspect: "21:9",
      renderResolution: "1080",
    }).join("\n"),
    /smoothMotion.*canvas.*21:9.*canvasHeight/s,
  );
  assert.deepEqual(
    presentationFailures(
      {
        smoothMotion: true,
        extendedWorld: true,
        cameraZoom: "55",
        outputAspect: "21:9",
        renderResolution: "1080",
        canvasWidth: 2520,
        canvasHeight: 1080,
        rect: { x: 0, y: 0, width: 2520, height: 1080 },
      },
      {
        smoothMotion: true,
        extendedWorld: true,
        cameraZoom: "55",
        outputAspect: "21:9",
        renderResolution: "1080",
      },
    ),
    [],
  );
});

test("browser smoke accepts only the exact IPv6 loopback literal", () => {
  const ipv6Loopback = parseArguments(
    ["--asset", "./x.nsd", "--url", "http://[::1]:4175/", "--no-server"],
    {},
  );
  assert.equal(ipv6Loopback.url, "http://[::1]:4175/");
  assert.equal(ipv6Loopback.startServer, false);
  assert.throws(
    () => parseArguments(["--asset", "./x.nsd", "--url", "http://[::2]:4175/"], {}),
    /loopback HTTP URL/,
  );
});

test("CLI terminal progression requirements extend replay expectations exactly", () => {
  const replay = normalizeReplay({
    schema: 1,
    bootLid: 0x19,
    segments: [{ frames: 1, held: 0 }],
    expect: { currentLid: 0x19 },
  });
  assert.equal(applyTerminalProgressionRequirements(replay), replay);

  const required = applyTerminalProgressionRequirements(replay, {
    expectFinalKeyCount: 2,
    expectFinalItemPool2: 0x0010_0400,
  });
  assert.deepEqual(required.expect, {
    currentLid: 0x19,
    keyCount: 2,
    itemPool2: 0x0010_0400,
  });
  assert.deepEqual(replay.expect, { currentLid: 0x19 });
  assert.deepEqual(
    expectationFailures(required.expect, {
      debug: {
        currentLid: 0x19,
        browserTestGlobals: {
          keyCount: 2,
          itemPool2: 0x0010_0400,
        },
      },
    }),
    [],
  );
  assert.match(
    expectationFailures(required.expect, {
      debug: {
        currentLid: 0x19,
        browserTestGlobals: {
          keyCount: 1,
          itemPool2: 0x400,
        },
      },
    }).join("\n"),
    /keyCount: expected 2, received 1[\s\S]*itemPool2: expected 1049600, received 1024/,
  );
});

test("CLI terminal progression requirements reject replay conflicts", () => {
  const replay = normalizeReplay({
    schema: 1,
    bootLid: 0x19,
    segments: [{ frames: 1, held: 0 }],
    expect: { keyCount: 1, itemPool2: 0x400 },
  });
  assert.throws(
    () => applyTerminalProgressionRequirements(replay, {
      expectFinalKeyCount: 2,
    }),
    /conflicts with replay\.expect\.keyCount=1/,
  );
  assert.throws(
    () => applyTerminalProgressionRequirements(replay, {
      expectFinalItemPool2: 0x0010_0400,
    }),
    /conflicts with replay\.expect\.itemPool2=1024/,
  );
  assert.deepEqual(
    applyTerminalProgressionRequirements(replay, {
      expectFinalKeyCount: 1,
      expectFinalItemPool2: 0x400,
    }).expect,
    replay.expect,
  );
});

test("storage seed parser preserves exact bounded versioned envelopes", () => {
  const resumeJson = JSON.stringify(resumeStorageEnvelope(), null, 2);
  assert.deepEqual(parseStorageSeedJson(resumeJson, "resume"), {
    key: "c1.browser-resume.v1",
    json: resumeJson,
  });

  const cardJson = JSON.stringify(cardStorageEnvelope());
  assert.deepEqual(parseStorageSeedJson(cardJson, "card"), {
    key: "c1.virtual-memory-card.v1",
    json: cardJson,
  });
});

test("storage seed parser rejects malformed, oversized, or non-exact input", () => {
  const secret = "PAYLOAD_MUST_NOT_APPEAR_IN_THE_ERROR";
  let malformedError;
  try {
    parseStorageSeedJson(`{${secret}`, "resume");
  } catch (error) {
    malformedError = error;
  }
  assert.match(malformedError?.message ?? "", /not valid JSON/);
  assert.equal(malformedError?.message.includes(secret), false);

  assert.throws(
    () => parseStorageSeedJson("x".repeat((16 * 1024) + 1), "resume"),
    /1 through 16384 UTF-8 bytes/,
  );
  assert.throws(
    () =>
      parseStorageSeedJson(
        JSON.stringify(resumeStorageEnvelope({ extra: true })),
        "resume",
      ),
    /only its versioned envelope fields/,
  );
  assert.throws(
    () =>
      parseStorageSeedJson(
        JSON.stringify(resumeStorageEnvelope({ version: 2 })),
        "resume",
      ),
    /version must be 1/,
  );
  assert.throws(
    () =>
      parseStorageSeedJson(
        JSON.stringify(resumeStorageEnvelope({ payload: secret })),
        "resume",
      ),
    /canonical base64 for exactly 128 bytes/,
  );
  assert.throws(
    () =>
      parseStorageSeedJson(
        JSON.stringify(cardStorageEnvelope({ slots: [] })),
        "card",
      ),
    /exactly 15 entries/,
  );
  assert.throws(
    () =>
      parseStorageSeedJson(
        JSON.stringify(cardStorageEnvelope({ updatedAt: -1 })),
        "card",
      ),
    /non-negative safe integer/,
  );
  assert.throws(
    () => parseStorageSeedJson(JSON.stringify(resumeStorageEnvelope()), "other"),
    /kind must be card or resume/,
  );
});

test("authored card round-trip evidence requires one exact atomic slot write", () => {
  const exactPayload =
    "KAAAAAgAAAAABwAAeFY0EgEAAADvAAAA3wAAAAAAACAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAHlnRSs=";
  const exact = cardStorageEnvelope({ updatedAt: 23 });
  exact.slots[0] = { payload: exactPayload, updatedAt: 23 };
  assert.deepEqual(cardRoundTripStorageFailures(JSON.stringify(exact)), []);
  assert.match(cardRoundTripStorageFailures(null).join("\n"), /did not create/);

  const wrongPayload = structuredClone(exact);
  wrongPayload.slots[0].payload = STORAGE_PAYLOAD;
  assert.match(
    cardRoundTripStorageFailures(JSON.stringify(wrongPayload)).join("\n"),
    /exact 128-byte fixture/,
  );
  const extraSlot = structuredClone(exact);
  extraSlot.slots[4] = { payload: exactPayload, updatedAt: 23 };
  assert.match(
    cardRoundTripStorageFailures(JSON.stringify(extraSlot)).join("\n"),
    /only slot zero/,
  );
  const splitTimestamp = structuredClone(exact);
  splitTimestamp.updatedAt = 24;
  assert.match(
    cardRoundTripStorageFailures(JSON.stringify(splitTimestamp)).join("\n"),
    /atomic write/,
  );
});

test("page reload resume evidence preserves the exact authored payload", () => {
  const exactPayload =
    "KAAAAAgAAAAABwAAeFY0EgEAAADvAAAA3wAAAAAAACAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAHlnRSs=";
  assert.deepEqual(
    resumeRoundTripStorageFailures(
      JSON.stringify(resumeStorageEnvelope({ payload: exactPayload, updatedAt: 23 })),
    ),
    [],
  );
  assert.match(
    resumeRoundTripStorageFailures(null).join("\n"),
    /did not create/,
  );
  assert.match(
    resumeRoundTripStorageFailures(
      JSON.stringify(resumeStorageEnvelope({ payload: STORAGE_PAYLOAD })),
    ).join("\n"),
    /exact authored 128-byte fixture/,
  );
  assert.match(
    resumeRoundTripStorageFailures(
      JSON.stringify(resumeStorageEnvelope({ payload: exactPayload, updatedAt: 0 })),
    ).join("\n"),
    /positive write timestamp/,
  );
});

test("synthetic cooked-ISO import evidence is exact and fail-closed", () => {
  const cleanSnapshot = {
    bootstrap: "running",
    runtimeState: "idle",
    runtimeStatus: "Local media ready",
    assetMessage: "Full set mounted: 43 playable pairs plus the Cave archive.",
    fileCount: 88,
    pairCount: 44,
    launchDisabled: false,
    progressHidden: true,
    runtimeLog:
      "> Mounted 88 streams from ISO 2048 without uploading it.\n"
      + "> Local game data is ready.",
    consoleErrors: [],
    harness: { lastError: null },
    debug: {
      glError: 0,
      retailFaultedObjects: 0,
      retailExecutionErrors: 0,
      retailZoneEventFailures: 0,
      retailRuntimeError: null,
      retailRuntimeWarning: null,
    },
  };
  const ranges = expectedSyntheticCookedIsoBlobRanges();
  const cleanEvidence = {
    blobRanges: ranges,
    arrayBufferSizes: ranges.map(({ start, end }) => end - start),
    networkRequests: [],
  };
  assert.deepEqual(
    syntheticCookedIsoImportFailures(cleanSnapshot, cleanEvidence),
    [],
  );

  const failures = syntheticCookedIsoImportFailures(
    {
      ...cleanSnapshot,
      pairCount: 43,
      runtimeLog: "> wrong layout",
    },
    {
      ...cleanEvidence,
      blobRanges: cleanEvidence.blobRanges.slice(1),
      networkRequests: [{ method: "POST", url: "http://127.0.0.1/upload" }],
    },
  ).join("\n");
  assert.match(failures, /pair count/);
  assert.match(failures, /ISO 2048/);
  assert.match(failures, /Blob\.slice/);
  assert.match(failures, /network activity/);
});

test("run-length replay validates 16-bit input and deterministic frame count", () => {
  const replay = normalizeReplay({
    schema: 1,
    bootLid: "0x19",
    unlockAll: true,
    traceFromSegment: 2,
    settleFrames: 120,
    segments: [
      { frames: 8, held: 0 },
      {
        frames: 1,
        held: "0x0800",
        expect: { mountedLid: 0x19, minFrame: 9 },
        settleFrames: 2,
        settleHeld: "0x0040",
      },
      {
        frames: 1,
        held: 0,
        while: { currentLid: 0x2d, mountedLid: 0x2d },
      },
    ],
    expect: {
      currentLid: 0x19,
      retailFrame: 123,
      retailDrawCount: 123,
      retailProcessDrawCount: 123,
      retailRandomSeed: 0x1234_5678,
      retailRandomSeedB: 0x8765_4321,
      retailHardRestarts: 0,
      retailLoadStates: 0,
      retailDeathCameraFrames: 0,
      paused: false,
      lifeCount: 3 << 8,
      playerLifeCount: 3 << 8,
      minRetailExecutions: 1,
    },
  });

  assert.equal(replay.bootLid, 0x19);
  assert.equal(replay.unlockAll, true);
  assert.equal(replay.traceFromSegment, 2);
  assert.equal(replay.settleFrames, 120);
  assert.equal(replay.totalFrames, 10);
  assert.equal(replay.maximumFrames, 132);
  assert.equal(replay.segments[1].inputKind, "physical");
  assert.equal(replay.segments[1].held, 0x0800);
  assert.equal(replay.segments[1].settleFrames, 2);
  assert.equal(replay.segments[1].settleHeld, 0x0040);
  assert.deepEqual(replay.segments[2].while, {
    currentLid: 0x2d,
    mountedLid: 0x2d,
  });
  assert.equal(replay.expect.retailDrawCount, 123);
  assert.equal(replay.expect.retailFrame, 123);
  assert.equal(replay.expect.retailProcessDrawCount, 123);
  assert.equal(replay.expect.retailRandomSeed, 0x1234_5678);
  assert.equal(replay.expect.retailRandomSeedB, 0x8765_4321);
  assert.equal(replay.expect.retailHardRestarts, 0);
  assert.equal(replay.expect.retailLoadStates, 0);
  assert.equal(replay.expect.retailDeathCameraFrames, 0);
  assert.equal(replay.expect.paused, false);
  assert.equal(replay.expect.lifeCount, 3 << 8);
  assert.equal(replay.expect.playerLifeCount, 3 << 8);
  assert.equal(
    normalizeReplay(
      { schema: 1, bootLid: 0x19, segments: [{ frames: 1, held: 0 }] },
      { unlockAll: true },
    ).unlockAll,
    true,
  );
  assert.equal(
    normalizeReplay({
      schema: 1,
      bootLid: 0x19,
      segments: [{ frames: 1, held: 0 }],
    }).settleFrames,
    0,
  );
  assert.throws(
    () =>
      normalizeReplay({
        schema: 1,
        bootLid: 0x19,
        settleFrames: 10_001,
        segments: [{ frames: 1, held: 0 }],
      }),
    /0 through 10000/,
  );
  assert.throws(
    () =>
      normalizeReplay({
        schema: 1,
        bootLid: 0x19,
        segments: [
          {
            frames: 1,
            held: 0,
            settleFrames: 10_001,
          },
        ],
      }),
    /0 through 10000/,
  );
  assert.throws(
    () =>
      normalizeReplay({
        schema: 1,
        bootLid: 0x19,
        segments: [{ frames: 1, held: 0x1_0000 }],
      }),
    /0 through 65535/,
  );
  assert.throws(
    () =>
      normalizeReplay({
        schema: 1,
        bootLid: 0x19,
        segments: [{ frames: 1, held: 0, while: {} }],
      }),
    /must contain at least one expectation/,
  );
  assert.throws(
    () =>
      normalizeReplay({
        schema: 1,
        bootLid: 0x19,
        segments: [
          {
            frames: 1,
            held: 0,
            while: { currentLid: 0x2d, minFrame: 10 },
          },
        ],
      }),
    /supports only currentLid and mountedLid/,
  );
  for (const traceFromSegment of [0, 2]) {
    assert.throws(
      () =>
        normalizeReplay({
          schema: 1,
          bootLid: 0x19,
          traceFromSegment,
          segments: [{ frames: 1, held: 0 }],
        }),
      /traceFromSegment/,
    );
  }
});

test("recorded replay segments preserve full 32-bit PBAK words explicitly", () => {
  const replay = normalizeReplay({
    schema: 1,
    bootLid: 0x0e,
    segments: [
      {
        frames: 2,
        inputKind: "recorded",
        held: "0x00100040",
        settleFrames: 1,
        settleHeld: "0xffffffff",
      },
      { frames: 1, inputKind: "physical", held: 0xffff },
    ],
  });

  assert.equal(replay.segments[0].inputKind, "recorded");
  assert.equal(replay.segments[0].held, 0x0010_0040);
  assert.equal(replay.segments[0].settleHeld, 0xffff_ffff);
  assert.equal(replayStepMethod(replay.segments[0].inputKind), "stepRecorded");
  assert.equal(replay.segments[1].inputKind, "physical");
  assert.equal(replay.segments[1].held, 0xffff);
  assert.equal(replayStepMethod(replay.segments[1].inputKind), "step");
  assert.throws(() => replayStepMethod("demo"), /unsupported replay input kind/);
  assert.throws(
    () =>
      normalizeReplay({
        schema: 1,
        bootLid: 0x0e,
        segments: [
          {
            frames: 1,
            inputKind: "recorded",
            held: 0x1_0000_0000,
          },
        ],
      }),
    /0 through 4294967295/,
  );
  assert.throws(
    () =>
      normalizeReplay({
        schema: 1,
        bootLid: 0x0e,
        segments: [{ frames: 1, inputKind: "demo", held: 0 }],
      }),
    /must be "physical", "recorded", or "snapshot"/,
  );
});

test("snapshot replay segments preserve complete native pad history", () => {
  const replay = normalizeReplay({
    schema: 1,
    bootLid: 0x19,
    segments: [{
      frames: 1,
      inputKind: "snapshot",
      held: 0,
      tapped: 0,
      heldPrevious: 0x0800,
      tappedPrevious: 0x0800,
      heldPrevious2: 0,
    }],
  });
  assert.deepEqual(replay.segments[0], {
    frames: 1,
    inputKind: "snapshot",
    held: 0,
    tapped: 0,
    heldPrevious: 0x0800,
    tappedPrevious: 0x0800,
    heldPrevious2: 0,
    beforeHeld: 0,
    beforeTapped: 0,
    beforeHeldPrevious: 0x0800,
    beforeTappedPrevious: 0x0800,
    beforeHeldPrevious2: 0,
    expect: {},
    while: undefined,
    settleFrames: 0,
    settleHeld: 0,
  });
  assert.equal(replayStepMethod("snapshot"), "stepSnapshotBoundary");
  for (const invalid of [
    { frames: 2 },
    { frames: 1, settleFrames: 1 },
  ]) {
    assert.throws(
      () => normalizeReplay({
        schema: 1,
        bootLid: 0x19,
        segments: [{
          inputKind: "snapshot",
          held: 0,
          tapped: 0,
          heldPrevious: 0,
          tappedPrevious: 0,
          heldPrevious2: 0,
          ...invalid,
        }],
      }),
      /snapshot input/,
    );
  }
});

test("browser snapshot rejects runtime, fault, console, and WebGL errors", () => {
  const clean = {
    bootstrap: "running",
    runtimeState: "running",
    harness: { lastError: null },
    debug: {
      glError: 0,
      retailFaultedObjects: 0,
      retailExecutionErrors: 0,
      retailZoneEventFailures: 0,
      retailRuntimeError: null,
      retailRuntimeWarning: null,
    },
    consoleErrors: [],
    runtimeLog: "> mounted",
  };
  assert.deepEqual(snapshotFailures(clean), []);

  const failures = snapshotFailures({
    ...clean,
    harness: { lastError: "bad step" },
    debug: { ...clean.debug, retailFaultedObjects: 1, glError: 1282 },
    consoleErrors: ["uncaught"],
    runtimeLog: "> mounted\n! authored fault",
  });
  assert.match(failures.join("\n"), /bad step/);
  assert.match(failures.join("\n"), /faulted retail objects/);
  assert.match(failures.join("\n"), /WebGL error/);
  assert.match(failures.join("\n"), /uncaught/);
  assert.match(failures.join("\n"), /authored fault/);
});

test("checkpoint expectations compare only exported read-only debug fields", () => {
  assert.deepEqual(
    expectationFailures(
      {
        mountedLid: 0x19,
        retailFrame: 64,
        retailDrawCount: 64,
        retailProcessDrawCount: 64,
        retailRandomSeed: 0x1234,
        retailRandomSeedB: 0x5678,
        retailHardRestarts: 0,
        retailLoadStates: 0,
        retailDeathCameraFrames: 0,
        paused: false,
        lifeCount: 3 << 8,
        playerLifeCount: 3 << 8,
        gemCount: 12,
        keyCount: 1,
        itemPool1: 0x1234_5678,
        itemPool2: 0x0010_0400,
        minFrame: 64,
        minRetailExecutions: 1,
      },
      {
        debug: {
          mountedLid: 0x19,
          retailFrame: 64,
          retailDrawCount: 64,
          retailProcessDrawCount: 64,
          retailRandomSeed: 0x1234,
          retailRandomSeedB: 0x5678,
          retailHardRestarts: 0,
          retailLoadStates: 0,
          retailDeathCameraFrames: 0,
          paused: false,
          playerLifeCount: 3 << 8,
          browserTestGlobals: {
            lifeCount: 3 << 8,
            gemCount: 12,
            keyCount: 1,
            itemPool1: 0x1234_5678,
            itemPool2: 0x0010_0400,
          },
          frame: 64,
          retailExecutions: 9,
        },
      },
    ),
    [],
  );
  assert.match(
    expectationFailures(
      {
        currentLid: 8,
        retailFrame: 64,
        retailRandomSeedB: 0x5678,
        retailHardRestarts: 0,
        retailLoadStates: 0,
        retailDeathCameraFrames: 0,
        paused: false,
        lifeCount: 3 << 8,
        playerLifeCount: 3 << 8,
        minRetailFrame: 65,
      },
      {
        debug: {
          currentLid: 0x19,
          paused: true,
          playerLifeCount: 4 << 8,
          retailRandomSeedB: 0x9999,
          retailHardRestarts: 2,
          retailLoadStates: 1,
          retailDeathCameraFrames: 17,
          browserTestGlobals: { lifeCount: 4 << 8 },
          retailFrame: 2,
        },
      },
    ).join("\n"),
    /currentLid.*retailFrame.*retailRandomSeedB.*retailHardRestarts.*retailLoadStates.*retailDeathCameraFrames.*paused.*playerLifeCount.*retailFrame.*lifeCount/s,
  );
});

test("live-object checkpoints match phase, motion, status, and collider identity", () => {
  const obstacleHandle = { arenaSlot: 4, arenaGeneration: 3, vm: 9 };
  const playerHandle = { arenaSlot: 96, arenaGeneration: 1, vm: 2 };
  const snapshot = {
    debug: {
      browserTestObjects: [
        {
          handle: obstacleHandle,
          entityId: 42,
          entityGroup: 3,
          programEid: 0x1234_5679,
          executable: 9,
          spawnSubtype: 3,
          subtype: 0x123,
          state: 7,
          pc: 81,
          zoneEid: 0x2234_5679,
          register65: 0x345,
          translation: { x: -1_234, y: 5_678, z: -90 },
          rotationYxz: { y: 111, x: 222, z: 333 },
          velocity: { x: -44, y: 55, z: -66 },
          frameBound: {
            min: { x: -1_500, y: 5_000, z: -200 },
            max: { x: -1_000, y: 6_000, z: 100 },
          },
          status: {
            a: 0x1122_3344,
            b: 0x5566_7788,
            c: 0x99aa_bbcc,
            stateFlags: 0xddee_ff00,
          },
          player: false,
          collider: null,
          faulted: false,
        },
        {
          handle: playerHandle,
          entityId: 5,
          executable: 0,
          subtype: 0,
          state: 2,
          zoneEid: 0x2234_5679,
          translation: { x: 10, y: 20, z: 30 },
          rotationYxz: { y: 0, x: 0, z: 0 },
          velocity: { x: 1, y: 2, z: 3 },
          frameBound: null,
          status: { a: 0, b: 0, c: 0, stateFlags: 0 },
          player: true,
          collider: obstacleHandle,
          faulted: false,
        },
      ],
    },
  };

  assert.deepEqual(
    liveObjectExpectationFailures(
      {
        executable: 9,
        subtype: 0x123,
        state: 7,
        minX: -1_300,
        maxX: -1_200,
        velocityX: -44,
        rotationY: 111,
        statusB: 0x5566_7788,
        register65: 0x345,
        hasFrameBound: true,
        player: false,
      },
      snapshot,
    ),
    [],
  );
  assert.deepEqual(
    liveObjectExpectationFailures(
      {
        player: true,
        hasCollider: true,
        colliderEntityId: 42,
        colliderExecutable: 9,
        colliderSubtype: 0x123,
        colliderState: 7,
      },
      snapshot,
    ),
    [],
  );
  assert.match(
    liveObjectExpectationFailures(
      { executable: 9, minVelocityX: 0 },
      snapshot,
    ).join("\n"),
    /no object matched/,
  );
  assert.match(
    liveObjectExpectationFailures({}, { debug: {} }).join("\n"),
    /unavailable/,
  );
});

test("replay validation bounds live-object phase predicates", () => {
  const replay = normalizeReplay({
    schema: 1,
    bootLid: 0x19,
    segments: [
      {
        frames: 1,
        held: 0,
        expect: {
          liveObject: {
            executable: 9,
            subtype: 0x123,
            register65: 0x345,
            minX: -1_300,
            maxX: -1_200,
            player: false,
            hasCollider: true,
          },
        },
      },
    ],
  });
  assert.deepEqual(replay.segments[0].expect.liveObject, {
    executable: 9,
    subtype: 0x123,
    register65: 0x345,
    minX: -1_300,
    maxX: -1_200,
    player: false,
    hasCollider: true,
  });
  assert.throws(
    () =>
      normalizeReplay({
        schema: 1,
        bootLid: 0x19,
        segments: [
          {
            frames: 1,
            held: 0,
            expect: { liveObject: { minX: 10, maxX: 9 } },
          },
        ],
      }),
    /must not exceed/,
  );
  assert.throws(
    () =>
      normalizeReplay({
        schema: 1,
        bootLid: 0x19,
        segments: [
          {
            frames: 1,
            held: 0,
            expect: { liveObject: { surprise: 1 } },
          },
        ],
      }),
    /not a supported live-object predicate/,
  );
});

test("all-level browser assertion permits spent lives after verifying launch", () => {
  const clean = {
    debug: {
      playerLifeCount: 999 << 8,
      browserTestGlobals: {
        allLevels: true,
        lifeCount: 999 << 8,
        initialLifeCount: 999 << 8,
        levelsUnlocked: 99,
        itemPool2: (1 << 10) | (1 << 20),
      },
    },
  };
  assert.deepEqual(allLevelsFailures(clean), []);
  assert.deepEqual(
    allLevelsFailures(clean, {
      requireStartingLives: true,
      requireLivePlayer: true,
    }),
    [],
  );

  const spentLife = {
    debug: {
      playerLifeCount: 998 << 8,
      browserTestGlobals: {
        ...clean.debug.browserTestGlobals,
        lifeCount: 998 << 8,
      },
    },
  };
  assert.deepEqual(
    allLevelsFailures(spentLife),
    [],
    "a legitimate life loss must remain valid after launch",
  );
  assert.match(
    allLevelsFailures(spentLife, {
      requireStartingLives: true,
      requireLivePlayer: true,
    }).join("\n"),
    /lifeCount.*at launch.*playerLifeCount.*at launch/s,
  );

  const fractionalLife = {
    debug: {
      playerLifeCount: (998 << 8) + 1,
      browserTestGlobals: {
        ...clean.debug.browserTestGlobals,
        lifeCount: (998 << 8) + 1,
      },
    },
  };
  assert.match(
    allLevelsFailures(fractionalLife).join("\n"),
    /lifeCount.*aligned 24\.8.*playerLifeCount.*aligned 24\.8/s,
  );

  const failures = allLevelsFailures({
    debug: {
      browserTestGlobals: {
        ...clean.debug.browserTestGlobals,
        lifeCount: (999 << 8) + 1,
        initialLifeCount: 4 << 8,
        itemPool2: 0,
      },
    },
  });
  assert.match(failures.join("\n"), /lifeCount/);
  assert.match(failures.join("\n"), /initialLifeCount/);
  assert.match(failures.join("\n"), /secret-path bits/);

  const accessFailures = allLevelsFailures({
    debug: {
      browserTestGlobals: {
        ...clean.debug.browserTestGlobals,
        allLevels: false,
        levelsUnlocked: 98,
      },
    },
  });
  assert.match(accessFailures.join("\n"), /all-level mode/);
  assert.match(accessFailures.join("\n"), /levelsUnlocked/);

  assert.match(
    allLevelsFailures(
      { debug: { browserTestGlobals: clean.debug.browserTestGlobals } },
      { requireLivePlayer: true },
    ).join("\n"),
    /live-player life count is unavailable/,
  );
});

test("all-level storage audit preserves exact card and resume state", () => {
  const cardKey = "c1.virtual-memory-card.v1";
  const resumeKey = "c1.browser-resume.v1";
  assert.deepEqual(allLevelsStorageFailures({}, {}), []);
  assert.deepEqual(
    allLevelsStorageFailures(
      { [cardKey]: "card-seed", [resumeKey]: "resume-seed" },
      { [cardKey]: "card-seed", [resumeKey]: "resume-seed" },
    ),
    [],
  );
  assert.match(
    allLevelsStorageFailures(
      { [cardKey]: "card-seed" },
      { [cardKey]: "changed", [resumeKey]: "unexpected" },
    ).join("\n"),
    /virtual card.*changed value.*browser resume.*no value.*changed value/s,
  );
  assert.throws(
    () => allLevelsStorageFailures([], {}),
    /storage seeds must be an object/,
  );
});
