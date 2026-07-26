import assert from "node:assert/strict";
import test from "node:test";

import {
  allLevelsFailures,
  destinationMountReady,
  expectationFailures,
  liveObjectExpectationFailures,
  nextReplayBatchFrameCount,
  normalizeReplay,
  parseArguments,
  replayLidConditionMatches,
  replayStepMethod,
  snapshotFailures,
} from "./browser-harness-smoke.mjs";

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
      "--unlock-all",
    ],
    {},
  );

  assert.equal(parsed.url, "http://127.0.0.1:4175/");
  assert.equal(parsed.bootLid, 0x19);
  assert.equal(parsed.frames, 64);
  assert.equal(parsed.unlockAll, true);
  assert.equal(parsed.assets.length, 2);
  assert.throws(
    () => parseArguments(["--asset", "./x.nsd", "--url", "https://example.com/"], {}),
    /loopback HTTP URL/,
  );
});

test("run-length replay validates 16-bit input and deterministic frame count", () => {
  const replay = normalizeReplay({
    schema: 1,
    bootLid: "0x19",
    unlockAll: true,
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
    expect: { currentLid: 0x19, minRetailExecutions: 1 },
  });

  assert.equal(replay.bootLid, 0x19);
  assert.equal(replay.unlockAll, true);
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
    /must be "physical" or "recorded"/,
  );
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
      { mountedLid: 0x19, minFrame: 64, minRetailExecutions: 1 },
      { debug: { mountedLid: 0x19, frame: 64, retailExecutions: 9 } },
    ),
    [],
  );
  assert.match(
    expectationFailures(
      { currentLid: 8, minRetailFrame: 65 },
      { debug: { currentLid: 0x19, retailFrame: 2 } },
    ).join("\n"),
    /currentLid.*retailFrame/s,
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
    allLevelsFailures(clean, { requireStartingLives: true }),
    [],
  );

  const spentLife = {
    debug: {
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
    allLevelsFailures(spentLife, { requireStartingLives: true }).join("\n"),
    /lifeCount.*at launch/,
  );

  const fractionalLife = {
    debug: {
      browserTestGlobals: {
        ...clean.debug.browserTestGlobals,
        lifeCount: (998 << 8) + 1,
      },
    },
  };
  assert.match(
    allLevelsFailures(fractionalLife).join("\n"),
    /lifeCount.*aligned 24\.8/,
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
});
