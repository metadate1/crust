import assert from "node:assert/strict";
import test from "node:test";

import {
  alignReplayAfterPrefix,
  convertMovieToReplay,
  parseArguments,
  parsePjm,
  parsePxm,
  parseRetailBoundaryTrace,
  parseRetailPadTimingTrace,
  parseRetailPadUpdateTrace,
  parseRetailStateTrace,
  retailBoundaryTimings,
  retailPadTimings,
  retailPadUpdateTicks,
  runLengthEncode,
} from "./import-psx-movie-replay.mjs";

function movieHeader(signature, frameCount, controllerOffset = 0x34) {
  const header = Buffer.alloc(controllerOffset);
  header.write(signature, 0, "ascii");
  header.writeUInt32LE(2, 4);
  header[0x0e] = 4;
  header[0x0f] = 4;
  header.writeUInt32LE(frameCount, 0x10);
  header.writeUInt32LE(17, 0x14);
  header.writeUInt32LE(controllerOffset, 0x2c);
  return header;
}

function pxmFrame(player1, player2 = 0, control = 0) {
  const frame = Buffer.alloc(5);
  frame.writeUInt16LE(player1, 0);
  frame.writeUInt16LE(player2, 2);
  frame[4] = control;
  return frame;
}

function pjmLine(player1 = "..............", player2 = "..............", control = "0") {
  return `${player1}|${player2}|${control}|\r\n`;
}

test("PXM parser skips leading uninitialized samples and byte-swaps standard-pad words", () => {
  const data = Buffer.concat([
    movieHeader("PXM ", 6),
    pxmFrame(0xffff, 0xffff),
    pxmFrame(0xffff, 0xffff),
    pxmFrame(0),
    pxmFrame(0x0008),
    pxmFrame(0x4000),
    pxmFrame(0x4000),
  ]);
  const movie = parsePxm(data);
  assert.equal(movie.inputStartFrame, 2);
  assert.deepEqual(
    movie.frames.map(({ held }) => held),
    [0xffff, 0xffff, 0, 0x0800, 0x0040, 0x0040],
  );
  const replay = convertMovieToReplay(movie);
  assert.equal(replay.sourceMovie.retailFrames, 2);
  assert.deepEqual(replay.segments, [
    { frames: 1, held: 0x0800, inputKind: "recorded" },
    { frames: 1, held: 0x0040, inputKind: "recorded" },
  ]);
  const alignedReplay = convertMovieToReplay(movie, { startFrame: 0 });
  assert.deepEqual(alignedReplay.segments.slice(0, 2), [
    { frames: 1, held: 0, inputKind: "recorded" },
    { frames: 1, held: 0x0800, inputKind: "recorded" },
  ]);
});

test("PJM parser maps the documented 14 columns into Crust pad bits", () => {
  const lines = [
    pjmLine(".X.........U.."),
    pjmLine("#.......L...S."),
  ];
  const data = Buffer.concat([
    movieHeader("PJM ", lines.length),
    Buffer.from(lines.join(""), "ascii"),
  ]);
  const movie = parsePjm(data);
  assert.deepEqual(movie.frames.map(({ held }) => held), [
    0x0040 | 0x1000,
    0x0080 | 0x8000 | 0x0800,
  ]);
  const replay = convertMovieToReplay(movie, {
    nativeFramesPerRetailFrame: 1,
    sampleIndex: 0,
  });
  assert.deepEqual(replay.segments, [
    { frames: 1, held: 0x1040, inputKind: "recorded" },
    { frames: 1, held: 0x8880, inputKind: "recorded" },
  ]);
});

test("conversion rejects player-two and emulator-control events", () => {
  const base = {
    format: "PXM",
    version: 2,
    frameCount: 2,
    rerecordCount: 0,
    inputStartFrame: 0,
    trailingBytes: 0,
  };
  assert.throws(
    () => convertMovieToReplay({
      ...base,
      frames: [
        { held: 0, player2: 0, control: 0, uninitialized: false },
        { held: 0, player2: 0x40, control: 0, uninitialized: false },
      ],
    }),
    /player 2 input/u,
  );
  assert.throws(
    () => convertMovieToReplay({
      ...base,
      frames: [
        { held: 0, player2: 0, control: 0, uninitialized: false },
        { held: 0, player2: 0, control: 2, uninitialized: false },
      ],
    }),
    /emulator control byte/u,
  );
});

test("PXM parser rejects savestate, PAL, and emulator-hack movies", () => {
  for (const [flag, description] of [
    [0x02, /savestate-anchored/u],
    [0x04, /PAL/u],
    [0x20, /emulator hacks/u],
  ]) {
    const header = movieHeader("PXM ", 1);
    header[0x0c] = flag;
    assert.throws(() => parsePxm(Buffer.concat([header, pxmFrame(0)])), description);
  }
});

