#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { basename, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { writeComposedReplay } from "./browser-campaign-replay.mjs";

const PXM_SIGNATURE = "PXM ";
const PJM_SIGNATURE = "PJM ";
const STANDARD_CONTROLLER = 4;
const TITLE_LID = 0x19;

const PJM_BUTTONS = Object.freeze([
  0x0080, // Square
  0x0040, // Cross
  0x0020, // Circle
  0x0010, // Triangle
  0x0008, // R1
  0x0004, // L1
  0x0002, // R2
  0x0001, // L2
  0x8000, // Left
  0x4000, // Down
  0x2000, // Right
  0x1000, // Up
  0x0800, // Start
  0x0100, // Select
]);

function readU32(data, offset, label) {
  if (offset + 4 > data.length) throw new Error(`${label} is truncated`);
  return data.readUInt32LE(offset);
}

function movieHeader(data, signature) {
  if (data.length < 0x34) throw new Error(`${signature.trim()} header is truncated`);
  if (data.subarray(0, 4).toString("ascii") !== signature) {
    throw new Error(`movie signature is not ${JSON.stringify(signature)}`);
  }
  const version = readU32(data, 4, "movie version");
  if (version !== 2) throw new Error(`unsupported movie version ${version}`);
  const controller1 = data[0x0e];
  const controller2 = data[0x0f];
  if (controller1 !== STANDARD_CONTROLLER || controller2 !== STANDARD_CONTROLLER) {
    throw new Error(
      `both ports must use standard controllers; received ${controller1}/${controller2}`,
    );
  }
  return {
    version,
    flags: data[0x0c],
    frameCount: readU32(data, 0x10, "movie frame count"),
    rerecordCount: readU32(data, 0x14, "movie rerecord count"),
    controllerOffset: readU32(data, 0x2c, "controller-data offset"),
  };
}

function swapControllerBytes(value) {
  return ((value & 0xff) << 8) | (value >>> 8);
}

export function parsePxm(data) {
  const header = movieHeader(data, PXM_SIGNATURE);
  if ((header.flags & 0x02) !== 0) {
    throw new Error("savestate-anchored PXM movies are not supported");
  }
  if ((header.flags & 0x04) !== 0) {
    throw new Error("PAL PXM movies are not supported");
  }
  if ((header.flags & 0x20) !== 0) {
    throw new Error("PXM movies that enable emulator hacks are not supported");
  }
  const bytesPerFrame = 5;
  const payloadBytes = data.length - header.controllerOffset;
  if (payloadBytes < header.frameCount * bytesPerFrame) {
    throw new Error(
      `PXM controller data has ${payloadBytes} bytes for ${header.frameCount} frames`,
    );
  }
  const frames = [];
  for (let frame = 0; frame < header.frameCount; frame += 1) {
    const offset = header.controllerOffset + frame * bytesPerFrame;
    const player1Raw = data.readUInt16LE(offset);
    const player2Raw = data.readUInt16LE(offset + 2);
    const control = data[offset + 4];
    frames.push({
      held: swapControllerBytes(player1Raw),
      player2: swapControllerBytes(player2Raw),
      control,
      uninitialized: player1Raw === 0xffff && player2Raw === 0xffff && control === 0,
    });
  }
  let inputStartFrame = 0;
  while (frames[inputStartFrame]?.uninitialized) inputStartFrame += 1;
  return {
    format: "PXM",
    ...header,
    inputStartFrame,
    trailingBytes: payloadBytes - header.frameCount * bytesPerFrame,
    frames,
  };
}

function decodePjmPad(text, label) {
  if (text.length !== PJM_BUTTONS.length) {
    throw new Error(`${label} must contain exactly 14 button columns`);
  }
  let held = 0;
  for (let index = 0; index < text.length; index += 1) {
    if (text[index] !== ".") held |= PJM_BUTTONS[index];
  }
  return held;
}

export function parsePjm(data) {
  const header = movieHeader(data, PJM_SIGNATURE);
  const payload = data.subarray(header.controllerOffset).toString("ascii");
  const lines = payload.split(/\r?\n/u);
  if (lines.at(-1) === "") lines.pop();
  if (lines.length !== header.frameCount) {
    throw new Error(
      `PJM controller data has ${lines.length} lines; header declares ${header.frameCount}`,
    );
  }
  const frames = lines.map((line, frame) => {
    const match = /^(\S{14})\|(\S{14})\|([0-9a-fA-F]+)\|$/u.exec(line);
    if (!match) throw new Error(`PJM frame ${frame} has an invalid input row`);
    const control = Number.parseInt(match[3], 16);
    return {
      held: decodePjmPad(match[1], `PJM frame ${frame} player 1`),
      player2: decodePjmPad(match[2], `PJM frame ${frame} player 2`),
      control,
      uninitialized: false,
    };
  });
  return {
    format: "PJM",
    ...header,
    inputStartFrame: 0,
    trailingBytes: 0,
    frames,
  };
}

export function parsePsxMovie(data) {
  const signature = data.subarray(0, 4).toString("ascii");
  if (signature === PXM_SIGNATURE) return parsePxm(data);
  if (signature === PJM_SIGNATURE) return parsePjm(data);
  throw new Error(`unsupported PSX movie signature ${JSON.stringify(signature)}`);
}

