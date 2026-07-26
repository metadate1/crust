import assert from "node:assert/strict";
import test from "node:test";

import {
  allLevelsFailures,
  expectationFailures,
  normalizeReplay,
  parseArguments,
  snapshotFailures,
} from "./browser-harness-smoke.mjs";

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
      { frames: 1, held: 0 },
    ],
    expect: { currentLid: 0x19, minRetailExecutions: 1 },
  });

  assert.equal(replay.bootLid, 0x19);
  assert.equal(replay.unlockAll, true);
  assert.equal(replay.settleFrames, 120);
  assert.equal(replay.totalFrames, 10);
  assert.equal(replay.maximumFrames, 132);
  assert.equal(replay.segments[1].held, 0x0800);
  assert.equal(replay.segments[1].settleFrames, 2);
  assert.equal(replay.segments[1].settleHeld, 0x0040);
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