test("run-length encoding preserves recorded opposing directions", () => {
  assert.deepEqual(runLengthEncode([0xc000, 0xc000, 0x0040]), [
    { frames: 2, held: 0xc000, inputKind: "recorded" },
    { frames: 1, held: 0x0040, inputKind: "recorded" },
  ]);
});

test("CLI parses hexadecimal boot LIDs and refuses output in check mode", () => {
  const options = parseArguments([
    "--movie", "movie.pxm", "--check", "--boot-lid", "0x19",
  ]);
  assert.equal(options.check, true);
  assert.equal(options.bootLid, 0x19);
  assert.throws(
    () => parseArguments(["--movie", "movie.pxm", "--check", "--output", "x"]),
    /cannot be combined/u,
  );
});

test("retail PadUpdate trace parser preserves the complete pre-call pad history", () => {
  const rows = parseRetailPadUpdateTrace([
    "movie_frame\tlag_count\tpsx_cycle\ttapped_before\theld_before\theld_previous_before\ttapped_previous_before\theld_previous_2_before",
    "436\t0\t100\t0\t0\t0\t0\t0",
    "436\t0\t200\t2048\t2048\t0\t0\t0",
  ].join("\n"));
  assert.deepEqual(rows, [
    {
      movie_frame: 436,
      lag_count: 0,
      psx_cycle: 100,
      tapped_before: 0,
      held_before: 0,
      held_previous_before: 0,
      tapped_previous_before: 0,
      held_previous_2_before: 0,
    },
    {
      movie_frame: 436,
      lag_count: 0,
      psx_cycle: 200,
      tapped_before: 2048,
      held_before: 2048,
      held_previous_before: 0,
      tapped_previous_before: 0,
      held_previous_2_before: 0,
    },
  ]);
});

test("retail boundary timing selects the latest completed draw before PadUpdate", () => {
  const rows = parseRetailBoundaryTrace([
    "movie_frame\tpsx_cycle\tpc\tdraw_count\tframe_stamp\tticks_current_frame\tphysics_ticks",
    "100\t1000\t8001d63c\t1\t40\t28\t34",
    "102\t1600\t8001d63c\t2\t41\t29\t51",
    "103\t1900\t8001d63c\t3\t43\t30\t34",
  ].join("\n"));
  assert.deepEqual(retailBoundaryTimings(rows, [
    { movie_frame: 101, psx_cycle: 1200 },
    { movie_frame: 102, psx_cycle: 1700 },
  ]), [
    { ticksCurrentFrame: 28, ticksPerFrame: 34 },
    { ticksCurrentFrame: 29, ticksPerFrame: 51 },
  ]);
});

test("retail boundary timing fails when no pre-PadUpdate draw was captured", () => {
  assert.throws(
    () => retailBoundaryTimings(
      [{ psx_cycle: 1400, ticks_current_frame: 28, physics_ticks: 34 }],
      [{ movie_frame: 101, psx_cycle: 1200 }],
    ),
    /starts after pad update frame 101/u,
  );
});

test("direct PadUpdate timing uses the live native physics-object word", () => {
  const rows = parseRetailPadTimingTrace([
    "movie_frame\tpsx_cycle\tframe_stamp\tticks_current_frame\tphysics_ticks\tphysics_object_ticks\tplayer_x",
    "1715\t1000\t722\t5749\t34\t5800\t2073344",
    "1717\t1600\t723\t23\t51\t5834\t2073344",
  ].join("\n"));
  assert.deepEqual(retailPadTimings(rows, [
    { movie_frame: 1715, psx_cycle: 1000 },
    { movie_frame: 1717, psx_cycle: 1600 },
  ]), [
    { frameStamp: 722, ticksCurrentFrame: 5749, ticksPerFrame: 5800 },
    { frameStamp: 723, ticksCurrentFrame: 23, ticksPerFrame: 5834 },
  ]);
});

test("retail state trace parser accepts the optional native title boundary fields", () => {
  const rows = parseRetailStateTrace([
    "movie_frame\tlag_count\tpsx_cycle\tcurrent_lid\tgame_state\ttitle_state\tcheckpoint\tdraw_count\ttitle_phase\ttitle_current\ttitle_next",
    "436\t95\t125783274\t6400\t0\t10\t4294967295\t0\t6\t10\t10",
  ].join("\n"));
  assert.deepEqual(rows, [{
    movie_frame: 436,
    lag_count: 95,
    psx_cycle: 125783274,
    current_lid: 6400,
    game_state: 0,
    title_state: 10,
    checkpoint: 4294967295,
    draw_count: 0,
    title_phase: 6,
    title_current: 10,
    title_next: 10,
  }]);
});

test("retail state trace parser accepts the native GOOL frame stamp", () => {
  const rows = parseRetailStateTrace([
    "movie_frame\tlag_count\tpsx_cycle\tcurrent_lid\tgame_state\ttitle_state\tcheckpoint\tdraw_count\tframes_elapsed\ttitle_phase\ttitle_current\ttitle_next",
    "1007\t113\t287047234\t6400\t0\t8\t4294967295\t193\t375\t3\t8\t8",
  ].join("\n"));
  assert.equal(rows[0].frames_elapsed, 375);
  assert.equal(rows[0].title_phase, 3);
});