function safeInteger(value, label, { minimum = 0, maximum = Number.MAX_SAFE_INTEGER } = {}) {
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${label} must be an integer from ${minimum} through ${maximum}`);
  }
  return parsed;
}

export function runLengthEncode(heldFrames, inputKind = "recorded") {
  const segments = [];
  for (const held of heldFrames) {
    const previous = segments.at(-1);
    if (previous?.held === held) {
      previous.frames += 1;
    } else {
      segments.push({ frames: 1, held, inputKind });
    }
  }
  return segments;
}

function resolvePhysicalOpposites(held) {
  let resolved = held & 0xffff;
  if ((resolved & 0x1000) !== 0) resolved &= ~0x4000;
  if ((resolved & 0x8000) !== 0) resolved &= ~0x2000;
  return resolved;
}

function shiftedPadSnapshot(previous, held) {
  return {
    held,
    tapped: (~previous.held & held) & 0xf9ff,
    heldPrevious: previous.held,
    tappedPrevious: previous.tapped,
    heldPrevious2: previous.heldPrevious,
  };
}

export function padSnapshotsForRetailTicks(
  movie,
  padRows,
  retailTickFrames,
  startFrame,
) {
  if (!Array.isArray(padRows) || padRows.length === 0) {
    throw new Error("PCSX pad trace contains no controller polls");
  }
  const snapshots = [];
  let previousTick = startFrame - 1;
  let pollIndex = 0;
  let latestHeld = 0;
  let snapshot = {
    held: 0,
    tapped: 0,
    heldPrevious: 0,
    tappedPrevious: 0,
    heldPrevious2: 0,
  };
  while (padRows[pollIndex]?.movie_frame < startFrame) {
    latestHeld = padRows[pollIndex].held;
    pollIndex += 1;
  }
  for (const tickFrame of retailTickFrames) {
    let latchedHeld = 0;
    while (padRows[pollIndex]?.movie_frame <= tickFrame) {
      const row = padRows[pollIndex];
      if (row.movie_frame > previousTick) {
        const sample = movie.frames[row.movie_frame];
        if (sample === undefined) {
          throw new Error(`PCSX pad trace frame ${row.movie_frame} exceeds the movie`);
        }
        if (!sample.uninitialized && sample.held !== row.held) {
          throw new Error(
            `PCSX pad trace frame ${row.movie_frame} held 0x${row.held.toString(16)} does not match movie 0x${sample.held.toString(16)}`,
          );
        }
        latestHeld = row.held;
        latchedHeld |= row.held;
      }
      pollIndex += 1;
    }
    const held = resolvePhysicalOpposites(latestHeld | latchedHeld);
    snapshot = shiftedPadSnapshot(snapshot, held);
    snapshots.push(snapshot);
    previousTick = tickFrame;
  }
  return snapshots;
}

export function convertMovieToReplay(movie, options = {}) {
  const startFrame = safeInteger(
    options.startFrame ?? movie.inputStartFrame,
    "start frame",
    { maximum: movie.frameCount },
  );
  const endFrame = safeInteger(options.endFrame ?? movie.frameCount, "end frame", {
    maximum: movie.frameCount,
  });
  if (endFrame <= startFrame) throw new Error("end frame must be after start frame");
  const nativeFramesPerRetailFrame = safeInteger(
    options.nativeFramesPerRetailFrame ?? 2,
    "native frames per retail frame",
    { minimum: 1, maximum: 16 },
  );
  const sampleIndex = safeInteger(options.sampleIndex ?? 1, "sample index", {
    maximum: nativeFramesPerRetailFrame - 1,
  });
  const bootLid = safeInteger(options.bootLid ?? TITLE_LID, "boot LID", {
    maximum: 0xff,
  });
  const heldFrames = [];
  const sampledNativeFrames = options.retailTickFrames ?? Array.from(
    {
      length: Math.ceil(
        Math.max(0, endFrame - (startFrame + sampleIndex)) / nativeFramesPerRetailFrame,
      ),
    },
    (_, index) => startFrame + sampleIndex + index * nativeFramesPerRetailFrame,
  ).filter((frame) => frame < endFrame);
  for (const frame of sampledNativeFrames) {
    if (frame < startFrame || frame >= endFrame) {
      throw new Error(`retail tick frame ${frame} is outside the selected movie window`);
    }
    const sample = movie.frames[frame];
    if (sample.uninitialized) {
      if (frame >= movie.inputStartFrame) {
        throw new Error(
          `native frame ${frame} has an uninitialized controller sample after input begins`,
        );
      }
      heldFrames.push(0);
      continue;
    }
    if (sample.player2 !== 0) {
      throw new Error(`native frame ${frame} uses player 2 input 0x${sample.player2.toString(16)}`);
    }
    if (sample.control !== 0) {
      throw new Error(`native frame ${frame} uses emulator control byte 0x${sample.control.toString(16)}`);
    }
    heldFrames.push(sample.held);
  }
  if (heldFrames.length === 0) throw new Error("sampling produced no retail input frames");
  let segments;
  if (options.padSnapshots !== undefined) {
    if (options.padSnapshots.length !== heldFrames.length) {
      throw new Error("pad snapshot count must match the sampled retail frame count");
    }
    segments = options.padSnapshots.map((snapshot) => ({
      frames: 1,
      inputKind: "snapshot",
      ...snapshot,
    }));
  } else {
    segments = runLengthEncode(heldFrames, options.inputKind ?? "recorded");
  }
  if (options.expectClean === true) {
    segments = segments.flatMap((segment) => {
      const chunks = [];
      let remaining = segment.frames;
      while (remaining > 0) {
        const frames = 1;
        chunks.push({
          ...segment,
          frames,
          expect: {
            retailHardRestarts: 0,
            retailLoadStates: 0,
            retailDeathCameraFrames: 0,
          },
        });
        remaining -= frames;
      }
      return chunks;
    });
  }
  const replay = {
    schema: 1,
    localDiagnosticOnly: true,
    canonicalCampaign: false,
    bootLid,
    unlockAll: false,
    segments,
    sourceMovie: {
      format: movie.format,
      version: movie.version,
      advertisedNativeFrames: movie.frameCount,
      flags: movie.flags,
      rerecordCount: movie.rerecordCount,
      nativeFrameStart: startFrame,
      nativeFrameEndExclusive: endFrame,
      nativeFramesPerRetailFrame,
      sampleIndex,
      sampling: options.padSnapshots === undefined
        ? options.retailTickFrames === undefined ? "fixed-step" : "retail-draw-trace"
        : "retail-draw-and-pad-poll-trace",
      retailFrames: heldFrames.length,
      trailingBytes: movie.trailingBytes,
    },
  };
  if (options.traceFromStart === true) replay.traceFromSegment = 1;
  return replay;
}

export function parseRetailStateTrace(text) {
  const lines = text.trim().split(/\r?\n/u);
  const requiredHeader = [
    "movie_frame",
    "lag_count",
    "psx_cycle",
    "current_lid",
    "game_state",
    "title_state",
    "checkpoint",
    "draw_count",
  ];
  const optionalTitleHeader = [
    "title_phase",
    "title_current",
    "title_next",
  ];
  const optionalClockAndTitleHeader = [
    "frames_elapsed",
    ...optionalTitleHeader,
  ];
  const header = lines.shift()?.split("\t");
  const supportedHeader = header !== undefined
    && requiredHeader.every((name, index) => header[index] === name)
    && (header.length === requiredHeader.length
      || JSON.stringify(header.slice(requiredHeader.length))
        === JSON.stringify(optionalTitleHeader)
      || JSON.stringify(header.slice(requiredHeader.length))
        === JSON.stringify(optionalClockAndTitleHeader));
  if (!supportedHeader) {
    throw new Error("PCSX state trace has an unsupported header");
  }
  const rows = lines.map((line, index) => {
    const fields = line.split("\t");
    if (fields.length !== header.length) {
      throw new Error(`PCSX state trace row ${index + 2} has ${fields.length} fields`);
    }
    return Object.fromEntries(header.map((name, field) => [
      name,
      safeInteger(fields[field], `PCSX state trace row ${index + 2} ${name}`, {
        maximum: 0xffff_ffff,
      }),
    ]));
  });
  let playbackStart = 0;
  for (let index = 1; index < rows.length; index += 1) {
    if (rows[index].movie_frame <= rows[index - 1].movie_frame) {
      if (rows[index].movie_frame > 10) {
        throw new Error("PCSX state trace movie frames must be strictly increasing");
      }
      playbackStart = index;
    }
  }
  return rows.slice(playbackStart);
}

export function parsePadTrace(text) {
  const lines = text.trim().split(/\r?\n/u);
  const expectedHeader = [
    "movie_frame",
    "lag_count",
    "first_poll",
    "psx_cycle",
    "psx_pc",
    "buttons",
  ];
  const header = lines.shift()?.split("\t");
  if (JSON.stringify(header) !== JSON.stringify(expectedHeader)) {
    throw new Error("PCSX pad trace has an unsupported header");
  }
  const rows = lines.map((line, index) => {
    const fields = line.split("\t");
    if (fields.length !== expectedHeader.length) {
      throw new Error(`PCSX pad trace row ${index + 2} has ${fields.length} fields`);
    }
    const firstPoll = safeInteger(fields[2], `PCSX pad trace row ${index + 2} first_poll`, {
      maximum: 1,
    });
    if (firstPoll !== 1) throw new Error("PCSX pad trace must contain only first polls");
    if (!/^[0-9a-f]{4}$/iu.test(fields[5])) {
      throw new Error(`PCSX pad trace row ${index + 2} has an invalid button word`);
    }
    const activeLow = Number.parseInt(fields[5], 16);
    return {
      movie_frame: safeInteger(fields[0], `PCSX pad trace row ${index + 2} movie_frame`, {
        maximum: 0xffff_ffff,
      }),
      lag_count: safeInteger(fields[1], `PCSX pad trace row ${index + 2} lag_count`, {
        maximum: 0xffff_ffff,
      }),
      held: swapControllerBytes((~activeLow) & 0xffff),
    };
  });
  let playbackStart = 0;
  for (let index = 1; index < rows.length; index += 1) {
    if (rows[index].movie_frame <= rows[index - 1].movie_frame) {
      if (rows[index].movie_frame > 10) {
        throw new Error("PCSX pad trace movie frames must be strictly increasing");
      }
      playbackStart = index;
    }
  }
  return rows.slice(playbackStart);
}

export function parseRetailPadUpdateTrace(text) {
  const lines = text.trim().split(/\r?\n/u);
  const expectedHeader = [
    "movie_frame",
    "lag_count",
    "psx_cycle",
    "tapped_before",
    "held_before",
    "held_previous_before",
    "tapped_previous_before",
    "held_previous_2_before",
  ];
  const header = lines.shift()?.split("\t");
  if (JSON.stringify(header) !== JSON.stringify(expectedHeader)) {
    throw new Error("PCSX retail PadUpdate trace has an unsupported header");
  }
  const rows = lines.map((line, index) => {
    const fields = line.split("\t");
    if (fields.length !== expectedHeader.length) {
      throw new Error(
        `PCSX retail PadUpdate trace row ${index + 2} has ${fields.length} fields`,
      );
    }
    const parsed = expectedHeader.map((name, field) => safeInteger(
      fields[field],
      `PCSX retail PadUpdate trace row ${index + 2} ${name}`,
      { maximum: 0xffff_ffff },
    ));
    return Object.fromEntries(expectedHeader.map((name, field) => [name, parsed[field]]));
  });
  let playbackStart = 0;
  for (let index = 1; index < rows.length; index += 1) {
    if (rows[index].movie_frame < rows[index - 1].movie_frame) {
      if (rows[index].movie_frame > 10) {
        throw new Error("PCSX retail PadUpdate trace movie frames must not decrease");
      }
      playbackStart = index;
    }
  }
  return rows.slice(playbackStart);
}

export function parseRetailBoundaryTrace(text) {
  const lines = text.trim().split(/\r?\n/u);
  const requiredHeader = [
    "movie_frame",
    "psx_cycle",
    "pc",
    "draw_count",
    "frame_stamp",
    "ticks_current_frame",
    "physics_ticks",
  ];
  const header = lines.shift()?.split("\t");
  if (
    header === undefined
    || requiredHeader.some((name, index) => header[index] !== name)
  ) {
    throw new Error("PCSX retail boundary trace has an unsupported header");
  }
  const rows = lines.map((line, index) => {
    const fields = line.split("\t");
    if (fields.length !== header.length) {
      throw new Error(
        `PCSX retail boundary trace row ${index + 2} has ${fields.length} fields`,
      );
    }
    return {
      movie_frame: safeInteger(
        fields[0],
        `PCSX retail boundary trace row ${index + 2} movie_frame`,
        { maximum: 0xffff_ffff },
      ),
      psx_cycle: safeInteger(
        fields[1],
        `PCSX retail boundary trace row ${index + 2} psx_cycle`,
        { maximum: 0xffff_ffff },
      ),
      draw_count: safeInteger(
        fields[3],
        `PCSX retail boundary trace row ${index + 2} draw_count`,
        { maximum: 0xffff_ffff },
      ),
      frame_stamp: safeInteger(
        fields[4],
        `PCSX retail boundary trace row ${index + 2} frame_stamp`,
        { maximum: 0xffff_ffff },
      ),
      ticks_current_frame: safeInteger(
        fields[5],
        `PCSX retail boundary trace row ${index + 2} ticks_current_frame`,
        { maximum: 0x7fff_ffff },
      ),
      physics_ticks: safeInteger(
        fields[6],
        `PCSX retail boundary trace row ${index + 2} physics_ticks`,
        { maximum: 0x7fff_ffff },
      ),
    };
  });
  for (let index = 1; index < rows.length; index += 1) {
    if (rows[index].psx_cycle <= rows[index - 1].psx_cycle) {
      throw new Error("PCSX retail boundary trace cycles must be strictly increasing");
    }
  }
  return rows;
}

export function parseRetailPadTimingTrace(text) {
  const lines = text.trim().split(/\r?\n/u);
  const requiredHeader = [
    "movie_frame",
    "psx_cycle",
    "frame_stamp",
    "ticks_current_frame",
    "physics_ticks",
  ];
  const header = lines.shift()?.split("\t");
  if (
    header === undefined
    || requiredHeader.some((name, index) => header[index] !== name)
  ) {
    throw new Error("PCSX retail PadUpdate timing trace has an unsupported header");
  }
  const rows = lines.map((line, index) => {
    const fields = line.split("\t");
    if (fields.length !== header.length) {
      throw new Error(
        `PCSX retail PadUpdate timing trace row ${index + 2} has ${fields.length} fields`,
      );
    }
    const field = (offset, name, maximum = 0xffff_ffff) => safeInteger(
      fields[offset],
      `PCSX retail PadUpdate timing trace row ${index + 2} ${name}`,
      { maximum },
    );
    return {
      movie_frame: field(0, "movie_frame"),
      psx_cycle: field(1, "psx_cycle"),
      frame_stamp: field(2, "frame_stamp"),
      ticks_current_frame: field(3, "ticks_current_frame", 0x7fff_ffff),
      physics_ticks: field(4, "physics_ticks", 0x7fff_ffff),
      physics_object_ticks: header.includes("physics_object_ticks")
        ? field(
            header.indexOf("physics_object_ticks"),
            "physics_object_ticks",
            0x7fff_ffff,
          )
        : undefined,
    };
  });
  for (let index = 1; index < rows.length; index += 1) {
    const delta = (rows[index].psx_cycle - rows[index - 1].psx_cycle) >>> 0;
    if (delta === 0 || delta >= 0x8000_0000) {
      throw new Error("PCSX retail PadUpdate timing trace cycles must be strictly increasing");
    }
  }
  return rows;
}

export function retailPadTimings(timingRows, padUpdateRows) {
  const byCycle = new Map(timingRows.map((row) => [row.psx_cycle, row]));
  return padUpdateRows.map((pad) => {
    const timing = byCycle.get(pad.psx_cycle);
    if (timing === undefined || timing.movie_frame !== pad.movie_frame) {
      throw new Error(
        `PCSX retail PadUpdate timing trace does not match frame ${pad.movie_frame} cycle ${pad.psx_cycle}`,
      );
    }
    const physicsTicks = timing.physics_object_ticks ?? timing.physics_ticks;
    if (physicsTicks === 0) {
      throw new Error(
        `PCSX retail PadUpdate timing trace has no physics timing at frame ${pad.movie_frame}`,
      );
    }
    return {
      frameStamp: timing.frame_stamp,
      ticksCurrentFrame: timing.ticks_current_frame,
      // Native physics reads the live CoreObjects timing word. The adjacent
      // fixed global is contextual evidence only and can remain stale across
      // a synchronous level load.
      ticksPerFrame: physicsTicks,
    };
  });
}

export function retailBoundaryTimings(boundaryRows, padUpdateRows) {
  const timings = [];
  let boundaryIndex = 0;
  for (const pad of padUpdateRows) {
    while (
      boundaryRows[boundaryIndex + 1] !== undefined
      && boundaryRows[boundaryIndex + 1].psx_cycle <= pad.psx_cycle
    ) {
      boundaryIndex += 1;
    }
    const boundary = boundaryRows[boundaryIndex];
    if (boundary === undefined || boundary.psx_cycle > pad.psx_cycle) {
      throw new Error(
        `PCSX retail boundary trace starts after pad update frame ${pad.movie_frame}`,
      );
    }
    if (boundary.physics_ticks === 0) {
      throw new Error(
        `PCSX retail boundary trace has no physics timing at pad update frame ${pad.movie_frame}`,
      );
    }
    timings.push({
      ticksCurrentFrame: boundary.ticks_current_frame,
      ticksPerFrame: boundary.physics_ticks,
    });
  }
  return timings;
}

export function retailPadUpdateTicks(
  padUpdateRows,
  stateRows,
  startFrame,
  endFrame,
) {
  const stateByFrame = new Map(stateRows.map((row) => [row.movie_frame, row]));
  const selected = [];
  for (let index = 0; index < padUpdateRows.length;) {
    const frame = padUpdateRows[index].movie_frame;
    let end = index + 1;
    while (padUpdateRows[end]?.movie_frame === frame) end += 1;
    if (frame >= startFrame && frame < endFrame) {
      const group = padUpdateRows.slice(index, end);
      const state = stateByFrame.get(frame);
      const previousState = stateByFrame.get(frame - 1);
      if (state === undefined) {
        throw new Error(`PCSX state trace is missing retail PadUpdate frame ${frame}`);
      }
      const destinationCoreObjectsCreate = previousState !== undefined
        && state.current_lid !== previousState.current_lid;
      if (group.length > 1) {
        // CoreObjectsCreate calls PadUpdate immediately before the first
        // ordinary update at an initial/same-VSync mount. The browser mount
        // performs that first call itself, so replay begins at the last call.
        for (let rowIndex = index + 1; rowIndex < end; rowIndex += 1) {
          selected.push({
            ...padUpdateRows[rowIndex],
            snapshot_after: padUpdateRows[rowIndex + 1],
          });
        }
      } else if (!destinationCoreObjectsCreate) {
        selected.push({
          ...group[0],
          snapshot_after: padUpdateRows[index + 1],
        });
      }
    }
    index = end;
  }
  if (selected.length === 0) {
    throw new Error("PCSX retail PadUpdate trace contains no replay ticks in the window");
  }
  if (selected.some((row) => row.snapshot_after === undefined)) {
    throw new Error("PCSX retail PadUpdate trace ends before a selected update completes");
  }
  return selected;
}

export function retailTickFramesFromTrace(rows, startFrame, endFrame) {
  const frames = [];
  for (let index = 1; index < rows.length; index += 1) {
    const previous = rows[index - 1];
    const current = rows[index];
    if (current.movie_frame < startFrame || current.movie_frame >= endFrame) continue;
    const validLid = current.current_lid <= 0xff00 && current.current_lid % 0x100 === 0;
    if (validLid && current.draw_count === ((previous.draw_count + 1) >>> 0)) {
      frames.push(current.movie_frame);
    }
  }
  if (frames.length === 0) throw new Error("PCSX state trace contains no retail ticks in the selected window");
  return frames;
}

function replayPrefix(raw, label) {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error(`${label} must contain a JSON object`);
  }
  if (raw.schema !== 1) throw new Error(`${label}.schema must equal 1`);
  if (raw.localDiagnosticOnly !== true || raw.canonicalCampaign !== false) {
    throw new Error(
      `${label} must opt in with localDiagnosticOnly=true and canonicalCampaign=false`,
    );
  }
  if (!Array.isArray(raw.segments) || raw.segments.length === 0) {
    throw new Error(`${label}.segments must be a non-empty array`);
  }
  if (!raw.expect || typeof raw.expect !== "object" || Array.isArray(raw.expect)) {
    throw new Error(`${label}.expect must describe the splice boundary`);
  }
  const currentLid = safeInteger(
    raw.expect.currentLid,
    `${label}.expect.currentLid`,
    { maximum: 0xff },
  );
  const mountedLid = safeInteger(
    raw.expect.mountedLid,
    `${label}.expect.mountedLid`,
    { maximum: 0xff },
  );
  if (currentLid !== mountedLid) {
    throw new Error(`${label} splice boundary must have matching current/mounted LIDs`);
  }
  const segments = structuredClone(raw.segments);
  const finalSegment = segments.at(-1);
  if (!finalSegment || typeof finalSegment !== "object" || Array.isArray(finalSegment)) {
    throw new Error(`${label} has an invalid final segment`);
  }
  if (finalSegment.expect !== undefined) {
    throw new Error(`${label} final segment already contains an expectation`);
  }
  finalSegment.expect = structuredClone(raw.expect);
  finalSegment.settleFrames = safeInteger(
    raw.settleFrames ?? 0,
    `${label}.settleFrames`,
    { maximum: 10_000 },
  );
  finalSegment.settleHeld = safeInteger(
    raw.finalPad?.held ?? finalSegment.held ?? 0,
    `${label}.finalPad.held`,
    { maximum: 0xffff },
  );
  return {
    bootLid: safeInteger(raw.bootLid, `${label}.bootLid`, { maximum: 0xff }),
    unlockAll: raw.unlockAll ?? false,
    boundaryLid: currentLid,
    boundary: structuredClone(raw.exitCheckpoint ?? raw.expect),
    segments,
  };
}

export function alignReplayAfterPrefix(replay, rawPrefix, label = "prefix replay") {
  const prefix = replayPrefix(rawPrefix, label);
  if (replay.bootLid !== prefix.boundaryLid) {
    throw new Error(
      `movie boot LID 0x${replay.bootLid.toString(16)} does not match `
      + `${label} boundary LID 0x${prefix.boundaryLid.toString(16)}`,
    );
  }
  return {
    ...replay,
    bootLid: prefix.bootLid,
    unlockAll: prefix.unlockAll,
    segments: [...prefix.segments, ...replay.segments],
    traceFromSegment: prefix.segments.length + 1,
    alignmentBoundary: {
      kind: "verified-replay-prefix",
      lid: prefix.boundaryLid,
      checkpoint: prefix.boundary,
      prefixSegments: prefix.segments.length,
    },
  };
}

export function usage() {
  return `Usage:
  node scripts/import-psx-movie-replay.mjs --movie PATH [options]

Options:
  --movie PATH          Legally local PCSX-RR PXM or PSXjin PJM movie
  --output PATH         Ignored/local browser-harness replay JSON
  --start-frame N       First native frame in the alignment window
  --end-frame N         Exclusive native-frame limit
  --sample-index N      Sample within each native-frame group (default: 1)
  --native-step N       Native frames per Crust retail frame (default: 2)
  --state-trace PATH    Instrumented PCSX per-VSync retail-state TSV
  --pad-trace PATH      Instrumented PCSX first-controller-poll TSV
  --pad-update-trace P  Instrumented retail PadUpdate-boundary TSV
  --pad-timing-trace P  Instrumented timing sampled at that exact boundary
  --retail-boundary-trace PATH
                        Instrumented GL/physics-timing boundary TSV
  --boot-lid N          Movie splice LID (or direct boot; default: 0x19)
  --input-kind KIND     Browser input path: recorded (default), physical, or snapshot
  --initial-neutral N   Neutral browser setup frames before traced input
  --prefix-replay PATH  Verified local replay ending at the movie splice LID
  --expect-clean        Fail on the first restart/load/death-camera tick
  --expect-trace-state  Also match the native current/mounted LID each tick
  --trace-from-start    Record browser state after every imported input tick
  --check               Parse, validate, and summarize without writing
  --force               Replace an existing ignored/local output
  --help                 Show this help

The importer byte-swaps PXM standard-pad words into Crust's pad layout, parses
PJM's 14 button columns, preserves opposing recorded directions, rejects player
2 and emulator-control events, samples one 60 Hz native input per ~30 Hz retail
tick, and run-length encodes the result. PXM's leading 0xffff controller samples
are skipped automatically. An explicit earlier --start-frame maps only those
leading not-yet-polled sentinels to neutral; a sentinel after input begins fails.

With --prefix-replay, the prefix's terminal expectation is moved onto its final
segment and checked before the first movie input is consumed. This preserves
the prefix's session, RNG, progression, and persistent-pad history instead of
cold-booting a gameplay level and mistaking setup drift for an engine mismatch.

The output is a legally local diagnostic artifact and must remain outside Git
or under target/, local-data/, artifacts/, captures/, or recordings/.
`;
}

export function parseArguments(argv) {
  const options = { check: false, force: false, help: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const value = () => {
      index += 1;
      if (index >= argv.length) throw new Error(`${argument} requires a value`);
      return argv[index];
    };
    switch (argument) {
      case "--movie": options.movie = resolve(value()); break;
      case "--output": options.output = resolve(value()); break;
      case "--start-frame": options.startFrame = value(); break;
      case "--end-frame": options.endFrame = value(); break;
      case "--sample-index": options.sampleIndex = value(); break;
      case "--native-step": options.nativeFramesPerRetailFrame = value(); break;
      case "--state-trace": options.stateTrace = resolve(value()); break;
      case "--pad-trace": options.padTrace = resolve(value()); break;
      case "--pad-update-trace": options.padUpdateTrace = resolve(value()); break;
      case "--pad-timing-trace": options.padTimingTrace = resolve(value()); break;
      case "--retail-boundary-trace": options.retailBoundaryTrace = resolve(value()); break;
      case "--boot-lid": {
        const raw = value();
        options.bootLid = /^0x[0-9a-f]+$/iu.test(raw)
          ? Number.parseInt(raw.slice(2), 16)
          : raw;
        break;
      }
      case "--input-kind": {
        const inputKind = value();
        if (!["recorded", "physical", "snapshot"].includes(inputKind)) {
          throw new Error("--input-kind must be recorded, physical, or snapshot");
        }
        options.inputKind = inputKind;
        break;
      }
      case "--initial-neutral": options.initialNeutralFrames = value(); break;
      case "--prefix-replay": options.prefixReplay = resolve(value()); break;
      case "--expect-clean": options.expectClean = true; break;
      case "--expect-trace-state": options.expectTraceState = true; break;
      case "--trace-from-start": options.traceFromStart = true; break;
      case "--check": options.check = true; break;
      case "--force": options.force = true; break;
      case "--help":
      case "-h": options.help = true; break;
      default: throw new Error(`unknown argument: ${argument}`);
    }
  }
  if (!options.help && options.movie === undefined) throw new Error("--movie is required");
  if (!options.help && !options.check && options.output === undefined) {
    throw new Error("--output is required unless --check is used");
  }
  if (options.check && options.output !== undefined) {
    throw new Error("--output cannot be combined with --check");
  }
  if (options.check && options.force) throw new Error("--force cannot be combined with --check");
  if (options.expectTraceState && !options.expectClean) {
    throw new Error("--expect-trace-state requires --expect-clean");
  }
  if (options.expectTraceState && options.stateTrace === undefined) {
    throw new Error("--expect-trace-state requires --state-trace");
  }
  if (options.inputKind === "snapshot" && options.stateTrace === undefined) {
    throw new Error("snapshot input requires --state-trace");
  }
  if (options.inputKind === "snapshot" && options.padUpdateTrace === undefined) {
    throw new Error("snapshot input requires --pad-update-trace");
  }
  if (options.retailBoundaryTrace !== undefined && options.inputKind !== "snapshot") {
    throw new Error("--retail-boundary-trace requires snapshot input");
  }
  if (options.padTimingTrace !== undefined && options.inputKind !== "snapshot") {
    throw new Error("--pad-timing-trace requires snapshot input");
  }
  if (options.padTimingTrace !== undefined && options.retailBoundaryTrace !== undefined) {
    throw new Error("--pad-timing-trace cannot be combined with --retail-boundary-trace");
  }
  return options;
}

export async function run(options) {
  const data = await readFile(options.movie);
  const movie = parsePsxMovie(data);
  let conversionOptions = options;
  let stateTraceData;
  let padTraceData;
  let padUpdateTraceData;
  let padTimingTraceData;
  let retailBoundaryTraceData;
  let tickRows;
  if (options.stateTrace !== undefined) {
    stateTraceData = await readFile(options.stateTrace);
    const rows = parseRetailStateTrace(stateTraceData.toString("utf8"));
    const startFrame = safeInteger(options.startFrame ?? movie.inputStartFrame, "start frame", {
      maximum: movie.frameCount,
    });
    const endFrame = safeInteger(options.endFrame ?? movie.frameCount, "end frame", {
      maximum: movie.frameCount,
    });
    const rowsByFrame = new Map(rows.map((row) => [row.movie_frame, row]));
    let retailTickFrames;
    if (options.inputKind === "snapshot") {
      padUpdateTraceData = await readFile(options.padUpdateTrace);
      const padUpdateRows = retailPadUpdateTicks(
        parseRetailPadUpdateTrace(padUpdateTraceData.toString("utf8")),
        rows,
        startFrame,
        endFrame,
      );
      let boundaryTimings;
      if (options.padTimingTrace !== undefined) {
        padTimingTraceData = await readFile(options.padTimingTrace);
        boundaryTimings = retailPadTimings(
          parseRetailPadTimingTrace(padTimingTraceData.toString("utf8")),
          padUpdateRows,
        );
      } else if (options.retailBoundaryTrace !== undefined) {
        retailBoundaryTraceData = await readFile(options.retailBoundaryTrace);
        boundaryTimings = retailBoundaryTimings(
          parseRetailBoundaryTrace(retailBoundaryTraceData.toString("utf8")),
          padUpdateRows,
        );
      }
      retailTickFrames = padUpdateRows.map((row) => row.movie_frame);
      conversionOptions = {
        ...options,
        retailTickFrames,
        padSnapshots: padUpdateRows.map((row, index) => ({
          tapped: row.snapshot_after.tapped_before,
          held: row.snapshot_after.held_before,
          heldPrevious: row.snapshot_after.held_previous_before,
          tappedPrevious: row.snapshot_after.tapped_previous_before,
          heldPrevious2: row.snapshot_after.held_previous_2_before,
          beforeTapped: row.tapped_before,
          beforeHeld: row.held_before,
          beforeHeldPrevious: row.held_previous_before,
          beforeTappedPrevious: row.tapped_previous_before,
          beforeHeldPrevious2: row.held_previous_2_before,
          ...(boundaryTimings?.[index]?.frameStamp !== undefined
            ? { frameStamp: boundaryTimings[index].frameStamp }
            : rowsByFrame.get(row.movie_frame)?.frames_elapsed === undefined
            ? {}
            : { frameStamp: rowsByFrame.get(row.movie_frame).frames_elapsed }),
          ...(boundaryTimings?.[index] ?? {}),
        })),
      };
    } else {
      retailTickFrames = retailTickFramesFromTrace(rows, startFrame, endFrame);
      conversionOptions = {
        ...options,
        retailTickFrames,
      };
    }
    tickRows = retailTickFrames.map((frame) => rowsByFrame.get(frame));
    if (tickRows.some((row) => row === undefined)) {
      throw new Error("PCSX state trace ends before a selected retail update is observable");
    }
    if (options.padTrace !== undefined) {
      padTraceData = await readFile(options.padTrace);
      parsePadTrace(padTraceData.toString("utf8"));
    }
  }
  let replay = convertMovieToReplay(movie, conversionOptions);
  const initialNeutralFrames = safeInteger(
    options.initialNeutralFrames ?? 0,
    "initial neutral frames",
    { maximum: 100 },
  );
  if (initialNeutralFrames > 0) {
    replay.segments.unshift({
      frames: initialNeutralFrames,
      inputKind: "physical",
      held: 0,
    });
  }
  if (options.expectTraceState) {
    if (replay.segments.length !== tickRows.length + Number(initialNeutralFrames > 0)) {
      throw new Error("trace-state expectations require one replay segment per retail tick");
    }
    for (let index = 0; index < replay.segments.length; index += 1) {
      if (initialNeutralFrames > 0 && index === 0) continue;
      const tickIndex = index - Number(initialNeutralFrames > 0);
      const lid = tickRows[tickIndex].current_lid >>> 8;
      replay.segments[index].expect.currentLid = lid;
      replay.segments[index].expect.mountedLid = lid;
      if (lid === TITLE_LID) {
        replay.segments[index].expect.titleState = tickRows[tickIndex].title_state;
      }
    }
  }
  replay.sourceMovie.file = basename(options.movie);
  replay.sourceMovie.sha256 = createHash("sha256").update(data).digest("hex");
  if (stateTraceData !== undefined) {
    replay.sourceMovie.stateTraceFile = basename(options.stateTrace);
    replay.sourceMovie.stateTraceSha256 = createHash("sha256")
      .update(stateTraceData)
      .digest("hex");
  }
  if (padTraceData !== undefined) {
    replay.sourceMovie.padTraceFile = basename(options.padTrace);
    replay.sourceMovie.padTraceSha256 = createHash("sha256")
      .update(padTraceData)
      .digest("hex");
  }
  if (padUpdateTraceData !== undefined) {
    replay.sourceMovie.padUpdateTraceFile = basename(options.padUpdateTrace);
    replay.sourceMovie.padUpdateTraceSha256 = createHash("sha256")
      .update(padUpdateTraceData)
      .digest("hex");
  }
  if (padTimingTraceData !== undefined) {
    replay.sourceMovie.padTimingTraceFile = basename(options.padTimingTrace);
    replay.sourceMovie.padTimingTraceSha256 = createHash("sha256")
      .update(padTimingTraceData)
      .digest("hex");
  }
  if (options.prefixReplay !== undefined) {
    const prefixData = await readFile(options.prefixReplay);
    let prefix;
    try {
      prefix = JSON.parse(prefixData.toString("utf8"));
    } catch (error) {
      throw new Error(`could not parse prefix replay: ${error.message}`);
    }
    replay = alignReplayAfterPrefix(replay, prefix);
    replay.alignmentBoundary.prefixFile = basename(options.prefixReplay);
    replay.alignmentBoundary.prefixSha256 = createHash("sha256")
      .update(prefixData)
      .digest("hex");
  }
  const summary = {
    format: movie.format,
    nativeFrames: movie.frameCount,
    inputStartFrame: movie.inputStartFrame,
    retailFrames: replay.sourceMovie.retailFrames,
    segments: replay.segments.length,
    bootLid: replay.bootLid,
    boundaryLid: replay.alignmentBoundary?.lid,
    sha256: replay.sourceMovie.sha256,
  };
  if (options.check) return { ...summary, checked: true };
  const output = await writeComposedReplay(options.output, replay, {
    force: options.force,
    protectedPaths: [options.movie],
  });
  return { ...summary, output };
}

async function main() {
  try {
    const options = parseArguments(process.argv.slice(2));
    if (options.help) {
      process.stdout.write(usage());
      return;
    }
    const result = await run(options);
    process.stdout.write(`PSX movie replay imported: ${JSON.stringify(result)}\n`);
  } catch (error) {
    process.stderr.write(`PSX movie replay import failed: ${error.stack ?? error}\n`);
    process.exitCode = 1;
  }
}

const invokedPath = process.argv[1] === undefined ? undefined : resolve(process.argv[1]);
if (invokedPath === fileURLToPath(import.meta.url)) await main();