test("retail PadUpdate ticks skip mount calls and expose each completed snapshot", () => {
  const padRows = [
    { movie_frame: 10, held_before: 0 },
    { movie_frame: 10, held_before: 1 },
    { movie_frame: 12, held_before: 2 },
    { movie_frame: 13, held_before: 3 },
    { movie_frame: 14, held_before: 4 },
  ];
  const stateRows = [
    { movie_frame: 9, current_lid: 0x19 },
    { movie_frame: 10, current_lid: 0x19 },
    { movie_frame: 11, current_lid: 0x19 },
    { movie_frame: 12, current_lid: 0x09 },
    { movie_frame: 13, current_lid: 0x09 },
    { movie_frame: 14, current_lid: 0x09 },
  ];
  const ticks = retailPadUpdateTicks(padRows, stateRows, 10, 14);
  assert.deepEqual(ticks.map((row) => [
    row.movie_frame,
    row.held_before,
    row.snapshot_after.held_before,
  ]), [
    [10, 1, 2],
    [13, 3, 4],
  ]);
});

test("trace-from-start annotates imported replays without changing input segments", () => {
  const movie = {
    format: "PXM",
    version: 2,
    flags: 0,
    frameCount: 1,
    rerecordCount: 0,
    inputStartFrame: 0,
    trailingBytes: 0,
    frames: [{ held: 0, player2: 0, control: 0, uninitialized: false }],
  };
  const replay = convertMovieToReplay(movie, {
    nativeFramesPerRetailFrame: 1,
    sampleIndex: 0,
    traceFromStart: true,
  });
  assert.equal(replay.traceFromSegment, 1);
  assert.deepEqual(replay.segments, [
    { frames: 1, held: 0, inputKind: "recorded" },
  ]);
});

test("verified prefix alignment checks the mounted boundary before movie input", () => {
  const replay = {
    schema: 1,
    localDiagnosticOnly: true,
    canonicalCampaign: false,
    bootLid: 9,
    unlockAll: false,
    segments: [{ frames: 2, held: 0x1000, inputKind: "recorded" }],
    sourceMovie: {},
  };
  const aligned = alignReplayAfterPrefix(replay, {
    schema: 1,
    localDiagnosticOnly: true,
    canonicalCampaign: false,
    bootLid: 0x19,
    unlockAll: false,
    settleFrames: 120,
    finalPad: { held: 0x40 },
    segments: [{ frames: 1, held: 0x40, inputKind: "physical" }],
    expect: { currentLid: 9, mountedLid: 9, retailDrawCount: 668 },
    exitCheckpoint: { currentLid: 9, mountedLid: 9, retailDrawCount: 668 },
  });
  assert.equal(aligned.bootLid, 0x19);
  assert.equal(aligned.traceFromSegment, 2);
  assert.deepEqual(aligned.segments, [
    {
      frames: 1,
      held: 0x40,
      inputKind: "physical",
      expect: { currentLid: 9, mountedLid: 9, retailDrawCount: 668 },
      settleFrames: 120,
      settleHeld: 0x40,
    },
    { frames: 2, held: 0x1000, inputKind: "recorded" },
  ]);
  assert.equal(aligned.alignmentBoundary.kind, "verified-replay-prefix");
  assert.equal(aligned.alignmentBoundary.lid, 9);
});

test("prefix alignment fails closed when the splice LIDs differ", () => {
  assert.throws(
    () => alignReplayAfterPrefix(
      {
        schema: 1,
        localDiagnosticOnly: true,
        canonicalCampaign: false,
        bootLid: 9,
        segments: [{ frames: 1, held: 0 }],
      },
      {
        schema: 1,
        localDiagnosticOnly: true,
        canonicalCampaign: false,
        bootLid: 0x19,
        segments: [{ frames: 1, held: 0 }],
        expect: { currentLid: 0x0c, mountedLid: 0x0c },
      },
    ),
    /does not match/u,
  );
});

test("clean alignment checks cumulative failure counters after every input tick", () => {
  const replay = convertMovieToReplay({
    format: "PXM",
    version: 2,
    flags: 0,
    frameCount: 20,
    rerecordCount: 0,
    inputStartFrame: 0,
    trailingBytes: 0,
    frames: Array.from({ length: 20 }, () => ({
      held: 0x1000,
      player2: 0,
      control: 0,
      uninitialized: false,
    })),
  }, {
    nativeFramesPerRetailFrame: 1,
    sampleIndex: 0,
    expectClean: true,
  });
  assert.deepEqual(replay.segments.map(({ frames }) => frames), Array(20).fill(1));
  for (const segment of replay.segments) {
    assert.deepEqual(segment.expect, {
      retailHardRestarts: 0,
      retailLoadStates: 0,
      retailDeathCameraFrames: 0,
    });
  }
});
