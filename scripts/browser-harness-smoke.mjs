import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createReadStream, createWriteStream } from "node:fs";
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  rename,
  rm,
  stat,
  statfs,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, dirname, extname, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

import { ChromeCdp } from "./chrome-cdp.mjs";
import {
  SYNTHETIC_COOKED_ISO_BYTES,
  createSyntheticRetailCookedIso,
  expectedSyntheticCookedIsoBlobRanges,
} from "./synthetic-retail-iso.mjs";

const repositoryRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const DEFAULT_URL = "http://127.0.0.1:4175/";
const DEFAULT_BOOT_LID = 0x19;
const DIRECT_BONUS_AUDIT_LID = 0x24;
const DEFAULT_FRAMES = 120;
const REPLAY_BATCH_FRAME_LIMIT = 128;
// A high-resolution showcase still renders every simulated source frame even
// when that frame is outside a capture window.  Keep those uncaptured CDP
// evaluations comfortably below Chrome's 20-second command timeout.
const SHOWCASE_UNCAPTURED_BATCH_FRAME_LIMIT = 32;
const REPLAY_ZERO_STEP_CALLBACK_LIMIT = 512;
const VIDEO_FRAME_RATE = 30;
const SOURCE_FRAME_DURATION_MS = 34;
const SHOWCASE_VIDEO_FRAME_RATE = 1000 / SOURCE_FRAME_DURATION_MS;
const SHOWCASE_VIDEO_FRAME_RATE_ARGUMENT = `1000/${SOURCE_FRAME_DURATION_MS}`;
const CAPTURE_AUDIO_SAMPLE_RATE = 44_100;
const CAPTURE_AUDIO_CHANNELS = 2;
const VIDEO_MINIMUM_FREE_BYTES = 3 * 1024 * 1024 * 1024;
const VIDEO_DISK_CHECK_INTERVAL = 300;
const PHYSICAL_INPUT_KIND = "physical";
const RECORDED_INPUT_KIND = "recorded";
const SNAPSHOT_INPUT_KIND = "snapshot";
const ALL_LEVELS_MAX_LIVES = 999 << 8;
const ALL_LEVELS_UNLOCK_GATE = 99;
const ALL_LEVELS_SECRET_PATH_BITS = (1 << 10) | (1 << 20);
const PAD_CROSS = 0x0040;
const PAD_DOWN = 0x4000;
const CARD_STORAGE_KEY = "c1.virtual-memory-card.v1";
const CARD_STORAGE_SCHEMA = "c1-virtual-memory-card";
const RESUME_STORAGE_KEY = "c1.browser-resume.v1";
const RESUME_STORAGE_SCHEMA = "c1-browser-resume";
const STORAGE_VERSION = 1;
const STORAGE_SLOT_COUNT = 15;
const STORAGE_PAYLOAD_BYTES = 128;
const CARD_ROUND_TRIP_PAYLOAD_BASE64 =
  "KAAAAAgAAAAABwAAeFY0EgEAAADvAAAA3wAAAAAAACAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAHlnRSs=";
const STORAGE_RELOAD_SENTINEL = "crust.browser-harness.storage-initialized";
const MAX_STORAGE_SEED_BYTES = 16 * 1024;
const STORAGE_KEYS = [CARD_STORAGE_KEY, RESUME_STORAGE_KEY];
const SUPPORTED_ASSET_EXTENSIONS = new Set([".bin", ".iso", ".nsd", ".nsf"]);
const OUTPUT_ASPECT_VALUES = new Set(["4:3", "16:9", "21:9", "screen"]);
const RENDER_RESOLUTION_VALUES = new Set([
  "native", "720", "1080", "1440", "2160",
]);
const CAMERA_ZOOM_VALUES = new Set(["100", "85", "70", "55"]);
const CHROME_CANDIDATES = [
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
  "/Applications/Chromium.app/Contents/MacOS/Chromium",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
];

function parseWholeNumber(raw, name, maximum) {
  const value =
    typeof raw === "number"
      ? raw
      : /^0x[0-9a-f]+$/i.test(raw)
        ? Number.parseInt(raw.slice(2), 16)
        : Number(raw);
  if (!Number.isSafeInteger(value) || value < 0 || value > maximum) {
    throw new Error(`${name} must be a whole number from 0 through ${maximum}`);
  }
  return value;
}

function parseSignedWholeNumber(raw, name) {
  const value = Number(raw);
  if (
    !Number.isSafeInteger(value) ||
    value < -0x8000_0000 ||
    value > 0x7fff_ffff
  ) {
    throw new Error(
      `${name} must be a signed 32-bit whole number`,
    );
  }
  return value;
}

export function parseVideoWindow(raw) {
  if (typeof raw !== "string") {
    throw new Error("--video-window must be NAME:STARTFRAME:ENDFRAME:OUTPUTPATH");
  }
  const match = raw.match(/^([^:]+):([^:]+):([^:]+):(.+)$/);
  if (!match) {
    throw new Error("--video-window must be NAME:STARTFRAME:ENDFRAME:OUTPUTPATH");
  }
  const [, name, rawStartFrame, rawEndFrame, outputPath] = match;
  if (!/^[a-z0-9][a-z0-9_-]*$/i.test(name)) {
    throw new Error("--video-window NAME must use letters, digits, underscores, or hyphens");
  }
  const startFrame = parseWholeNumber(
    rawStartFrame,
    `--video-window ${name} STARTFRAME`,
    1_000_000,
  );
  const endFrame = parseWholeNumber(
    rawEndFrame,
    `--video-window ${name} ENDFRAME`,
    1_000_000,
  );
  if (endFrame <= startFrame) {
    throw new Error(`--video-window ${name} ENDFRAME must exceed STARTFRAME`);
  }
  return {
    name,
    startFrame,
    endFrame,
    outputPath: resolve(outputPath),
  };
}

function normalizeReplayInputKind(raw, label) {
  const inputKind = raw ?? PHYSICAL_INPUT_KIND;
  if (
    inputKind !== PHYSICAL_INPUT_KIND
    && inputKind !== RECORDED_INPUT_KIND
    && inputKind !== SNAPSHOT_INPUT_KIND
  ) {
    throw new Error(`${label} must be "physical", "recorded", or "snapshot"`);
  }
  return inputKind;
}

export function replayStepMethod(inputKind) {
  if (inputKind === PHYSICAL_INPUT_KIND) return "step";
  if (inputKind === RECORDED_INPUT_KIND) return "stepRecorded";
  if (inputKind === SNAPSHOT_INPUT_KIND) return "stepSnapshotBoundary";
  throw new Error(`unsupported replay input kind ${JSON.stringify(inputKind)}`);
}

function replayStepArguments(inputKind, input) {
  if (inputKind !== SNAPSHOT_INPUT_KIND) return String(input);
  return `[${[
    input.held,
    input.tapped,
    input.heldPrevious,
    input.tappedPrevious,
    input.heldPrevious2,
    input.beforeHeld,
    input.beforeTapped,
    input.beforeHeldPrevious,
    input.beforeTappedPrevious,
    input.beforeHeldPrevious2,
    input.frameStamp ?? 0xffff_ffff,
    input.ticksCurrentFrame ?? 0xffff_ffff,
    input.ticksPerFrame ?? 0xffff_ffff,
  ].join(",")}]`;
}

export function nextReplayBatchFrameCount(
  remainingFrames,
  { isolateFirstFrame = false } = {},
) {
  if (!Number.isSafeInteger(remainingFrames) || remainingFrames < 1) {
    throw new Error("remaining replay frames must be a positive safe integer");
  }
  if (typeof isolateFirstFrame !== "boolean") {
    throw new Error("isolateFirstFrame must be a boolean");
  }
  return isolateFirstFrame
    ? 1
    : Math.min(remainingFrames, REPLAY_BATCH_FRAME_LIMIT);
}

export function showcaseWindowBatchFrameCount(
  sourceFrame,
  maximumFrames,
  windowStarts,
  { needsCapture = false } = {},
) {
  if (!Number.isSafeInteger(sourceFrame) || sourceFrame < 0) {
    throw new Error("showcase source frame must be a nonnegative safe integer");
  }
  if (!Number.isSafeInteger(maximumFrames) || maximumFrames < 1) {
    throw new Error("showcase maximum batch must be a positive safe integer");
  }
  if (
    !Array.isArray(windowStarts)
    || windowStarts.some(
      (startFrame) => !Number.isSafeInteger(startFrame) || startFrame < 0,
    )
  ) {
    throw new Error("showcase window starts must be nonnegative safe integers");
  }
  if (typeof needsCapture !== "boolean") {
    throw new Error("showcase capture state must be a boolean");
  }
  if (needsCapture) return 1;
  const nextStart = windowStarts
    .filter((startFrame) => startFrame > sourceFrame)
    .sort((left, right) => left - right)[0];
  return Math.min(
    maximumFrames,
    SHOWCASE_UNCAPTURED_BATCH_FRAME_LIMIT,
    nextStart === undefined ? maximumFrames : nextStart - sourceFrame,
  );
}

export function summarizeReplayHostCallbacks(
  simulationSteps,
  requestedSteps,
  { zeroStepLimit = REPLAY_ZERO_STEP_CALLBACK_LIMIT } = {},
) {
  if (!Array.isArray(simulationSteps)) {
    throw new Error("replay host-callback steps must be an array");
  }
  if (!Number.isSafeInteger(requestedSteps) || requestedSteps < 1) {
    throw new Error("requested replay steps must be a positive safe integer");
  }
  if (!Number.isSafeInteger(zeroStepLimit) || zeroStepLimit < 1) {
    throw new Error("zero-step callback limit must be a positive safe integer");
  }
  let executed = 0;
  let consecutiveZeroSteps = 0;
  let maximumConsecutiveZeroSteps = 0;
  for (const simulationStepped of simulationSteps) {
    if (typeof simulationStepped !== "boolean") {
      throw new Error("replay host-callback results must be booleans");
    }
    if (simulationStepped) {
      executed += 1;
      consecutiveZeroSteps = 0;
      if (executed > requestedSteps) {
        throw new Error("replay host callbacks exceeded the requested steps");
      }
    } else {
      consecutiveZeroSteps += 1;
      maximumConsecutiveZeroSteps = Math.max(
        maximumConsecutiveZeroSteps,
        consecutiveZeroSteps,
      );
      if (consecutiveZeroSteps > zeroStepLimit) {
        throw new Error(
          `replay exceeded ${zeroStepLimit} consecutive zero-step host callbacks`,
        );
      }
    }
  }
  return {
    executed,
    hostCallbacks: simulationSteps.length,
    consecutiveZeroSteps,
    maximumConsecutiveZeroSteps,
  };
}

export function validateReplayBatchExecution(
  executed,
  { mountedDestination = false, label = "browser replay" } = {},
) {
  if (!Number.isSafeInteger(executed) || executed < 0) {
    throw new Error("executed replay frames must be a nonnegative safe integer");
  }
  if (typeof mountedDestination !== "boolean") {
    throw new Error("mountedDestination must be a boolean");
  }
  if (typeof label !== "string" || label.length === 0) {
    throw new Error("replay batch label must be a nonempty string");
  }
  if (executed < 1 && !mountedDestination) {
    throw new Error(`${label} did not execute a cooperative simulation step`);
  }
  return executed;
}

export function destinationMountReady(
  snapshot,
  requestedLid,
  previousRuntimeLog = "",
) {
  const runtimeLog = snapshot?.runtimeLog ?? "";
  let appendedLog;
  if (runtimeLog.startsWith(previousRuntimeLog)) {
    appendedLog = runtimeLog.slice(previousRuntimeLog.length);
  } else {
    // The visible engineering log is bounded and can discard its oldest
    // lines during a verbose mount. Find the longest retained suffix/prefix
    // overlap so a marker from an earlier visit to the same LID cannot
    // acknowledge this request.
    const maximumOverlap = Math.min(
      previousRuntimeLog.length,
      runtimeLog.length,
      512,
    );
    const minimumOverlap = Math.min(16, previousRuntimeLog.length);
    let overlap = 0;
    for (
      let length = maximumOverlap;
      length >= minimumOverlap && overlap === 0;
      length -= 1
    ) {
      if (
        previousRuntimeLog.endsWith(runtimeLog.slice(0, length))
      ) {
        overlap = length;
      }
    }
    appendedLog = overlap === 0 ? "" : runtimeLog.slice(overlap);
  }
  const marker =
    `Mounted destination 0x${requestedLid.toString(16).padStart(2, "0")}:`;
  return Boolean(
    snapshot?.runtimeState === "running"
    && (
      snapshot.debug?.mountedLid === requestedLid
      || appendedLog.toLowerCase().includes(marker.toLowerCase())
    )
  );
}

export function retailExecutionObserved(previouslyObserved, snapshot) {
  if (typeof previouslyObserved !== "boolean") {
    throw new Error("previouslyObserved must be a boolean");
  }
  const executions = snapshot?.debug?.retailExecutions;
  return previouslyObserved
    || (Number.isSafeInteger(executions) && executions > 0);
}

export function retailGameplayReadyAfterMount(
  snapshot,
  expectedLid,
  mountExecutions,
) {
  if (!Number.isSafeInteger(expectedLid) || expectedLid < 0) {
    throw new Error("expectedLid must be a non-negative safe integer");
  }
  if (!Number.isSafeInteger(mountExecutions) || mountExecutions < 0) {
    throw new Error("mountExecutions must be a non-negative safe integer");
  }
  const debug = snapshot?.debug;
  return Boolean(
    snapshot?.runtimeState === "running"
    && debug?.currentLid === expectedLid
    && debug?.mountedLid === expectedLid
    && Number.isSafeInteger(debug?.retailExecutions)
    && debug.retailExecutions > mountExecutions
    && Array.isArray(debug?.browserTestObjects)
    && debug.browserTestObjects.some(
      (object) => object?.player === true && object?.faulted !== true,
    )
  );
}

export function directBonusReturnAuditFailures(snapshot) {
  const failures = [...snapshotFailures(snapshot)];
  const debug = snapshot?.debug ?? {};
  const harness = snapshot?.harness ?? {};
  if (harness.directBonusStateBoundary !== 32) {
    failures.push(
      `directBonusStateBoundary: expected 32, received ${JSON.stringify(harness.directBonusStateBoundary)}`,
    );
  }
  for (const [name, expected] of [
    ["currentLid", DEFAULT_BOOT_LID],
    ["mountedLid", DEFAULT_BOOT_LID],
    ["titleState", 5],
  ]) {
    if (debug[name] !== expected) {
      failures.push(
        `${name}: expected ${expected}, received ${JSON.stringify(debug[name])}`,
      );
    }
  }
  if (!(debug.retailLoadStates >= 1)) {
    failures.push(
      `retailLoadStates: expected at least 1, received ${JSON.stringify(debug.retailLoadStates)}`,
    );
  }
  const runtimeLog = snapshot?.runtimeLog ?? "";
  if (!runtimeLog.includes(
    "Completed a directly selected bonus without a parent snapshot; returning to the Main Menu.",
  )) {
    failures.push("runtime log is missing the direct-bonus completion classification");
  }
  if (!runtimeLog.includes("Mounted destination 0x19:")) {
    failures.push("runtime log is missing the Title destination mount");
  }
  return failures;
}

/**
 * Returns lines newly appended to the browser's bounded engineering log.
 * `Dom::log` discards old lines once the visible log grows past its cap, so a
 * simple string prefix check would silently lose evidence during long runs.
 */
export function appendedRuntimeLogLines(previousText, currentText) {
  if (typeof previousText !== "string" || typeof currentText !== "string") {
    throw new Error("runtime log snapshots must be strings");
  }
  const split = (text) => text.split("\n").filter((line) => line.length > 0);
  const previous = split(previousText);
  const current = split(currentText);
  const maximumOverlap = Math.min(previous.length, current.length);
  let overlap = 0;
  for (let length = maximumOverlap; length >= 1; length -= 1) {
    if (
      previous
        .slice(previous.length - length)
        .every((line, index) => line === current[index])
    ) {
      overlap = length;
      break;
    }
  }
  return current.slice(overlap);
}

const RETAIL_PBAK_ARMED =
  /^> Armed retail PBAK ([0-9A-Za-z_]{5}) \(([^,]+), ([0-9]+) recorded frames\);/;
const RETAIL_PBAK_STARTED = /^> Started retail PBAK ([0-9A-Za-z_]{5});/;
const RETAIL_PBAK_PAGER_WAIT =
  /^> Retail PBAK ([0-9A-Za-z_]{5}) physical open is waiting for the in-flight CD page transfer\.$/;
const RETAIL_PBAK_FINISHED =
  /^> Retail PBAK input ended \(([^)]+)\); caption .+ received event 0xE00 \(acknowledged: true\) and retained the authored return lock\.$/;
const RETAIL_PBAK_FINISH_ATTEMPT = /^> Retail PBAK input ended \(/;
const RETAIL_LEVEL_END =
  /^> Retail LEVEL_END resolved [^ ]+ to ([^ ]+) \(bonus return: (true|false)\)\.$/;
const RETAIL_PBAK_AUDIT_PROFILES = [
  { eid: "pb0aB", layout: "SpawnWords304", recordedFrames: 872 },
  { eid: "pb0cB", layout: "SpawnWords304", recordedFrames: 1_348 },
  { eid: "pb0eB", layout: "SpawnWords304", recordedFrames: 990 },
  { eid: "pb0fB", layout: "SpawnWords511", recordedFrames: 934 },
  { eid: "pb0iB", layout: "SpawnWords304", recordedFrames: 1_240 },
  { eid: "pb0sB", layout: "SpawnWords304", recordedFrames: 998 },
  { eid: "pb0tB", layout: "SpawnWords304", recordedFrames: 1_804 },
  { eid: "pb0wB", layout: "SpawnWords304", recordedFrames: 1_878 },
  { eid: "pb0FB", layout: "SpawnWords304", recordedFrames: 902 },
];
const NATURAL_TITLE_PBAK_EIDS = [
  "pb0aB",
  "pb0cB",
  "pb0eB",
  "pb0iB",
  "pb0tB",
  "pb0wB",
  "pb0FB",
];
const ISOLATED_TITLE_PBAK_BY_LID = new Map([
  [0x0f, "pb0fB"],
  [0x1c, "pb0sB"],
]);

function retailPbakAuditExpectedEids(options) {
  if (options.auditRetailPbaks) return [...NATURAL_TITLE_PBAK_EIDS];
  if (options.auditIsolatedRetailPbakLid !== undefined) {
    return [ISOLATED_TITLE_PBAK_BY_LID.get(options.auditIsolatedRetailPbakLid)];
  }
  return null;
}

function pbakMetricSnapshot(entry) {
  return Object.fromEntries(
    [
      "stepCount",
      "hostCallbackCount",
      "currentLid",
      "mountedLid",
      "retailFrame",
      "retailDrawCount",
      "retailProcessDrawCount",
      "retailHardRestarts",
      "retailLoadStates",
      "retailDeathCameraFrames",
      "retailExecutions",
      "retailExecutionErrors",
      "retailFaultedObjects",
      "retailZoneEventFailures",
      "retailRandomSeed",
      "retailRandomSeedB",
    ].map((name) => [name, entry[name] ?? null]),
  );
}

/**
 * Reduces retained browser log observations into exact authored-demo runs.
 * Any duplicate, out-of-order, mismatched, or unclosed arm/start/finish event
 * is rejected instead of being interpreted as coverage.
 */
export function parseRetailPbakEvidence(
  entries,
  { allowIncomplete = false, allowTrailingRepeat = false } = {},
) {
  if (!Array.isArray(entries)) {
    throw new Error("retail PBAK log evidence must be an array");
  }
  if (typeof allowIncomplete !== "boolean") {
    throw new Error("allowIncomplete must be a boolean");
  }
  if (typeof allowTrailingRepeat !== "boolean") {
    throw new Error("allowTrailingRepeat must be a boolean");
  }
  const completed = [];
  let active;
  let awaitingTransition;
  for (const [index, entry] of entries.entries()) {
    if (!entry || typeof entry !== "object" || typeof entry.line !== "string") {
      throw new Error(`retail PBAK log evidence ${index} is malformed`);
    }
    const armed = entry.line.match(RETAIL_PBAK_ARMED);
    if (armed) {
      if (active) {
        throw new Error(`retail PBAK ${active.eid} was armed without finishing`);
      }
      active = {
        eid: armed[1],
        layout: armed[2],
        recordedFrames: parseWholeNumber(
          armed[3],
          `retail PBAK ${armed[1]} recorded frames`,
          0xffff_ffff,
        ),
        armed: pbakMetricSnapshot(entry),
      };
      continue;
    }
    const started = entry.line.match(RETAIL_PBAK_STARTED);
    if (started) {
      if (!active) {
        throw new Error(`retail PBAK ${started[1]} started without being armed`);
      }
      if (active.eid !== started[1]) {
        throw new Error(
          `retail PBAK ${started[1]} started while ${active.eid} was armed`,
        );
      }
      if (active.started) {
        throw new Error(`retail PBAK ${active.eid} started more than once`);
      }
      active.started = pbakMetricSnapshot(entry);
      continue;
    }
    const pagerWait = entry.line.match(RETAIL_PBAK_PAGER_WAIT);
    if (pagerWait) {
      if (!active) {
        throw new Error(`retail PBAK ${pagerWait[1]} waited without being armed`);
      }
      if (active.eid !== pagerWait[1]) {
        throw new Error(
          `retail PBAK ${pagerWait[1]} waited while ${active.eid} was armed`,
        );
      }
      if (active.started) {
        throw new Error(`retail PBAK ${active.eid} waited after starting`);
      }
      (active.pagerWaits ??= []).push(pbakMetricSnapshot(entry));
      continue;
    }
    const finished = entry.line.match(RETAIL_PBAK_FINISHED);
    if (finished) {
      if (!active?.started) {
        throw new Error("retail PBAK input finished without an active started recording");
      }
      active.finishReason = finished[1];
      active.finished = pbakMetricSnapshot(entry);
      active.wallFrames =
        Number.isSafeInteger(active.started.stepCount)
        && Number.isSafeInteger(active.finished.stepCount)
          ? active.finished.stepCount - active.started.stepCount + 1
          : null;
      for (const [name, startName, finishName] of [
        ["hardRestartsDuringPlayback", "retailHardRestarts", "retailHardRestarts"],
        ["loadStatesDuringPlayback", "retailLoadStates", "retailLoadStates"],
        ["deathCameraFramesDuringPlayback", "retailDeathCameraFrames", "retailDeathCameraFrames"],
      ]) {
        const start = active.started[startName];
        const end = active.finished[finishName];
        active[name] = Number.isSafeInteger(start) && Number.isSafeInteger(end)
          ? end - start
          : null;
      }
      completed.push(active);
      awaitingTransition = active;
      active = undefined;
      continue;
    }
    if (RETAIL_PBAK_FINISH_ATTEMPT.test(entry.line)) {
      throw new Error(
        `retail PBAK ${active?.eid ?? "completion"} did not publish the exact successful caption acknowledgement`,
      );
    }
    const levelEnd = entry.line.match(RETAIL_LEVEL_END);
    if (levelEnd && awaitingTransition && awaitingTransition.transition === undefined) {
      const raw = levelEnd[1];
      awaitingTransition.transition = {
        targetLid: /^0x[0-9a-f]+$/i.test(raw)
          ? Number.parseInt(raw.slice(2), 16)
          : Number(raw),
        bonusReturn: levelEnd[2] === "true",
        observed: pbakMetricSnapshot(entry),
      };
    }
  }
  if (active) {
    const legalTrailingRepeat =
      allowTrailingRepeat
      && !active.started
      && completed.some((run) => run.eid === active.eid);
    if (!allowIncomplete && !legalTrailingRepeat) {
      throw new Error(
        `retail PBAK ${active.eid} was ${active.started ? "started" : "armed"} without finishing`,
      );
    }
  }
  return completed;
}

/**
 * Validates every completed browser-observed attract run against the exact
 * requested owned-disc census. Repeats are legal; a complete audit requires
 * one clean Title return for every explicitly requested authored recording.
 */
export function retailPbakAuditFailures(
  evidence,
  {
    requireAll = true,
    expectedEids = RETAIL_PBAK_AUDIT_PROFILES.map((profile) => profile.eid),
  } = {},
) {
  if (!Array.isArray(evidence)) {
    throw new Error("retail PBAK audit evidence must be an array");
  }
  if (typeof requireAll !== "boolean") {
    throw new Error("requireAll must be a boolean");
  }
  if (
    !Array.isArray(expectedEids)
    || expectedEids.length === 0
    || expectedEids.some((eid) => typeof eid !== "string")
    || new Set(expectedEids).size !== expectedEids.length
  ) {
    throw new Error("expectedEids must be a nonempty array of unique strings");
  }
  const allProfiles = new Map(
    RETAIL_PBAK_AUDIT_PROFILES.map((profile) => [profile.eid, profile]),
  );
  const profiles = new Map(
    expectedEids.map((eid) => {
      const profile = allProfiles.get(eid);
      if (!profile) throw new Error(`unknown expected retail PBAK EID ${eid}`);
      return [eid, profile];
    }),
  );
  const returned = new Set();
  const failures = [];
  for (const [index, run] of evidence.entries()) {
    const label = `retail PBAK audit run ${index + 1}`;
    const profile = profiles.get(run?.eid);
    if (!profile) {
      failures.push(`${label} has unknown EID ${JSON.stringify(run?.eid)}`);
      continue;
    }
    if (run.layout !== profile.layout) {
      failures.push(
        `${label} ${run.eid} layout ${JSON.stringify(run.layout)}; expected ${profile.layout}`,
      );
    }
    if (run.recordedFrames !== profile.recordedFrames) {
      failures.push(
        `${label} ${run.eid} reports ${run.recordedFrames} frames; expected ${profile.recordedFrames}`,
      );
    }
    if (run.finishReason !== "Finished") {
      failures.push(
        `${label} ${run.eid} ended with ${JSON.stringify(run.finishReason)} instead of Finished`,
      );
    }
    for (const metric of [
      "retailExecutionErrors",
      "retailFaultedObjects",
      "retailZoneEventFailures",
    ]) {
      if (run.finished?.[metric] !== 0) {
        failures.push(
          `${label} ${run.eid} finished with ${metric}=${JSON.stringify(run.finished?.[metric])}`,
        );
      }
    }
    if (run.transition !== undefined) {
      if (run.transition.targetLid !== DEFAULT_BOOT_LID) {
        failures.push(
          `${label} ${run.eid} returned to ${JSON.stringify(run.transition.targetLid)} instead of Title`,
        );
      } else if (run.transition.bonusReturn) {
        failures.push(`${label} ${run.eid} unexpectedly used a bonus return`);
      } else {
        returned.add(run.eid);
      }
    } else if (requireAll) {
      failures.push(`${label} ${run.eid} has no observed LEVEL_END return`);
    }
  }
  if (requireAll) {
    for (const profile of profiles.values()) {
      if (!returned.has(profile.eid)) {
        failures.push(`retail PBAK audit did not complete ${profile.eid}`);
      }
    }
  }
  return failures;
}

export function retailPbakAuditTitleReady(snapshot) {
  return Boolean(
    snapshot?.runtimeState === "running"
    && snapshot.runtimeStatus === "Rust runtime active"
    && snapshot.debug?.currentLid === DEFAULT_BOOT_LID
    && snapshot.debug?.mountedLid === DEFAULT_BOOT_LID
    && Number.isSafeInteger(snapshot.debug?.mountedPages)
    && snapshot.debug.mountedPages > 0
    && Number.isSafeInteger(snapshot.debug?.mountedEntries)
    && snapshot.debug.mountedEntries > 0
    && snapshot.harness?.lastRequestedLid == null
  );
}

function retailPbakAuditCoverageComplete(evidence, expectedEids, snapshot) {
  const failures = retailPbakAuditFailures(evidence, {
    requireAll: false,
    expectedEids,
  });
  if (failures.length > 0) {
    throw new Error(`retail PBAK audit failed:\n${failures.join("\n")}`);
  }
  const returned = new Set(
    evidence
      .filter(
        (run) =>
          run.transition?.targetLid === DEFAULT_BOOT_LID
          && run.transition.bonusReturn === false,
      )
      .map((run) => run.eid),
  );
  return expectedEids.every((eid) => returned.has(eid))
    && retailPbakAuditTitleReady(snapshot);
}

function storageSeedObject(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be a JSON object`);
  }
  return value;
}

function requireExactKeys(value, expected, label) {
  const keys = Object.keys(value);
  if (
    keys.length !== expected.length
    || keys.some((key) => !expected.includes(key))
  ) {
    throw new Error(`${label} must contain only its versioned envelope fields`);
  }
}

function requireTimestamp(value, label) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${label} must be a non-negative safe integer`);
  }
}

function requireStoragePayload(value, label) {
  if (typeof value !== "string") {
    throw new Error(`${label} must be a base64 string`);
  }
  const decoded = Buffer.from(value, "base64");
  if (
    decoded.length !== STORAGE_PAYLOAD_BYTES
    || decoded.toString("base64") !== value
  ) {
    throw new Error(
      `${label} must be canonical base64 for exactly ${STORAGE_PAYLOAD_BYTES} bytes`,
    );
  }
}

function validateStorageHeader(envelope, schema, label) {
  if (envelope.schema !== schema) {
    throw new Error(`${label}.schema does not identify the expected storage format`);
  }
  if (envelope.version !== STORAGE_VERSION) {
    throw new Error(`${label}.version must be ${STORAGE_VERSION}`);
  }
  requireTimestamp(envelope.updatedAt, `${label}.updatedAt`);
}

function validateCardStorageEnvelope(envelope, label) {
  storageSeedObject(envelope, label);
  requireExactKeys(envelope, ["schema", "version", "slots", "updatedAt"], label);
  validateStorageHeader(envelope, CARD_STORAGE_SCHEMA, label);
  if (
    !Array.isArray(envelope.slots)
    || envelope.slots.length !== STORAGE_SLOT_COUNT
  ) {
    throw new Error(`${label}.slots must contain exactly ${STORAGE_SLOT_COUNT} entries`);
  }
  for (const [index, rawSlot] of envelope.slots.entries()) {
    if (rawSlot === null) continue;
    const slot = storageSeedObject(rawSlot, `${label}.slots[${index}]`);
    requireExactKeys(slot, ["payload", "updatedAt"], `${label}.slots[${index}]`);
    requireStoragePayload(slot.payload, `${label}.slots[${index}].payload`);
    requireTimestamp(slot.updatedAt, `${label}.slots[${index}].updatedAt`);
  }
}

function validateResumeStorageEnvelope(envelope, label) {
  storageSeedObject(envelope, label);
  requireExactKeys(envelope, ["schema", "version", "payload", "updatedAt"], label);
  validateStorageHeader(envelope, RESUME_STORAGE_SCHEMA, label);
  requireStoragePayload(envelope.payload, `${label}.payload`);
}

/**
 * Parses one bounded local storage seed without exposing its payload in errors.
 * The original JSON text is retained so Chrome receives the exact validated
 * envelope supplied by the caller rather than a rewritten equivalent.
 */
export function parseStorageSeedJson(text, kind, label = `${kind} storage seed`) {
  if (typeof text !== "string") {
    throw new Error(`${label} must be UTF-8 JSON text`);
  }
  const byteLength = Buffer.byteLength(text, "utf8");
  if (byteLength === 0 || byteLength > MAX_STORAGE_SEED_BYTES) {
    throw new Error(
      `${label} must contain 1 through ${MAX_STORAGE_SEED_BYTES} UTF-8 bytes`,
    );
  }
  let envelope;
  try {
    envelope = JSON.parse(text);
  } catch {
    throw new Error(`${label} is not valid JSON`);
  }
  if (kind === "card") {
    validateCardStorageEnvelope(envelope, label);
    return { key: CARD_STORAGE_KEY, json: text };
  }
  if (kind === "resume") {
    validateResumeStorageEnvelope(envelope, label);
    return { key: RESUME_STORAGE_KEY, json: text };
  }
  throw new Error("storage seed kind must be card or resume");
}

export function cardRoundTripStorageFailures(json) {
  if (typeof json !== "string") {
    return ["authored card write did not create a local-storage record"];
  }
  let envelope;
  try {
    envelope = JSON.parse(json);
    validateCardStorageEnvelope(envelope, "authored card record");
  } catch (error) {
    return [`authored card record is invalid: ${error.message}`];
  }
  const failures = [];
  const occupied = envelope.slots
    .map((slot, index) => slot === null ? null : index)
    .filter((index) => index !== null);
  if (JSON.stringify(occupied) !== "[0]") {
    failures.push(
      `authored card write occupied slots ${JSON.stringify(occupied)} instead of only slot zero`,
    );
  }
  if (envelope.slots[0]?.payload !== CARD_ROUND_TRIP_PAYLOAD_BASE64) {
    failures.push("authored card slot zero does not contain the exact 128-byte fixture payload");
  }
  if (!(envelope.slots[0]?.updatedAt > 0)) {
    failures.push("authored card slot zero has no positive write timestamp");
  }
  if (envelope.updatedAt !== envelope.slots[0]?.updatedAt) {
    failures.push("authored card and slot timestamps do not describe one atomic write");
  }
  return failures;
}

export function resumeRoundTripStorageFailures(json) {
  if (typeof json !== "string") {
    return ["page lifecycle did not create a local resume record"];
  }
  let envelope;
  try {
    envelope = JSON.parse(json);
    validateResumeStorageEnvelope(envelope, "page-lifecycle resume record");
  } catch (error) {
    return [`page-lifecycle resume record is invalid: ${error.message}`];
  }
  const failures = [];
  if (envelope.payload !== CARD_ROUND_TRIP_PAYLOAD_BASE64) {
    failures.push(
      "page-lifecycle resume does not contain the exact authored 128-byte fixture payload",
    );
  }
  if (!(envelope.updatedAt > 0)) {
    failures.push("page-lifecycle resume has no positive write timestamp");
  }
  return failures;
}

async function loadStorageSeed(path, kind) {
  const label = `${kind} storage seed`;
  const metadata = await stat(path).catch((error) => {
    throw new Error(`cannot read ${label} file ${path}: ${error.message}`);
  });
  if (!metadata.isFile()) {
    throw new Error(`${label} path is not a file: ${path}`);
  }
  if (metadata.size === 0 || metadata.size > MAX_STORAGE_SEED_BYTES) {
    throw new Error(
      `${label} file must contain 1 through ${MAX_STORAGE_SEED_BYTES} bytes`,
    );
  }
  const text = await readFile(path, "utf8").catch((error) => {
    throw new Error(`cannot read ${label} file ${path}: ${error.message}`);
  });
  return parseStorageSeedJson(text, kind, label);
}

async function loadStorageSeeds(options) {
  const seeds = {};
  for (const [path, kind] of [
    [options.cardStorageSeed, "card"],
    [options.resumeStorageSeed, "resume"],
  ]) {
    if (!path) continue;
    const seed = await loadStorageSeed(path, kind);
    seeds[seed.key] = seed.json;
  }
  return seeds;
}

export function usage() {
  return `Usage:
  node scripts/browser-harness-smoke.mjs --asset PATH [--asset PATH ...] [options]
  node scripts/browser-harness-smoke.mjs --synthetic-cooked-iso-import [options]

Options:
  --asset PATH       Legally owned BIN/ISO/NSD/NSF file (repeatable)
  --synthetic-cooked-iso-import
                     Verify cooked-ISO discovery with generated one-byte zero payloads
  --replay PATH      Run-length replay JSON; overrides --lid and --frames
  --lid NUMBER       Direct-boot stream id (default: 0x19)
  --frames NUMBER    Number of zero-input frames (default: 120)
  --expect-final-key-count NUMBER
                     Require an exact final retail keyCount
  --expect-final-item-pool-2 NUMBER
                     Require an exact final retail itemPool2 (hex accepted)
  --audit-retail-pbaks
                     Idle Title until all seven naturally reachable PBAKs return
  --audit-isolated-retail-pbak LID
                     Mount dormant Upstream (0x0f) or Temple Ruins (0x1c)
                     through a test-only GAME_STATE_TITLE transition; the
                     production PbakChoose path still selects the recording
  --audit-card-round-trip
                     Author a card save, reload the page, then Load into gameplay
  --audit-direct-bonus-return
                     Direct-boot Tawna Bonus 1, join at its separately proven
                     state-32 boundary, confirm CardC, and verify Title return
  --unlock-all       Enable the temporary all-level/max-lives option
  --seed-card PATH   Seed one exact local c1-virtual-memory-card v1 JSON envelope
  --seed-resume PATH Seed one exact local c1-browser-resume v1 JSON envelope
  --url URL          Local harness URL (default: ${DEFAULT_URL})
  --no-server        Use an already-running harness server
  --chrome PATH      Chrome/Chromium executable
  --screenshot PATH  PNG output (default: target/browser-test-artifacts/smoke.png)
  --video PATH       Per-source-frame H.264 MP4 output; requires --replay
  --video-window NAME:STARTFRAME:ENDFRAME:OUTPUTPATH
                     Repeatable one-pass source-frame H.264/AAC clip window;
                     requires --replay and --chapters
  --chapters PATH    JSON chapter/metadata list for --video or --video-window
  --output-aspect VALUE
                     4:3, 16:9, 21:9, or screen (default: 4:3)
  --render-resolution VALUE
                     native, 720, 1080, 1440, or 2160 (default: native)
  --camera-zoom VALUE
                     100, 85, 70, or 55 (default: 100)
  --smooth-motion    Enable smooth presentation; window capture uses 1000/34 fps
  --extended-world   Render geometry outside the retail camera frustum
  --ffmpeg PATH      ffmpeg executable (default: /usr/bin/ffmpeg)
  --help             Show this help

CRUST_GAME_FILES may supply additional paths separated by the platform path delimiter.
`;
}

export function parseArguments(argv, environment = process.env) {
  const options = {
    assets: (environment.CRUST_GAME_FILES ?? "")
      .split(delimiter)
      .filter(Boolean),
    bootLid: DEFAULT_BOOT_LID,
    frames: DEFAULT_FRAMES,
    framesExplicit: false,
    unlockAll: false,
    url: environment.CRUST_BROWSER_HARNESS_URL ?? DEFAULT_URL,
    startServer: true,
    chrome: environment.CRUST_CHROME_BIN,
    replay: undefined,
    expectFinalKeyCount: undefined,
    expectFinalItemPool2: undefined,
    cardStorageSeed: undefined,
    resumeStorageSeed: undefined,
    screenshot: resolve(
      repositoryRoot,
      "target/browser-test-artifacts/smoke.png",
    ),
    video: undefined,
    videoWindows: [],
    chapters: undefined,
    outputAspect: "4:3",
    renderResolution: "native",
    cameraZoom: "100",
    smoothMotion: false,
    extendedWorld: false,
    ffmpeg: environment.CRUST_FFMPEG_BIN ?? "/usr/bin/ffmpeg",
    syntheticCookedIsoImport: false,
    auditRetailPbaks: false,
    auditIsolatedRetailPbakLid: undefined,
    auditCardRoundTrip: false,
    auditDirectBonusReturn: false,
    help: false,
  };
  let launchArgumentUsed = false;
  let bootLidArgumentUsed = false;
  const presentationArguments = new Set();
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const value = () => {
      index += 1;
      if (index >= argv.length) {
        throw new Error(`${argument} requires a value`);
      }
      return argv[index];
    };
    switch (argument) {
      case "--asset":
        options.assets.push(value());
        break;
      case "--synthetic-cooked-iso-import":
        options.syntheticCookedIsoImport = true;
        break;
      case "--replay":
        options.replay = value();
        launchArgumentUsed = true;
        break;
      case "--lid":
        options.bootLid = parseWholeNumber(value(), "--lid", 0xff);
        bootLidArgumentUsed = true;
        launchArgumentUsed = true;
        break;
      case "--frames":
        options.frames = parseWholeNumber(value(), "--frames", 1_000_000);
        if (options.frames === 0) throw new Error("--frames must be at least 1");
        options.framesExplicit = true;
        launchArgumentUsed = true;
        break;
      case "--expect-final-key-count":
        if (options.expectFinalKeyCount !== undefined) {
          throw new Error("--expect-final-key-count may be supplied only once");
        }
        options.expectFinalKeyCount = parseWholeNumber(
          value(),
          "--expect-final-key-count",
          0xffff_ffff,
        );
        launchArgumentUsed = true;
        break;
      case "--expect-final-item-pool-2":
        if (options.expectFinalItemPool2 !== undefined) {
          throw new Error("--expect-final-item-pool-2 may be supplied only once");
        }
        options.expectFinalItemPool2 = parseWholeNumber(
          value(),
          "--expect-final-item-pool-2",
          0xffff_ffff,
        );
        launchArgumentUsed = true;
        break;
      case "--audit-retail-pbaks":
        options.auditRetailPbaks = true;
        launchArgumentUsed = true;
        break;
      case "--audit-isolated-retail-pbak":
        if (options.auditIsolatedRetailPbakLid !== undefined) {
          throw new Error("--audit-isolated-retail-pbak may be supplied only once");
        }
        options.auditIsolatedRetailPbakLid = parseWholeNumber(
          value(),
          "--audit-isolated-retail-pbak",
          0xff,
        );
        launchArgumentUsed = true;
        break;
      case "--audit-card-round-trip":
        options.auditCardRoundTrip = true;
        launchArgumentUsed = true;
        break;
      case "--audit-direct-bonus-return":
        options.auditDirectBonusReturn = true;
        launchArgumentUsed = true;
        break;
      case "--unlock-all":
        options.unlockAll = true;
        launchArgumentUsed = true;
        break;
      case "--seed-card":
        if (options.cardStorageSeed !== undefined) {
          throw new Error("--seed-card may be supplied only once");
        }
        options.cardStorageSeed = value();
        launchArgumentUsed = true;
        break;
      case "--seed-resume":
        if (options.resumeStorageSeed !== undefined) {
          throw new Error("--seed-resume may be supplied only once");
        }
        options.resumeStorageSeed = value();
        launchArgumentUsed = true;
        break;
      case "--url":
        options.url = value();
        break;
      case "--no-server":
        options.startServer = false;
        break;
      case "--chrome":
        options.chrome = value();
        break;
      case "--screenshot":
        options.screenshot = resolve(value());
        break;
      case "--video":
        if (options.video !== undefined) {
          throw new Error("--video may be supplied only once");
        }
        options.video = resolve(value());
        break;
      case "--video-window":
        options.videoWindows.push(parseVideoWindow(value()));
        break;
      case "--chapters":
        if (options.chapters !== undefined) {
          throw new Error("--chapters may be supplied only once");
        }
        options.chapters = resolve(value());
        break;
      case "--output-aspect": {
        if (presentationArguments.has(argument)) {
          throw new Error(`${argument} may be supplied only once`);
        }
        const aspect = value();
        if (!OUTPUT_ASPECT_VALUES.has(aspect)) {
          throw new Error("--output-aspect must be 4:3, 16:9, 21:9, or screen");
        }
        presentationArguments.add(argument);
        options.outputAspect = aspect;
        break;
      }
      case "--render-resolution": {
        if (presentationArguments.has(argument)) {
          throw new Error(`${argument} may be supplied only once`);
        }
        const resolution = value();
        if (!RENDER_RESOLUTION_VALUES.has(resolution)) {
          throw new Error(
            "--render-resolution must be native, 720, 1080, 1440, or 2160",
          );
        }
        presentationArguments.add(argument);
        options.renderResolution = resolution;
        break;
      }
      case "--camera-zoom": {
        if (presentationArguments.has(argument)) {
          throw new Error(`${argument} may be supplied only once`);
        }
        const zoom = value();
        if (!CAMERA_ZOOM_VALUES.has(zoom)) {
          throw new Error("--camera-zoom must be 100, 85, 70, or 55");
        }
        presentationArguments.add(argument);
        options.cameraZoom = zoom;
        break;
      }
      case "--smooth-motion":
        options.smoothMotion = true;
        break;
      case "--extended-world":
        options.extendedWorld = true;
        break;
      case "--ffmpeg":
        options.ffmpeg = resolve(value());
        break;
      case "--help":
      case "-h":
        options.help = true;
        break;
      default:
        throw new Error(`unknown argument: ${argument}`);
    }
  }
  if (
    options.syntheticCookedIsoImport
    && (options.assets.length > 0 || launchArgumentUsed)
  ) {
    throw new Error(
      "--synthetic-cooked-iso-import cannot be combined with assets or launch/replay options",
    );
  }
  if (options.video !== undefined && options.videoWindows.length > 0) {
    throw new Error("--video and --video-window cannot be combined");
  }
  const captureRequested =
    options.video !== undefined || options.videoWindows.length > 0;
  if (captureRequested && options.replay === undefined) {
    throw new Error("video capture requires --replay");
  }
  if (options.chapters !== undefined && !captureRequested) {
    throw new Error("--chapters requires --video or --video-window");
  }
  if (captureRequested && options.chapters === undefined) {
    throw new Error("video capture requires --chapters");
  }
  const windowNames = new Set();
  const windowPaths = new Set();
  for (const window of options.videoWindows) {
    if (windowNames.has(window.name)) {
      throw new Error(`duplicate --video-window name: ${window.name}`);
    }
    if (windowPaths.has(window.outputPath)) {
      throw new Error(`duplicate --video-window output path: ${window.outputPath}`);
    }
    windowNames.add(window.name);
    windowPaths.add(window.outputPath);
  }
  if (options.videoWindows.length > 0 && options.framesExplicit) {
    const lastWindowEnd = Math.max(
      ...options.videoWindows.map((window) => window.endFrame),
    );
    if (options.frames < lastWindowEnd) {
      throw new Error(
        `--frames ${options.frames} stops before the final video window closes at ${lastWindowEnd}`,
      );
    }
  }
  if (options.auditRetailPbaks && options.replay !== undefined) {
    throw new Error("--audit-retail-pbaks cannot be combined with --replay");
  }
  if (
    options.auditDirectBonusReturn
    && (
      options.auditRetailPbaks
      || options.auditIsolatedRetailPbakLid !== undefined
      || options.auditCardRoundTrip
      || options.replay !== undefined
      || options.unlockAll
      || options.cardStorageSeed !== undefined
      || options.resumeStorageSeed !== undefined
    )
  ) {
    throw new Error(
      "--audit-direct-bonus-return cannot be combined with replay, other audits, all-level mode, or storage seeds",
    );
  }
  if (options.auditDirectBonusReturn) {
    if (bootLidArgumentUsed && options.bootLid !== DIRECT_BONUS_AUDIT_LID) {
      throw new Error(
        "--audit-direct-bonus-return requires Tawna Bonus 1 boot LID 0x24",
      );
    }
    options.bootLid = DIRECT_BONUS_AUDIT_LID;
  }
  if (
    options.auditIsolatedRetailPbakLid !== undefined
    && options.replay !== undefined
  ) {
    throw new Error(
      "--audit-isolated-retail-pbak cannot be combined with --replay",
    );
  }
  if (
    options.auditRetailPbaks
    && options.auditIsolatedRetailPbakLid !== undefined
  ) {
    throw new Error(
      "--audit-retail-pbaks cannot be combined with --audit-isolated-retail-pbak",
    );
  }
  if (
    options.auditCardRoundTrip
    && (
      options.auditRetailPbaks
      || options.auditIsolatedRetailPbakLid !== undefined
      || options.replay !== undefined
      || options.unlockAll
      || options.cardStorageSeed !== undefined
      || options.resumeStorageSeed !== undefined
    )
  ) {
    throw new Error(
      "--audit-card-round-trip cannot be combined with replay, PBAK audits, all-level mode, or storage seeds",
    );
  }
  if (options.auditCardRoundTrip && options.bootLid !== DEFAULT_BOOT_LID) {
    throw new Error("--audit-card-round-trip requires Title boot LID 0x19");
  }
  if (options.auditRetailPbaks && options.bootLid !== DEFAULT_BOOT_LID) {
    throw new Error("--audit-retail-pbaks requires Title boot LID 0x19");
  }
  if (
    options.auditIsolatedRetailPbakLid !== undefined
    && options.bootLid !== DEFAULT_BOOT_LID
  ) {
    throw new Error(
      "--audit-isolated-retail-pbak requires Title boot LID 0x19",
    );
  }
  if (
    options.auditIsolatedRetailPbakLid !== undefined
    && !ISOLATED_TITLE_PBAK_BY_LID.has(options.auditIsolatedRetailPbakLid)
  ) {
    throw new Error(
      "--audit-isolated-retail-pbak accepts only Upstream 0x0f or Temple Ruins 0x1c",
    );
  }
  options.assets = options.assets.map((path) => resolve(path));
  if (options.replay) options.replay = resolve(options.replay);
  if (options.cardStorageSeed) {
    options.cardStorageSeed = resolve(options.cardStorageSeed);
  }
  if (options.resumeStorageSeed) {
    options.resumeStorageSeed = resolve(options.resumeStorageSeed);
  }
  const url = new URL(options.url);
  // WHATWG URL retains brackets around IPv6 literals in `hostname`, unlike
  // IPv4 and DNS hostnames. Normalize only that syntax before applying the
  // exact loopback allowlist; no other IPv6 address is accepted.
  const hostname = url.hostname === "[::1]" ? "::1" : url.hostname;
  if (
    url.protocol !== "http:" ||
    !["127.0.0.1", "localhost", "::1"].includes(hostname)
  ) {
    throw new Error("--url must be a loopback HTTP URL");
  }
  if (url.pathname !== "/") {
    throw new Error("--url must point to the harness root");
  }
  options.url = url.href;
  return options;
}

const LIVE_OBJECT_UNSIGNED_FIELDS = new Map([
  ["arenaSlot", 0xff],
  ["arenaGeneration", 0xffff_ffff],
  ["vm", 0xffff],
  ["entityId", 0xffff],
  ["entityGroup", 0xffff],
  ["programEid", 0xffff_ffff],
  ["executable", 0xff],
  ["spawnSubtype", 0xff],
  ["subtype", 0xffff_ffff],
  ["state", 0xffff],
  ["pc", 0xffff_ffff],
  ["zoneEid", 0xffff_ffff],
  ["register65", 0xffff_ffff],
  ["statusA", 0xffff_ffff],
  ["statusB", 0xffff_ffff],
  ["statusC", 0xffff_ffff],
  ["stateFlags", 0xffff_ffff],
  ["colliderEntityId", 0xffff],
  ["colliderExecutable", 0xff],
  ["colliderSubtype", 0xffff_ffff],
  ["colliderState", 0xffff],
]);
const LIVE_OBJECT_SIGNED_FIELDS = new Set([
  "x", "y", "z",
  "velocityX", "velocityY", "velocityZ",
  "rotationY", "rotationX", "rotationZ",
  "minX", "maxX", "minY", "maxY", "minZ", "maxZ",
  "minVelocityX", "maxVelocityX",
  "minVelocityY", "maxVelocityY",
  "minVelocityZ", "maxVelocityZ",
]);
const LIVE_OBJECT_BOOLEAN_FIELDS = new Set([
  "player", "faulted", "hasCollider", "hasFrameBound",
]);

function normalizeLiveObjectExpectation(raw, label) {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error(`${label} must be an object`);
  }
  if (Object.keys(raw).length === 0) {
    throw new Error(`${label} must contain at least one predicate`);
  }
  const expectation = {};
  for (const [name, value] of Object.entries(raw)) {
    if (LIVE_OBJECT_UNSIGNED_FIELDS.has(name)) {
      expectation[name] = parseWholeNumber(
        value,
        `${label}.${name}`,
        LIVE_OBJECT_UNSIGNED_FIELDS.get(name),
      );
    } else if (LIVE_OBJECT_SIGNED_FIELDS.has(name)) {
      expectation[name] = parseSignedWholeNumber(value, `${label}.${name}`);
    } else if (LIVE_OBJECT_BOOLEAN_FIELDS.has(name)) {
      if (typeof value !== "boolean") {
        throw new Error(`${label}.${name} must be a boolean`);
      }
      expectation[name] = value;
    } else {
      throw new Error(`${label}.${name} is not a supported live-object predicate`);
    }
  }
  for (const axis of ["X", "Y", "Z"]) {
    for (const prefix of ["", "Velocity"]) {
      const minimum = `min${prefix}${axis}`;
      const maximum = `max${prefix}${axis}`;
      if (
        expectation[minimum] !== undefined &&
        expectation[maximum] !== undefined &&
        expectation[minimum] > expectation[maximum]
      ) {
        throw new Error(`${label}.${minimum} must not exceed ${label}.${maximum}`);
      }
    }
  }
  return expectation;
}

function normalizeExpectation(raw, label) {
  if (raw === undefined) return {};
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error(`${label} must be an object`);
  }
  const expectation = {};
  for (const [name, value] of Object.entries(raw)) {
    if (name === "liveObject") {
      expectation.liveObject = normalizeLiveObjectExpectation(
        value,
        `${label}.liveObject`,
      );
      continue;
    }
    if (
      ![
        "mountedLid",
        "currentLid",
        "titleState",
        "retailFrame",
        "retailDrawCount",
        "retailProcessDrawCount",
        "retailRandomSeed",
        "retailRandomSeedB",
        "retailHardRestarts",
        "retailLoadStates",
        "retailDeathCameraFrames",
        "paused",
        "lifeCount",
        "playerLifeCount",
        "gemCount",
        "keyCount",
        "itemPool1",
        "itemPool2",
        "minFrame",
        "minRetailFrame",
        "minRetailExecutions",
      ].includes(name)
    ) {
      throw new Error(`${label}.${name} is not a supported expectation`);
    }
    if (name === "paused") {
      if (typeof value !== "boolean") {
        throw new Error(`${label}.${name} must be a boolean`);
      }
      expectation[name] = value;
      continue;
    }
    expectation[name] = parseWholeNumber(
      value,
      `${label}.${name}`,
      Number.MAX_SAFE_INTEGER,
    );
  }
  return expectation;
}

function normalizeReplayCondition(raw, label) {
  if (raw === undefined) return undefined;
  const condition = normalizeExpectation(raw, label);
  if (Object.keys(condition).length === 0) {
    throw new Error(`${label} must contain at least one expectation`);
  }
  const unsupported = Object.keys(condition).filter(
    (name) => !["currentLid", "mountedLid"].includes(name),
  );
  if (unsupported.length > 0) {
    throw new Error(
      `${label} supports only currentLid and mountedLid; received ${unsupported.join(", ")}`,
    );
  }
  return condition;
}

export function normalizeReplay(raw, fallback = {}) {
  const replay = raw ?? {
    schema: 1,
    bootLid: fallback.bootLid ?? DEFAULT_BOOT_LID,
    unlockAll: fallback.unlockAll ?? false,
    segments: [{ frames: fallback.frames ?? DEFAULT_FRAMES, held: 0 }],
  };
  if (!replay || typeof replay !== "object" || Array.isArray(replay)) {
    throw new Error("replay must be an object");
  }
  if (replay.schema !== 1) throw new Error("replay.schema must equal 1");
  if (!Array.isArray(replay.segments) || replay.segments.length === 0) {
    throw new Error("replay.segments must be a non-empty array");
  }
  const normalized = {
    schema: 1,
    bootLid: parseWholeNumber(replay.bootLid, "replay.bootLid", 0xff),
    unlockAll: replay.unlockAll ?? fallback.unlockAll ?? false,
    traceFromSegment:
      replay.traceFromSegment === undefined
        ? undefined
        : parseWholeNumber(
            replay.traceFromSegment,
            "replay.traceFromSegment",
            1_000_000,
          ),
    settleFrames: parseWholeNumber(
      replay.settleFrames ?? 0,
      "replay.settleFrames",
      10_000,
    ),
    segments: [],
    expect: normalizeExpectation(replay.expect, "replay.expect"),
  };
  if (typeof normalized.unlockAll !== "boolean") {
    throw new Error("replay.unlockAll must be a boolean");
  }
  if (normalized.traceFromSegment === 0) {
    throw new Error("replay.traceFromSegment must be at least 1");
  }
  let totalFrames = 0;
  for (const [index, segment] of replay.segments.entries()) {
    if (!segment || typeof segment !== "object" || Array.isArray(segment)) {
      throw new Error(`replay.segments[${index}] must be an object`);
    }
    const frames = parseWholeNumber(
      segment.frames,
      `replay.segments[${index}].frames`,
      1_000_000,
    );
    if (frames === 0) {
      throw new Error(`replay.segments[${index}].frames must be at least 1`);
    }
    totalFrames += frames;
    const settleFrames = parseWholeNumber(
      segment.settleFrames ?? 0,
      `replay.segments[${index}].settleFrames`,
      10_000,
    );
    totalFrames += settleFrames;
    if (totalFrames + normalized.settleFrames > 5_000_000) {
      throw new Error("replay may not exceed 5,000,000 frames");
    }
    const inputKind = normalizeReplayInputKind(
      segment.inputKind,
      `replay.segments[${index}].inputKind`,
    );
    const heldMaximum =
      inputKind === PHYSICAL_INPUT_KIND ? 0xffff : 0xffff_ffff;
    const normalizedSegment = {
      frames,
      inputKind,
      held: parseWholeNumber(
        segment.held,
        `replay.segments[${index}].held`,
        heldMaximum,
      ),
      expect: normalizeExpectation(
        segment.expect,
        `replay.segments[${index}].expect`,
      ),
      while: normalizeReplayCondition(
        segment.while,
        `replay.segments[${index}].while`,
      ),
      settleFrames,
      settleHeld: parseWholeNumber(
        segment.settleHeld ?? 0,
        `replay.segments[${index}].settleHeld`,
        heldMaximum,
      ),
    };
    if (inputKind === SNAPSHOT_INPUT_KIND) {
      if (frames !== 1) {
        throw new Error(`replay.segments[${index}] snapshot input must have exactly one frame`);
      }
      if (settleFrames !== 0) {
        throw new Error(`replay.segments[${index}] snapshot input cannot use settle frames`);
      }
      normalizedSegment.tapped = parseWholeNumber(
        segment.tapped,
        `replay.segments[${index}].tapped`,
        0xffff_ffff,
      );
      normalizedSegment.heldPrevious = parseWholeNumber(
        segment.heldPrevious,
        `replay.segments[${index}].heldPrevious`,
        0xffff_ffff,
      );
      normalizedSegment.tappedPrevious = parseWholeNumber(
        segment.tappedPrevious,
        `replay.segments[${index}].tappedPrevious`,
        0xffff_ffff,
      );
      normalizedSegment.heldPrevious2 = parseWholeNumber(
        segment.heldPrevious2,
        `replay.segments[${index}].heldPrevious2`,
        0xffff_ffff,
      );
      normalizedSegment.beforeHeld = parseWholeNumber(
        segment.beforeHeld ?? normalizedSegment.held,
        `replay.segments[${index}].beforeHeld`,
        0xffff_ffff,
      );
      normalizedSegment.beforeTapped = parseWholeNumber(
        segment.beforeTapped ?? normalizedSegment.tapped,
        `replay.segments[${index}].beforeTapped`,
        0xffff_ffff,
      );
      normalizedSegment.beforeHeldPrevious = parseWholeNumber(
        segment.beforeHeldPrevious ?? normalizedSegment.heldPrevious,
        `replay.segments[${index}].beforeHeldPrevious`,
        0xffff_ffff,
      );
      normalizedSegment.beforeTappedPrevious = parseWholeNumber(
        segment.beforeTappedPrevious ?? normalizedSegment.tappedPrevious,
        `replay.segments[${index}].beforeTappedPrevious`,
        0xffff_ffff,
      );
      normalizedSegment.beforeHeldPrevious2 = parseWholeNumber(
        segment.beforeHeldPrevious2 ?? normalizedSegment.heldPrevious2,
        `replay.segments[${index}].beforeHeldPrevious2`,
        0xffff_ffff,
      );
      if (segment.frameStamp !== undefined) {
        normalizedSegment.frameStamp = parseWholeNumber(
          segment.frameStamp,
          `replay.segments[${index}].frameStamp`,
          0xffff_ffff,
        );
      }
      if (
        (segment.ticksCurrentFrame === undefined)
        !== (segment.ticksPerFrame === undefined)
      ) {
        throw new Error(
          `replay.segments[${index}] must provide both ticksCurrentFrame and ticksPerFrame`,
        );
      }
      if (segment.ticksCurrentFrame !== undefined) {
        normalizedSegment.ticksCurrentFrame = parseWholeNumber(
          segment.ticksCurrentFrame,
          `replay.segments[${index}].ticksCurrentFrame`,
          0x7fff_ffff,
        );
        normalizedSegment.ticksPerFrame = parseWholeNumber(
          segment.ticksPerFrame,
          `replay.segments[${index}].ticksPerFrame`,
          0x7fff_ffff,
        );
        if (normalizedSegment.ticksPerFrame === 0) {
          throw new Error(`replay.segments[${index}].ticksPerFrame must be positive`);
        }
      }
    }
    normalized.segments.push(normalizedSegment);
  }
  normalized.totalFrames = normalized.segments.reduce(
    (sum, segment) => sum + segment.frames,
    0,
  );
  if (replay.composition !== undefined) {
    const composition = replay.composition;
    if (!composition || typeof composition !== "object" || Array.isArray(composition)) {
      throw new Error("replay.composition must be an object");
    }
    if (!Array.isArray(composition.phaseIds) || composition.phaseIds.length === 0) {
      throw new Error("replay.composition.phaseIds must be a non-empty array");
    }
    if (!Array.isArray(composition.phases)) {
      throw new Error("replay.composition.phases must be an array");
    }
    if (composition.phases.length !== composition.phaseIds.length) {
      throw new Error("replay.composition phases and phaseIds must have equal lengths");
    }
    let previousLastSegment = 0;
    const phases = composition.phases.map((phase, index) => {
      const label = `replay.composition.phases[${index}]`;
      if (!phase || typeof phase !== "object" || Array.isArray(phase)) {
        throw new Error(`${label} must be an object`);
      }
      const id = phase.id;
      if (typeof id !== "string" || id.length === 0 || id !== composition.phaseIds[index]) {
        throw new Error(`${label}.id must match composition.phaseIds[${index}]`);
      }
      const firstSegment = parseWholeNumber(
        phase.firstSegment,
        `${label}.firstSegment`,
        normalized.segments.length,
      );
      const lastSegment = parseWholeNumber(
        phase.lastSegment,
        `${label}.lastSegment`,
        normalized.segments.length,
      );
      if (firstSegment !== previousLastSegment + 1 || lastSegment < firstSegment) {
        throw new Error(`${label} must describe the next contiguous segment range`);
      }
      previousLastSegment = lastSegment;
      return { id, firstSegment, lastSegment };
    });
    if (previousLastSegment !== normalized.segments.length) {
      throw new Error("replay.composition.phases must cover every replay segment");
    }
    normalized.composition = { phaseIds: [...composition.phaseIds], phases };
  }
  if (
    normalized.traceFromSegment !== undefined
    && normalized.traceFromSegment > normalized.segments.length
  ) {
    throw new Error(
      "replay.traceFromSegment must not exceed replay.segments.length",
    );
  }
  normalized.maximumFrames = totalFrames + normalized.settleFrames;
  return normalized;
}

export function applyTerminalProgressionRequirements(
  replay,
  {
    expectFinalKeyCount,
    expectFinalItemPool2,
  } = {},
) {
  if (!replay || typeof replay !== "object" || Array.isArray(replay)) {
    throw new Error("normalized replay must be an object");
  }
  const requirements = [
    ["keyCount", expectFinalKeyCount, "--expect-final-key-count"],
    ["itemPool2", expectFinalItemPool2, "--expect-final-item-pool-2"],
  ];
  if (requirements.every(([, value]) => value === undefined)) return replay;

  const expect = { ...(replay.expect ?? {}) };
  for (const [field, rawValue, option] of requirements) {
    if (rawValue === undefined) continue;
    const value = parseWholeNumber(rawValue, option, 0xffff_ffff);
    if (expect[field] !== undefined && expect[field] !== value) {
      throw new Error(
        `${option}=${value} conflicts with replay.expect.${field}=${expect[field]}`,
      );
    }
    expect[field] = value;
  }
  return { ...replay, expect };
}

export function snapshotFailures(snapshot) {
  const failures = [];
  if (snapshot.bootstrap === "failed") {
    failures.push(`bootstrap state is ${JSON.stringify(snapshot.bootstrap)}`);
  }
  if (snapshot.runtimeState === "error") {
    failures.push(`runtime state is error: ${snapshot.assetMessage}`);
  }
  if (snapshot.harness?.lastError != null) {
    failures.push(`harness error: ${snapshot.harness.lastError}`);
  }
  if (snapshot.debug?.retailRuntimeError != null) {
    failures.push(`retail runtime error: ${snapshot.debug.retailRuntimeError}`);
  }
  if (snapshot.debug?.retailRuntimeWarning != null) {
    failures.push(`retail runtime warning: ${snapshot.debug.retailRuntimeWarning}`);
  }
  for (const [name, value] of [
    ["WebGL error", snapshot.debug?.glError],
    ["faulted retail objects", snapshot.debug?.retailFaultedObjects],
    ["retail execution errors", snapshot.debug?.retailExecutionErrors],
    ["retail zone-event failures", snapshot.debug?.retailZoneEventFailures],
  ]) {
    if (typeof value === "number" && value !== 0) {
      failures.push(`${name}: ${value}`);
    }
  }
  if ((snapshot.consoleErrors ?? []).length > 0) {
    failures.push(`window console errors: ${snapshot.consoleErrors.join(" | ")}`);
  }
  const faultLines = (snapshot.runtimeLog ?? "")
    .split("\n")
    .filter((line) => line.startsWith("! "));
  if (faultLines.length > 0) {
    failures.push(`runtime fault log: ${faultLines.join(" | ")}`);
  }
  return failures;
}

export function syntheticCookedIsoImportFailures(snapshot, evidence) {
  const failures = [...snapshotFailures(snapshot)];
  if (snapshot.runtimeState !== "idle") {
    failures.push(
      `runtime state: expected "idle", received ${JSON.stringify(snapshot.runtimeState)}`,
    );
  }
  if (snapshot.fileCount !== 88) {
    failures.push(
      `file count: expected 88, received ${JSON.stringify(snapshot.fileCount)}`,
    );
  }
  if (snapshot.pairCount !== 44) {
    failures.push(
      `pair count: expected 44, received ${JSON.stringify(snapshot.pairCount)}`,
    );
  }
  if (snapshot.launchDisabled !== false) {
    failures.push("launch control did not become available after exact catalog import");
  }
  if (snapshot.progressHidden !== true) {
    failures.push("import progress remained visible after cooked-ISO discovery");
  }
  if (
    snapshot.assetMessage
    !== "Full set mounted: 43 playable pairs plus the Cave archive."
  ) {
    failures.push(
      `asset summary: received ${JSON.stringify(snapshot.assetMessage)}`,
    );
  }
  const mountMessage =
    "Mounted 88 streams from ISO 2048 without uploading it.";
  if (!snapshot.runtimeLog?.includes(mountMessage)) {
    failures.push(`runtime log is missing ${JSON.stringify(mountMessage)}`);
  }

  const expectedRanges = expectedSyntheticCookedIsoBlobRanges();
  if (
    JSON.stringify(evidence?.blobRanges)
    !== JSON.stringify(expectedRanges)
  ) {
    failures.push(
      "Blob.slice ranges did not match raw-first detection plus six bounded cooked-sector reads: "
      + JSON.stringify(evidence?.blobRanges),
    );
  }
  const expectedArrayBufferSizes = expectedRanges.map(
    ({ start, end }) => end - start,
  );
  if (
    JSON.stringify(evidence?.arrayBufferSizes)
    !== JSON.stringify(expectedArrayBufferSizes)
  ) {
    failures.push(
      "Blob.arrayBuffer reads did not match the bounded slices: "
      + JSON.stringify(evidence?.arrayBufferSizes),
    );
  }
  if (!Array.isArray(evidence?.networkRequests)) {
    failures.push("post-selection network-request evidence is unavailable");
  } else if (evidence.networkRequests.length > 0) {
    failures.push(
      "disc selection caused network activity: "
      + JSON.stringify(evidence.networkRequests),
    );
  }
  return failures;
}

function sameLiveObjectHandle(left, right) {
  return Boolean(
    left &&
    right &&
    left.arenaSlot === right.arenaSlot &&
    left.arenaGeneration === right.arenaGeneration &&
    left.vm === right.vm
  );
}

function liveObjectPredicateValue(object, name, objects) {
  const direct = {
    entityId: object.entityId,
    entityGroup: object.entityGroup,
    programEid: object.programEid,
    executable: object.executable,
    spawnSubtype: object.spawnSubtype,
    subtype: object.subtype,
    state: object.state,
    pc: object.pc,
    zoneEid: object.zoneEid,
    register65: object.register65,
    player: object.player,
    faulted: object.faulted,
    hasCollider: object.collider != null,
    hasFrameBound: object.frameBound != null,
    x: object.translation?.x,
    y: object.translation?.y,
    z: object.translation?.z,
    velocityX: object.velocity?.x,
    velocityY: object.velocity?.y,
    velocityZ: object.velocity?.z,
    rotationY: object.rotationYxz?.y,
    rotationX: object.rotationYxz?.x,
    rotationZ: object.rotationYxz?.z,
    statusA: object.status?.a,
    statusB: object.status?.b,
    statusC: object.status?.c,
    stateFlags: object.status?.stateFlags,
  };
  if (name === "arenaSlot") return object.handle?.arenaSlot;
  if (name === "arenaGeneration") return object.handle?.arenaGeneration;
  if (name === "vm") return object.handle?.vm;
  if (Object.hasOwn(direct, name)) return direct[name];
  if (/^(min|max)(X|Y|Z)$/.test(name)) {
    return object.translation?.[name.at(-1).toLowerCase()];
  }
  if (/^(min|max)Velocity(X|Y|Z)$/.test(name)) {
    return object.velocity?.[name.at(-1).toLowerCase()];
  }
  const collider = objects.find((candidate) =>
    sameLiveObjectHandle(candidate.handle, object.collider)
  );
  switch (name) {
    case "colliderEntityId":
      return collider?.entityId;
    case "colliderExecutable":
      return collider?.executable;
    case "colliderSubtype":
      return collider?.subtype;
    case "colliderState":
      return collider?.state;
    default:
      return undefined;
  }
}

function liveObjectMatches(expectation, object, objects) {
  return Object.entries(expectation).every(([name, expected]) => {
    const actual = liveObjectPredicateValue(object, name, objects);
    if (name.startsWith("min")) return actual >= expected;
    if (name.startsWith("max")) return actual <= expected;
    return actual === expected;
  });
}

export function liveObjectExpectationFailures(expectation, snapshot) {
  const objects = snapshot.debug?.browserTestObjects;
  if (!Array.isArray(objects)) {
    return ["browser-test live object snapshots are unavailable"];
  }
  if (objects.some((object) => liveObjectMatches(expectation, object, objects))) {
    return [];
  }
  const observed = objects.slice(0, 24).map((object) => ({
    handle: object.handle,
    entityId: object.entityId,
    executable: object.executable,
    subtype: object.subtype,
    state: object.state,
    zoneEid: object.zoneEid,
    translation: object.translation,
    velocity: object.velocity,
    player: object.player,
    collider: object.collider,
  }));
  return [
    `liveObject: no object matched ${JSON.stringify(expectation)}; ` +
      `observed ${objects.length}: ${JSON.stringify(observed)}`,
  ];
}

export function expectationFailures(expectation, snapshot) {
  const failures = [];
  const debug = snapshot.debug ?? {};
  for (const name of [
    "mountedLid",
    "currentLid",
    "titleState",
    "retailFrame",
    "retailDrawCount",
    "retailProcessDrawCount",
    "retailRandomSeed",
    "retailRandomSeedB",
    "retailHardRestarts",
    "retailLoadStates",
    "retailDeathCameraFrames",
    "paused",
    "playerLifeCount",
  ]) {
    if (expectation[name] !== undefined && debug[name] !== expectation[name]) {
      failures.push(
        `${name}: expected ${expectation[name]}, received ${JSON.stringify(debug[name])}`,
      );
    }
  }
  for (const [name, debugName] of [
    ["minFrame", "frame"],
    ["minRetailFrame", "retailFrame"],
    ["minRetailExecutions", "retailExecutions"],
  ]) {
    if (
      expectation[name] !== undefined &&
      !(debug[debugName] >= expectation[name])
    ) {
      failures.push(
        `${debugName}: expected at least ${expectation[name]}, received ` +
          JSON.stringify(debug[debugName]),
      );
    }
  }
  for (const name of [
    "lifeCount",
    "gemCount",
    "keyCount",
    "itemPool1",
    "itemPool2",
  ]) {
    if (
      expectation[name] !== undefined
      && debug.browserTestGlobals?.[name] !== expectation[name]
    ) {
      failures.push(
        `${name}: expected ${expectation[name]}, received ` +
          JSON.stringify(debug.browserTestGlobals?.[name]),
      );
    }
  }
  if (expectation.liveObject !== undefined) {
    failures.push(
      ...liveObjectExpectationFailures(expectation.liveObject, snapshot),
    );
  }
  return failures;
}

export function replayLidConditionMatches(
  condition,
  currentLid,
  mountedLid,
) {
  if (condition === undefined) return true;
  return expectationFailures(condition, {
    debug: { currentLid, mountedLid },
  }).length === 0;
}

export function replayLidConditionKnown(condition, currentLid, mountedLid) {
  if (condition === undefined) return true;
  return (
    (condition.currentLid === undefined || Number.isSafeInteger(currentLid))
    && (condition.mountedLid === undefined || Number.isSafeInteger(mountedLid))
  );
}

export function allLevelsFailures(
  snapshot,
  { requireStartingLives = false, requireLivePlayer = false } = {},
) {
  const globals = snapshot.debug?.browserTestGlobals;
  if (!globals) return ["browser-test all-level globals are unavailable"];
  const failures = [];
  if (globals.allLevels !== true) {
    failures.push(`all-level mode is ${JSON.stringify(globals.allLevels)}`);
  }
  if (globals.initialLifeCount !== ALL_LEVELS_MAX_LIVES) {
    failures.push(
      `initialLifeCount: expected ${ALL_LEVELS_MAX_LIVES}, received ` +
        JSON.stringify(globals.initialLifeCount),
    );
  }
  if (
    !Number.isSafeInteger(globals.lifeCount) ||
    globals.lifeCount < 0 ||
    globals.lifeCount > ALL_LEVELS_MAX_LIVES ||
    globals.lifeCount % 0x100 !== 0
  ) {
    failures.push(
      `lifeCount: expected an aligned 24.8 value from 0 through ${ALL_LEVELS_MAX_LIVES}, received ` +
        JSON.stringify(globals.lifeCount),
    );
  } else if (
    requireStartingLives &&
    globals.lifeCount !== ALL_LEVELS_MAX_LIVES
  ) {
    failures.push(
      `lifeCount: expected ${ALL_LEVELS_MAX_LIVES} at launch, received ` +
        JSON.stringify(globals.lifeCount),
    );
  }
  const playerLifeCount = snapshot.debug?.playerLifeCount;
  if (playerLifeCount == null) {
    if (requireLivePlayer) {
      failures.push("authoritative live-player life count is unavailable");
    }
  } else if (
    !Number.isSafeInteger(playerLifeCount) ||
    playerLifeCount < 0 ||
    playerLifeCount > ALL_LEVELS_MAX_LIVES ||
    playerLifeCount % 0x100 !== 0
  ) {
    failures.push(
      `playerLifeCount: expected an aligned 24.8 value from 0 through ${ALL_LEVELS_MAX_LIVES}, received ` +
        JSON.stringify(playerLifeCount),
    );
  } else if (requireStartingLives && playerLifeCount !== ALL_LEVELS_MAX_LIVES) {
    failures.push(
      `playerLifeCount: expected ${ALL_LEVELS_MAX_LIVES} at launch, received ` +
        JSON.stringify(playerLifeCount),
    );
  }
  if (globals.levelsUnlocked !== ALL_LEVELS_UNLOCK_GATE) {
    failures.push(
      `levelsUnlocked: expected ${ALL_LEVELS_UNLOCK_GATE}, received ` +
        JSON.stringify(globals.levelsUnlocked),
    );
  }
  if (
    (globals.itemPool2 & ALL_LEVELS_SECRET_PATH_BITS) !==
    ALL_LEVELS_SECRET_PATH_BITS
  ) {
    failures.push(
      `itemPool2 is missing secret-path bits 0x${ALL_LEVELS_SECRET_PATH_BITS.toString(16)}: ` +
        JSON.stringify(globals.itemPool2),
    );
  }
  return failures;
}

export function allLevelsStorageFailures(expectedSeeds, observedStorage) {
  if (!expectedSeeds || typeof expectedSeeds !== "object" || Array.isArray(expectedSeeds)) {
    throw new Error("expected storage seeds must be an object");
  }
  if (!observedStorage || typeof observedStorage !== "object" || Array.isArray(observedStorage)) {
    throw new Error("observed storage must be an object");
  }
  const failures = [];
  for (const key of STORAGE_KEYS) {
    const expected = Object.hasOwn(expectedSeeds, key) ? expectedSeeds[key] : null;
    const observed = Object.hasOwn(observedStorage, key)
      ? observedStorage[key]
      : null;
    if (observed !== expected) {
      const label = key === CARD_STORAGE_KEY ? "virtual card" : "browser resume";
      failures.push(
        `${label}: expected ${expected === null ? "no value" : "the exact seed"}, ` +
          `received ${observed === null ? "no value" : "a changed value"}`,
      );
    }
  }
  return failures;
}

async function validateAssets(paths) {
  if (paths.length === 0) {
    throw new Error("at least one --asset or CRUST_GAME_FILES path is required");
  }
  for (const path of paths) {
    if (!SUPPORTED_ASSET_EXTENSIONS.has(extname(path).toLowerCase())) {
      throw new Error(`unsupported game-file extension: ${path}`);
    }
    const metadata = await stat(path).catch((error) => {
      throw new Error(`cannot read game file ${path}: ${error.message}`);
    });
    if (!metadata.isFile()) throw new Error(`game-file path is not a file: ${path}`);
  }
}

async function loadReplay(path, fallback) {
  if (!path) return normalizeReplay(undefined, fallback);
  let decoded;
  try {
    decoded = JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    throw new Error(`could not read replay ${path}: ${error.message}`);
  }
  return normalizeReplay(decoded, fallback);
}

async function firstExisting(paths) {
  for (const path of paths) {
    try {
      await access(path);
      return path;
    } catch {
      // Try the next ordinary installation location.
    }
  }
  return undefined;
}

async function terminate(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolveExit) => {
    const timeout = setTimeout(resolveExit, 2_000);
    child.once("exit", () => {
      clearTimeout(timeout);
      resolveExit();
    });
    child.kill("SIGTERM");
  });
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
}

async function portIsBusy(url) {
  try {
    await fetch(url, { signal: AbortSignal.timeout(500) });
    return true;
  } catch (error) {
    if (error?.cause?.code === "ECONNREFUSED") return false;
    if (error?.name === "TimeoutError") return true;
    return false;
  }
}

async function startHarnessServer(url) {
  if (await portIsBusy(url)) {
    throw new Error(
      `${url} is already in use; pass --no-server only after verifying it is the harness`,
    );
  }
  const parsed = new URL(url);
  const output = [];
  const child = spawn(process.execPath, ["./scripts/serve.mjs"], {
    cwd: repositoryRoot,
    env: {
      ...process.env,
      CRUST_WEB_DIST: "target/browser-test-dist",
      PORT: parsed.port || "80",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let spawnError;
  child.on("error", (error) => {
    spawnError = error;
  });
  for (const stream of [child.stdout, child.stderr]) {
    stream.setEncoding("utf8");
    stream.on("data", (chunk) => output.push(chunk));
  }
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (spawnError) {
      throw new Error(`could not start harness server: ${spawnError.message}`);
    }
    if (child.exitCode !== null) {
      throw new Error(`harness server exited early:\n${output.join("")}`);
    }
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(500) });
      if (response.ok) return child;
    } catch {
      // The server may still be verifying its build metadata.
    }
    await delay(50);
  }
  await terminate(child);
  throw new Error(`harness server did not become ready:\n${output.join("")}`);
}

async function launchChrome(
  executable,
  { headed = false, windowWidth = 1440, windowHeight = 1100 } = {},
) {
  const profile = await mkdtemp(resolve(tmpdir(), "crust-browser-smoke-"));
  const output = [];
  const child = spawn(
    executable,
    [
      ...(headed
        ? [
            "--use-gl=angle",
            "--use-angle=swiftshader",
            "--enable-unsafe-swiftshader",
          ]
        : ["--headless=new"]),
      "--remote-debugging-port=0",
      `--user-data-dir=${profile}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-background-networking",
      "--disable-component-update",
      "--disable-sync",
      "--metrics-recording-only",
      "--autoplay-policy=no-user-gesture-required",
      `--window-size=${windowWidth},${windowHeight}`,
      "about:blank",
    ],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  let webSocketUrl;
  let spawnError;
  child.on("error", (error) => {
    spawnError = error;
  });
  for (const stream of [child.stdout, child.stderr]) {
    stream.setEncoding("utf8");
    stream.on("data", (chunk) => {
      output.push(chunk);
      webSocketUrl ??= output
        .join("")
        .match(/DevTools listening on (ws:\/\/\S+)/)?.[1];
    });
  }
  const deadline = Date.now() + 15_000;
  while (!webSocketUrl && Date.now() < deadline) {
    if (spawnError) {
      await removeTemporaryTree(profile);
      throw new Error(`could not launch Chrome: ${spawnError.message}`);
    }
    if (child.exitCode !== null) {
      await removeTemporaryTree(profile);
      throw new Error(`Chrome exited before DevTools was ready:\n${output.join("")}`);
    }
    await delay(25);
  }
  if (!webSocketUrl) {
    await terminate(child);
    await removeTemporaryTree(profile);
    throw new Error(`Chrome did not publish a DevTools endpoint:\n${output.join("")}`);
  }
  return { child, profile, webSocketUrl };
}

async function removeTemporaryTree(path) {
  await rm(path, {
    recursive: true,
    force: true,
    maxRetries: 8,
    retryDelay: 50,
  });
}

async function sha256File(path) {
  const hash = createHash("sha256");
  const stream = createReadStream(path);
  stream.on("data", (chunk) => hash.update(chunk));
  await once(stream, "end");
  return hash.digest("hex");
}

function waitForChild(child, label, output) {
  return new Promise((resolveExit, rejectExit) => {
    child.once("error", (error) => {
      rejectExit(new Error(`${label} could not start: ${error.message}`));
    });
    child.once("close", (code, signal) => {
      if (code === 0) {
        resolveExit();
      } else {
        rejectExit(
          new Error(
            `${label} exited with ${signal ?? code}:\n${output.join("")}`,
          ),
        );
      }
    });
  });
}

function requestedPresentation(options) {
  return {
    smoothMotion: options.smoothMotion,
    extendedWorld: options.extendedWorld,
    cameraZoom: options.cameraZoom,
    outputAspect: options.outputAspect,
    renderResolution: options.renderResolution,
  };
}

async function readPresentation(cdp, sessionId) {
  return evaluate(
    cdp,
    sessionId,
    `(() => {
      const canvas = document.querySelector("#canvas");
      const rect = canvas?.getBoundingClientRect();
      return {
        smoothMotion: document.querySelector("#smoothMotion")?.checked,
        extendedWorld: document.querySelector("#extendedWorld")?.checked,
        cameraZoom: document.querySelector("#cameraZoom")?.value,
        outputAspect: document.querySelector("#outputAspect")?.value,
        renderResolution: document.querySelector("#renderResolution")?.value,
        devicePixelRatio,
        canvasWidth: canvas?.width,
        canvasHeight: canvas?.height,
        rect: rect && {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height
        }
      };
    })()`,
  );
}

export function presentationFailures(presentation, expected) {
  const problems = [];
  for (const [name, value] of Object.entries(expected)) {
    if (presentation?.[name] !== value) {
      problems.push(
        `${name}: expected ${JSON.stringify(value)}, received ${JSON.stringify(presentation?.[name])}`,
      );
    }
  }
  const rect = presentation?.rect;
  const usable =
    rect
    && [rect.x, rect.y, rect.width, rect.height].every(Number.isFinite)
    && rect.width >= 2
    && rect.height >= 2
    && [presentation?.canvasWidth, presentation?.canvasHeight].every(Number.isSafeInteger)
    && presentation.canvasWidth >= 2
    && presentation.canvasHeight >= 2;
  let expectedRatio;
  if (expected.outputAspect === "4:3") expectedRatio = 4 / 3;
  if (expected.outputAspect === "16:9") expectedRatio = 16 / 9;
  if (expected.outputAspect === "21:9") expectedRatio = 21 / 9;
  const rectRatio = usable ? rect.width / rect.height : Number.NaN;
  const canvasRatio = usable
    ? presentation.canvasWidth / presentation.canvasHeight
    : Number.NaN;
  const ratioMatches = expectedRatio === undefined
    ? usable && Math.abs(rectRatio - canvasRatio) <= 0.01
    : usable
      && Math.abs(rectRatio - expectedRatio) <= 0.01
      && Math.abs(canvasRatio - expectedRatio) <= 0.01;
  if (!usable || !ratioMatches) {
    problems.push(
      `canvas is not a usable ${expected.outputAspect} capture surface: ${JSON.stringify({
        rect,
        width: presentation?.canvasWidth,
        height: presentation?.canvasHeight,
      })}`,
    );
  }
  const fixedHeight = {
    "720": 720,
    "1080": 1080,
    "1440": 1440,
    "2160": 2160,
  }[expected.renderResolution];
  if (fixedHeight !== undefined && presentation?.canvasHeight !== fixedHeight) {
    problems.push(
      `canvasHeight: expected fixed ${fixedHeight}, received ${JSON.stringify(presentation?.canvasHeight)}`,
    );
  }
  return problems;
}

async function assertRequestedPresentation(options, cdp, sessionId) {
  const presentation = await readPresentation(cdp, sessionId);
  const problems = presentationFailures(
    presentation,
    requestedPresentation(options),
  );
  if (problems.length > 0) {
    throw new Error(
      `video capture presentation does not match the requested settings:\n${problems.join("\n")}`,
    );
  }
  return presentation;
}

class SourceFrameVideoRecorder {
  static async create(options, cdp, sessionId) {
    for (const [path, label] of [
      [options.video, "video"],
      [options.chapters, "chapter list"],
    ]) {
      await access(path).then(
        () => {
          throw new Error(`${label} output already exists: ${path}`);
        },
        (error) => {
          if (error?.code !== "ENOENT") throw error;
        },
      );
      await mkdir(dirname(path), { recursive: true });
    }
    await access(options.ffmpeg).catch((error) => {
      throw new Error(`cannot execute ffmpeg at ${options.ffmpeg}: ${error.message}`);
    });
    const presentation = await assertRequestedPresentation(
      options,
      cdp,
      sessionId,
    );
    const clip = {
      x: 0,
      y: 0,
      width: presentation.canvasWidth,
      height: presentation.canvasHeight,
      scale: 1,
    };
    const frameRate = options.smoothMotion
      ? SHOWCASE_VIDEO_FRAME_RATE
      : VIDEO_FRAME_RATE;
    const frameRateArgument = options.smoothMotion
      ? SHOWCASE_VIDEO_FRAME_RATE_ARGUMENT
      : String(VIDEO_FRAME_RATE);
    const partialPath = `${options.video}.partial-${process.pid}.mp4`;
    await rm(partialPath, { force: true });
    const ffmpegOutput = [];
    const ffmpeg = spawn(
      options.ffmpeg,
      [
        "-hide_banner",
        "-loglevel", "warning",
        "-f", "image2pipe",
        "-framerate", frameRateArgument,
        "-vcodec", "mjpeg",
        "-i", "pipe:0",
        "-an",
        "-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2",
        "-c:v", "libx264",
        "-preset", "veryfast",
        "-crf", "24",
        "-pix_fmt", "yuv420p",
        "-movflags", "+faststart",
        partialPath,
      ],
      { stdio: ["pipe", "ignore", "pipe"] },
    );
    ffmpeg.stderr.setEncoding("utf8");
    ffmpeg.stderr.on("data", (chunk) => ffmpegOutput.push(chunk));
    const exit = waitForChild(ffmpeg, "ffmpeg video encoder", ffmpegOutput);
    return new SourceFrameVideoRecorder({
      cdp,
      sessionId,
      videoPath: options.video,
      chaptersPath: options.chapters,
      partialPath,
      clip,
      presentation,
      frameRate,
      ffmpeg,
      ffmpegExit: exit,
    });
  }

  constructor(values) {
    Object.assign(this, values);
    this.frames = 0;
    this.chapters = [];
    this.currentChapter = undefined;
    this.finished = false;
    this.aborted = false;
  }

  startChapter(id, segment) {
    if (this.currentChapter) {
      this.currentChapter.endFrame = this.frames;
    }
    this.currentChapter = {
      id,
      segment,
      startFrame: this.frames,
      endFrame: null,
    };
    this.chapters.push(this.currentChapter);
  }

  async capture(sourceFrameJpeg) {
    if (this.finished || this.aborted) {
      throw new Error("video recorder is not accepting source frames");
    }
    if (this.frames % VIDEO_DISK_CHECK_INTERVAL === 0) {
      const filesystem = await statfs(dirname(this.videoPath));
      const freeBytes = filesystem.bavail * filesystem.bsize;
      if (freeBytes < VIDEO_MINIMUM_FREE_BYTES) {
        throw new Error(
          `video capture aborted with ${freeBytes} free bytes; the 3 GiB safety floor was crossed`,
        );
      }
    }
    const prefix = "data:image/jpeg;base64,";
    if (typeof sourceFrameJpeg !== "string" || !sourceFrameJpeg.startsWith(prefix)) {
      throw new Error("browser did not return a JPEG canvas readback");
    }
    const bytes = Buffer.from(sourceFrameJpeg.slice(prefix.length), "base64");
    if (bytes.length < 4 || bytes[0] !== 0xff || bytes[1] !== 0xd8) {
      throw new Error("Chrome did not return a JPEG source frame");
    }
    if (!this.ffmpeg.stdin.write(bytes)) {
      await once(this.ffmpeg.stdin, "drain");
    }
    this.frames += 1;
    if (this.frames % 3_000 === 0) {
      process.stderr.write(
        `captured ${this.frames} source frames (${(this.frames / this.frameRate).toFixed(1)} video seconds)\n`,
      );
    }
  }

  async finish() {
    if (this.finished) throw new Error("video recorder was already finalized");
    if (this.aborted) throw new Error("video recorder was aborted");
    if (this.currentChapter) this.currentChapter.endFrame = this.frames;
    this.ffmpeg.stdin.end();
    await this.ffmpegExit;
    await rename(this.partialPath, this.videoPath);
    this.finished = true;
    const metadata = await stat(this.videoPath);
    const chapters = this.chapters.map((chapter) => ({
      ...chapter,
      startSeconds: chapter.startFrame / this.frameRate,
      endSeconds: chapter.endFrame / this.frameRate,
      timestamp: new Date(chapter.startFrame * 1000 / this.frameRate)
        .toISOString()
        .slice(11, 23),
    }));
    const chapterDocument = {
      schema: 1,
      frameRate: this.frameRate,
      frameCount: this.frames,
      durationSeconds: this.frames / this.frameRate,
      audio: false,
      presentation: {
        aspect: this.presentation.outputAspect,
        resolution: this.presentation.renderResolution,
        cameraZoom: this.presentation.cameraZoom,
        smoothMotion: this.presentation.smoothMotion,
        extendedWorld: this.presentation.extendedWorld,
        canvasWidth: this.presentation.canvasWidth,
        canvasHeight: this.presentation.canvasHeight,
        captureWidth: this.clip.width,
        captureHeight: this.clip.height,
      },
      chapters,
    };
    await writeFile(
      this.chaptersPath,
      `${JSON.stringify(chapterDocument, null, 2)}\n`,
      { flag: "wx" },
    );
    return {
      path: this.videoPath,
      sha256: await sha256File(this.videoPath),
      bytes: metadata.size,
      frameRate: this.frameRate,
      frameCount: this.frames,
      durationSeconds: this.frames / this.frameRate,
      width: this.clip.width - (this.clip.width % 2),
      height: this.clip.height - (this.clip.height % 2),
      audio: false,
      chapters: this.chaptersPath,
      phaseCount: chapters.length,
    };
  }

  async abort() {
    if (this.finished || this.aborted) return;
    this.aborted = true;
    this.ffmpeg.stdin.destroy();
    await terminate(this.ffmpeg);
    await this.ffmpegExit.catch(() => {});
    await rm(this.partialPath, { force: true });
  }
}

function pcmS16lePeak(bytes) {
  if (!Buffer.isBuffer(bytes) || bytes.length % 4 !== 0) {
    throw new Error("captured PCM must contain interleaved stereo s16le frames");
  }
  let peak = 0;
  for (let offset = 0; offset < bytes.length; offset += 2) {
    peak = Math.max(peak, Math.abs(bytes.readInt16LE(offset)));
  }
  return peak;
}

async function writeEncoderPipe(stream, bytes, label) {
  if (stream.destroyed || !stream.writable) {
    throw new Error(`${label} closed before the capture window finished`);
  }
  if (!stream.write(bytes)) await once(stream, "drain");
}

class SourceFrameWindowRecorder {
  static async create(options, cdp, sessionId) {
    const outputs = [
      [options.chapters, "chapter metadata"],
      ...options.videoWindows.map((window) => [
        window.outputPath,
        `video window ${window.name}`,
      ]),
    ];
    for (const [path, label] of outputs) {
      await access(path).then(
        () => {
          throw new Error(`${label} output already exists: ${path}`);
        },
        (error) => {
          if (error?.code !== "ENOENT") throw error;
        },
      );
      await mkdir(dirname(path), { recursive: true });
    }
    await access(options.ffmpeg).catch((error) => {
      throw new Error(`cannot execute ffmpeg at ${options.ffmpeg}: ${error.message}`);
    });
    for (const window of options.videoWindows) {
      for (const path of [
        `${window.outputPath}.partial-${process.pid}.mp4`,
        `${window.outputPath}.video-${process.pid}.mp4`,
        `${window.outputPath}.audio-${process.pid}.s16le`,
      ]) {
        await rm(path, { force: true });
      }
    }
    const presentation = await assertRequestedPresentation(
      options,
      cdp,
      sessionId,
    );
    const audio = await evaluate(
      cdp,
      sessionId,
      `(() => {
        const harness = window.__crustTest;
        if (typeof harness?.setAudioCaptureEnabled !== "function"
            || typeof harness?.takeAudioFramePcm16 !== "function") {
          return { error: "deterministic browser-test audio capture is unavailable" };
        }
        harness.setAudioCaptureEnabled(true);
        return {
          error: harness.lastError ?? null,
          sampleRate: harness.audioSampleRate ?? null,
          channels: harness.audioChannels ?? null,
          format: harness.audioFormat ?? null
        };
      })()`,
    );
    if (audio?.error != null) {
      throw new Error(`could not enable deterministic audio capture: ${audio.error}`);
    }
    if (
      audio?.sampleRate !== CAPTURE_AUDIO_SAMPLE_RATE
      || audio?.channels !== CAPTURE_AUDIO_CHANNELS
      || audio?.format !== "s16le"
    ) {
      throw new Error(
        `unexpected deterministic audio format: ${JSON.stringify(audio)}`,
      );
    }
    const encoders = options.videoWindows.map((window) => {
      const partialPath = `${window.outputPath}.partial-${process.pid}.mp4`;
      const videoPartialPath = `${window.outputPath}.video-${process.pid}.mp4`;
      const audioPartialPath = `${window.outputPath}.audio-${process.pid}.s16le`;
      const output = [];
      const ffmpeg = spawn(
        options.ffmpeg,
        [
          "-hide_banner",
          "-loglevel", "warning",
          "-f", "image2pipe",
          "-framerate", SHOWCASE_VIDEO_FRAME_RATE_ARGUMENT,
          "-vcodec", "mjpeg",
          "-i", "pipe:0",
          "-an",
          "-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2",
          "-c:v", "libx264",
          "-preset", "veryfast",
          "-crf", "24",
          "-pix_fmt", "yuv420p",
          videoPartialPath,
        ],
        { stdio: ["pipe", "ignore", "pipe"] },
      );
      ffmpeg.stderr.setEncoding("utf8");
      ffmpeg.stderr.on("data", (chunk) => output.push(chunk));
      ffmpeg.stdin.on("error", (error) => output.push(`video pipe: ${error.message}\n`));
      const audioStream = createWriteStream(audioPartialPath, { flags: "wx" });
      audioStream.on("error", (error) => output.push(`audio file: ${error.message}\n`));
      return {
        ...window,
        expectedFrames: window.endFrame - window.startFrame,
        partialPath,
        videoPartialPath,
        audioPartialPath,
        ffmpeg,
        ffmpegExit: waitForChild(
          ffmpeg,
          `ffmpeg encoder for ${window.name}`,
          output,
        ),
        audioStream,
        muxFfmpeg: undefined,
        frames: 0,
        audioBytes: 0,
        audioPeak: 0,
      };
    });
    return new SourceFrameWindowRecorder({
      cdp,
      sessionId,
      chaptersPath: options.chapters,
      presentation,
      encoders,
      ffmpegPath: options.ffmpeg,
    });
  }

  constructor(values) {
    Object.assign(this, values);
    this.totalCapturedFrames = 0;
    this.chapters = [];
    this.currentChapter = undefined;
    this.finished = false;
    this.aborted = false;
  }

  captureWindows(sourceFrame) {
    return this.encoders.filter(
      (window) => sourceFrame >= window.startFrame && sourceFrame < window.endFrame,
    );
  }

  needsCapture(sourceFrame) {
    return this.captureWindows(sourceFrame).length > 0;
  }

  batchFrameCount(sourceFrame, maximumFrames) {
    return showcaseWindowBatchFrameCount(
      sourceFrame,
      maximumFrames,
      this.encoders.map((window) => window.startFrame),
      { needsCapture: this.needsCapture(sourceFrame) },
    );
  }

  startChapter(id, segment, sourceFrame) {
    if (this.currentChapter) this.currentChapter.endFrame = sourceFrame;
    this.currentChapter = {
      id,
      segment,
      startFrame: sourceFrame,
      endFrame: null,
    };
    this.chapters.push(this.currentChapter);
  }

  async capture(sourceFrame, sourceFrameJpeg, sourceFrameAudio) {
    if (this.finished || this.aborted) {
      throw new Error("video-window recorder is not accepting source frames");
    }
    const windows = this.captureWindows(sourceFrame);
    if (windows.length === 0) {
      throw new Error(`source frame ${sourceFrame} is outside every video window`);
    }
    if (this.totalCapturedFrames % VIDEO_DISK_CHECK_INTERVAL === 0) {
      for (const directory of new Set(windows.map((window) => dirname(window.outputPath)))) {
        const filesystem = await statfs(directory);
        const freeBytes = filesystem.bavail * filesystem.bsize;
        if (freeBytes < VIDEO_MINIMUM_FREE_BYTES) {
          throw new Error(
            `showcase capture aborted with ${freeBytes} free bytes; the 3 GiB safety floor was crossed`,
          );
        }
      }
    }
    const jpegPrefix = "data:image/jpeg;base64,";
    if (
      typeof sourceFrameJpeg !== "string"
      || !sourceFrameJpeg.startsWith(jpegPrefix)
    ) {
      throw new Error(`source frame ${sourceFrame} has no JPEG canvas readback`);
    }
    const jpeg = Buffer.from(sourceFrameJpeg.slice(jpegPrefix.length), "base64");
    if (jpeg.length < 4 || jpeg[0] !== 0xff || jpeg[1] !== 0xd8) {
      throw new Error(`source frame ${sourceFrame} is not a JPEG image`);
    }
    if (typeof sourceFrameAudio !== "string" || sourceFrameAudio.length === 0) {
      throw new Error(`source frame ${sourceFrame} has no deterministic PCM payload`);
    }
    const pcm = Buffer.from(sourceFrameAudio, "base64");
    if (pcm.length === 0 || pcm.length % 4 !== 0) {
      throw new Error(`source frame ${sourceFrame} has malformed stereo s16le PCM`);
    }
    const peak = pcmS16lePeak(pcm);
    for (const window of windows) {
      await Promise.all([
        writeEncoderPipe(window.ffmpeg.stdin, jpeg, `${window.name} video pipe`),
        writeEncoderPipe(window.audioStream, pcm, `${window.name} audio file`),
      ]);
      window.frames += 1;
      window.audioBytes += pcm.length;
      window.audioPeak = Math.max(window.audioPeak, peak);
    }
    this.totalCapturedFrames += 1;
    if (this.totalCapturedFrames % 1_000 === 0) {
      process.stderr.write(
        `captured ${this.totalCapturedFrames} showcase source frames across all windows\n`,
      );
    }
  }

  async muxWindow(window) {
    const output = [];
    const ffmpeg = spawn(
      this.ffmpegPath,
      [
        "-hide_banner",
        "-loglevel", "warning",
        "-i", window.videoPartialPath,
        "-f", "s16le",
        "-ar", String(CAPTURE_AUDIO_SAMPLE_RATE),
        "-ac", String(CAPTURE_AUDIO_CHANNELS),
        "-i", window.audioPartialPath,
        "-map", "0:v:0",
        "-map", "1:a:0",
        "-c:v", "copy",
        "-c:a", "aac",
        "-b:a", "192k",
        "-movflags", "+faststart",
        window.partialPath,
      ],
      { stdio: ["ignore", "ignore", "pipe"] },
    );
    window.muxFfmpeg = ffmpeg;
    ffmpeg.stderr.setEncoding("utf8");
    ffmpeg.stderr.on("data", (chunk) => output.push(chunk));
    await waitForChild(ffmpeg, `ffmpeg muxer for ${window.name}`, output);
    window.muxFfmpeg = undefined;
  }

  async finish(finalSourceFrame) {
    if (this.finished) throw new Error("video-window recorder was already finalized");
    if (this.aborted) throw new Error("video-window recorder was aborted");
    if (this.currentChapter) this.currentChapter.endFrame = finalSourceFrame;
    for (const window of this.encoders) {
      if (window.frames !== window.expectedFrames) {
        throw new Error(
          `${window.name} captured ${window.frames} frames; expected ${window.expectedFrames}`,
        );
      }
      if (window.audioPeak === 0) {
        throw new Error(`${window.name} deterministic audio is silent`);
      }
      if (window.audioStream.destroyed && !window.audioStream.writableFinished) {
        throw new Error(`${window.name} audio file closed before capture finished`);
      }
      window.ffmpeg.stdin.end();
      window.audioFinished = once(window.audioStream, "finish");
      window.audioStream.end();
    }
    await Promise.all(
      this.encoders.flatMap((window) => [
        window.ffmpegExit,
        window.audioFinished,
      ]),
    );
    await Promise.all(this.encoders.map((window) => this.muxWindow(window)));
    for (const window of this.encoders) {
      await rm(window.videoPartialPath, { force: true });
      await rm(window.audioPartialPath, { force: true });
      await rename(window.partialPath, window.outputPath);
    }
    await evaluate(
      this.cdp,
      this.sessionId,
      `window.__crustTest?.setAudioCaptureEnabled?.(false)`,
    );
    const chapterMetadata = this.chapters.map((chapter) => ({
      ...chapter,
      startSeconds: chapter.startFrame * SOURCE_FRAME_DURATION_MS / 1_000,
      endSeconds: chapter.endFrame * SOURCE_FRAME_DURATION_MS / 1_000,
      timestamp: new Date(chapter.startFrame * SOURCE_FRAME_DURATION_MS)
        .toISOString()
        .slice(11, 23),
    }));
    const windows = [];
    for (const window of this.encoders) {
      const metadata = await stat(window.outputPath);
      const audioSampleFrames = window.audioBytes / (2 * CAPTURE_AUDIO_CHANNELS);
      windows.push({
        name: window.name,
        startFrame: window.startFrame,
        endFrame: window.endFrame,
        path: window.outputPath,
        sha256: await sha256File(window.outputPath),
        bytes: metadata.size,
        frameCount: window.frames,
        frameRate: SHOWCASE_VIDEO_FRAME_RATE,
        durationSeconds: window.frames / SHOWCASE_VIDEO_FRAME_RATE,
        width: this.presentation.canvasWidth - (this.presentation.canvasWidth % 2),
        height: this.presentation.canvasHeight - (this.presentation.canvasHeight % 2),
        audio: {
          codec: "aac",
          sourceFormat: "s16le",
          sampleRate: CAPTURE_AUDIO_SAMPLE_RATE,
          channels: CAPTURE_AUDIO_CHANNELS,
          sampleFrames: audioSampleFrames,
          sourceDurationSeconds: audioSampleFrames / CAPTURE_AUDIO_SAMPLE_RATE,
          peak: window.audioPeak,
          nonSilent: window.audioPeak > 0,
        },
      });
    }
    const document = {
      schema: 2,
      sourceFrameDurationMs: SOURCE_FRAME_DURATION_MS,
      frameRate: {
        numerator: 1_000,
        denominator: SOURCE_FRAME_DURATION_MS,
        value: SHOWCASE_VIDEO_FRAME_RATE,
      },
      sourceFramesSimulated: finalSourceFrame,
      audio: {
        captured: true,
        systemAudio: false,
        source: "deterministic final software mix per completed browser-test source frame",
        sampleRate: CAPTURE_AUDIO_SAMPLE_RATE,
        channels: CAPTURE_AUDIO_CHANNELS,
        sourceFormat: "s16le",
      },
      presentation: {
        aspect: this.presentation.outputAspect,
        resolution: this.presentation.renderResolution,
        cameraZoom: this.presentation.cameraZoom,
        smoothMotion: this.presentation.smoothMotion,
        extendedWorld: this.presentation.extendedWorld,
        canvasWidth: this.presentation.canvasWidth,
        canvasHeight: this.presentation.canvasHeight,
      },
      windows,
      chapters: chapterMetadata,
    };
    await writeFile(
      this.chaptersPath,
      `${JSON.stringify(document, null, 2)}\n`,
      { flag: "wx" },
    );
    this.finished = true;
    return {
      chapters: this.chaptersPath,
      frameRate: SHOWCASE_VIDEO_FRAME_RATE,
      sourceFrameDurationMs: SOURCE_FRAME_DURATION_MS,
      totalCapturedFrames: this.totalCapturedFrames,
      windows,
    };
  }

  async abort() {
    if (this.finished || this.aborted) return;
    this.aborted = true;
    await evaluate(
      this.cdp,
      this.sessionId,
      `window.__crustTest?.setAudioCaptureEnabled?.(false)`,
    ).catch(() => {});
    for (const window of this.encoders) {
      window.ffmpeg.stdin.destroy();
      window.audioStream.destroy();
      await terminate(window.ffmpeg);
      await window.ffmpegExit.catch(() => {});
      await terminate(window.muxFfmpeg);
      await rm(window.partialPath, { force: true });
      await rm(window.videoPartialPath, { force: true });
      await rm(window.audioPartialPath, { force: true });
    }
  }
}

async function evaluate(cdp, sessionId, expression) {
  const response = await cdp.command(
    "Runtime.evaluate",
    {
      expression,
      awaitPromise: true,
      returnByValue: true,
      userGesture: true,
    },
    sessionId,
  );
  if (response.exceptionDetails) {
    const detail =
      response.exceptionDetails.exception?.description ??
      response.exceptionDetails.text ??
      "unknown evaluation error";
    throw new Error(`browser evaluation failed: ${detail}`);
  }
  return response.result?.value;
}

const blobRangeInstrumentation = `(() => {
  const originalSlice = Blob.prototype.slice;
  const originalArrayBuffer = Blob.prototype.arrayBuffer;
  window.__crustBrowserSmokeBlobEvidence = {
    blobRanges: [],
    arrayBufferSizes: []
  };
  Blob.prototype.slice = function(start, end, contentType) {
    const actualStart = start ?? 0;
    const actualEnd = end ?? this.size;
    window.__crustBrowserSmokeBlobEvidence.blobRanges.push({
      sourceSize: this.size,
      start: actualStart,
      end: actualEnd
    });
    return Reflect.apply(originalSlice, this, [start, end, contentType]);
  };
  Blob.prototype.arrayBuffer = function() {
    window.__crustBrowserSmokeBlobEvidence.arrayBufferSizes.push(this.size);
    return Reflect.apply(originalArrayBuffer, this, []);
  };
})();`;

const runtimeLogHistoryInstrumentation = `(() => {
  const appendedRuntimeLogLines = ${appendedRuntimeLogLines.toString()};
  const install = () => {
    const log = document.querySelector("#runtimeLog");
    if (!log) throw new Error("browser smoke runtime log is unavailable");
    const evidence = {
      previousText: log.textContent ?? "",
      entries: []
    };
    window.__crustBrowserSmokeRuntimeLogEvidence = evidence;
    const capture = (line) => {
      const debug = window.__crustDebug ?? {};
      const harness = window.__crustTest ?? {};
      evidence.entries.push({
        line,
        stepCount: harness.stepCount ?? null,
        hostCallbackCount: harness.hostCallbackCount ?? null,
        currentLid: debug.currentLid ?? null,
        mountedLid: debug.mountedLid ?? null,
        retailFrame: debug.retailFrame ?? null,
        retailDrawCount: debug.retailDrawCount ?? null,
        retailProcessDrawCount: debug.retailProcessDrawCount ?? null,
        retailHardRestarts: debug.retailHardRestarts ?? null,
        retailLoadStates: debug.retailLoadStates ?? null,
        retailDeathCameraFrames: debug.retailDeathCameraFrames ?? null,
        retailExecutions: debug.retailExecutions ?? null,
        retailExecutionErrors: debug.retailExecutionErrors ?? null,
        retailFaultedObjects: debug.retailFaultedObjects ?? null,
        retailZoneEventFailures: debug.retailZoneEventFailures ?? null,
        retailRandomSeed: debug.retailRandomSeed ?? null,
        retailRandomSeedB: debug.retailRandomSeedB ?? null
      });
    };
    new MutationObserver(() => {
      const currentText = log.textContent ?? "";
      for (const line of appendedRuntimeLogLines(
        evidence.previousText,
        currentText,
      )) {
        if (
          line.includes("retail PBAK")
          || line.includes("Retail PBAK")
          || line.includes("Retail LEVEL_END resolved")
        ) {
          capture(line);
        }
      }
      evidence.previousText = currentText;
    }).observe(log, { childList: true, characterData: true, subtree: true });
  };
  if (document.readyState === "loading") {
    addEventListener("DOMContentLoaded", install, { once: true });
  } else {
    install();
  }
})();`;

const snapshotExpression = `(() => {
  const debug = window.__crustDebug || {};
  const harness = window.__crustTest || {};
  const browserTestObjects =
    typeof debug.snapshotRetailObjects === "function"
      ? debug.snapshotRetailObjects()
      : null;
  const livePlayer = Array.isArray(browserTestObjects)
    ? browserTestObjects.find((object) => object?.player === true)
    : null;
  const liveCollider = livePlayer?.collider && Array.isArray(browserTestObjects)
    ? browserTestObjects.find((object) =>
        object?.handle?.arenaSlot === livePlayer.collider.arenaSlot
        && object?.handle?.arenaGeneration === livePlayer.collider.arenaGeneration
        && object?.handle?.vm === livePlayer.collider.vm
      )
    : null;
  const pick = (source, names) => Object.fromEntries(
    names.map((name) => [name, source[name] ?? null])
  );
  return {
    bootstrap: window.__crustBootstrap ?? null,
    runtimeState: document.querySelector(".shell")?.dataset.runtimeState ?? null,
    runtimeStatus: document.querySelector("#runtimeStatus")?.textContent ?? null,
    assetMessage: document.querySelector("#assetMessage")?.textContent ?? null,
    fileCount: Number(document.querySelector("#fileCount")?.textContent ?? 0),
    pairCount: Number(document.querySelector("#pairCount")?.textContent ?? 0),
    launchDisabled: Boolean(document.querySelector("#launch")?.disabled),
    progressHidden: Boolean(document.querySelector("#importProgress")?.hidden),
    cardState: document.querySelector("#cardState")?.textContent ?? null,
    audioState: document.querySelector("#audioState")?.textContent ?? null,
    runtimeLog: document.querySelector("#runtimeLog")?.textContent ?? "",
    consoleErrors: [...(window.__consoleErrors || [])],
    harness: pick(harness, [
      "mode", "frameDurationMs", "stepCount", "hostCallbackCount",
      "lastSimulationStepped", "lastError", "lastHeld", "lastInputKind",
      "lastTimestampMs", "lastRequestedLid", "directBonusStateBoundary"
    ]),
    debug: {
      ...pick(debug, [
        "frame", "currentLid", "titleState", "retailTitleScreen",
        "retailTitleNextScreen", "retailTitlePhase", "retailTitleFadeCounter",
        "pairs", "mountedLid",
        "mountedPages", "mountedEntries", "glError", "paused", "retailFrame",
        "retailDrawCount", "retailProcessDrawCount", "retailRandomSeed",
        "retailRandomSeedB", "retailPathProgress", "retailCameraZone",
        "retailCameraPath", "retailCameraGameState", "retailCurrentZone",
        "retailLiveObjects",
        "retailHardRestarts", "retailLoadStates", "retailDeathCameraFrames",
        "retailAlreadyActiveSpawnSkips", "retailAuthoredSpawnRejections",
        "retailFailedSpawns",
        "retailFaultedObjects", "retailExecutions", "retailExecutionErrors",
        "retailZoneEventFailures", "retailRuntimeError", "retailRuntimeWarning",
        "audioContextState", "audioCallbacks", "audioPeak",
        "retailAudioCallbacks", "retailAudioActiveVoices", "retailMusicState",
        "retailPadHeld", "retailPadTapped", "retailPadHeldPrevious",
        "retailPadTappedPrevious", "retailPadHeldPrevious2",
        "retailMainHaltReason"
      ]),
      retailMain: debug.retailMain
        ? {
            ...debug.retailMain,
            floorY: livePlayer?.floorY ?? null,
            objectStatus: livePlayer?.status ?? null,
            solidWallSteps: livePlayer?.solidWallSteps ?? [],
            collider: liveCollider
              ? {
                  entityId: liveCollider.entityId ?? null,
                  executable: liveCollider.executable ?? null,
                  subtype: liveCollider.subtype ?? null,
                  state: liveCollider.state ?? null,
                  translation: liveCollider.translation ?? null,
                  bound: liveCollider.bound ?? null,
                  frameBound: liveCollider.frameBound ?? null
                }
              : null
          }
        : null,
      browserTestGlobals: debug.browserTestGlobals
        ? { ...debug.browserTestGlobals }
        : null,
      playerLifeCount: Number.isSafeInteger(livePlayer?.register65)
        ? livePlayer.register65
        : null,
      browserTestObjects
    }
  };
})()`;

async function browserSnapshot(cdp, sessionId) {
  return evaluate(cdp, sessionId, snapshotExpression);
}

async function waitFor(cdp, sessionId, description, predicate, failures, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let last;
  while (Date.now() < deadline) {
    if (failures.length > 0) {
      throw new Error(`${description} failed:\n${failures.join("\n")}`);
    }
    last = await browserSnapshot(cdp, sessionId);
    const snapshotProblems = snapshotFailures(last);
    if (snapshotProblems.length > 0) {
      throw new Error(`${description} failed:\n${snapshotProblems.join("\n")}`);
    }
    if (predicate(last)) return last;
    await delay(25);
  }
  throw new Error(
    `timed out waiting for ${description}; last state:\n${JSON.stringify(last, null, 2)}`,
  );
}

function assertExpected(expectation, snapshot, label) {
  const failures = expectationFailures(expectation, snapshot);
  if (failures.length > 0) {
    const evidence = {
      currentLid: snapshot.debug?.currentLid ?? null,
      mountedLid: snapshot.debug?.mountedLid ?? null,
      retailFrame: snapshot.debug?.retailFrame ?? null,
      retailHardRestarts: snapshot.debug?.retailHardRestarts ?? null,
      retailLoadStates: snapshot.debug?.retailLoadStates ?? null,
      retailDeathCameraFrames: snapshot.debug?.retailDeathCameraFrames ?? null,
      retailCurrentZone: snapshot.debug?.retailCurrentZone ?? null,
      retailCameraZone: snapshot.debug?.retailCameraZone ?? null,
      retailCameraPath: snapshot.debug?.retailCameraPath ?? null,
      retailMainHaltReason: snapshot.debug?.retailMainHaltReason ?? null,
      retailMain: snapshot.debug?.retailMain ?? null,
    };
    throw new Error(
      `${label} expectation failed:\n${failures.join("\n")}\n`
      + `evidence: ${JSON.stringify(evidence)}`,
    );
  }
}

async function attachPage(cdp) {
  const { targetInfos } = await cdp.command("Target.getTargets");
  let target = targetInfos.find(
    (candidate) => candidate.type === "page" && candidate.url === "about:blank",
  );
  if (!target) {
    const created = await cdp.command("Target.createTarget", { url: "about:blank" });
    target = { targetId: created.targetId };
  }
  const { sessionId } = await cdp.command("Target.attachToTarget", {
    targetId: target.targetId,
    flatten: true,
  });
  return sessionId;
}

function remoteArgumentText(argument) {
  if (argument.value !== undefined) {
    try {
      return JSON.stringify(argument.value);
    } catch {
      return String(argument.value);
    }
  }
  return argument.description ?? argument.type ?? "unknown console argument";
}

async function reloadPage(cdp, sessionId) {
  let removeListener = () => {};
  const loaded = new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      removeListener();
      reject(new Error("timed out waiting for the browser reload"));
    }, 30_000);
    removeListener = cdp.on("Page.loadEventFired", (_params, eventSession) => {
      if (eventSession !== sessionId) return;
      clearTimeout(timeout);
      removeListener();
      resolve();
    });
  });
  await cdp.command("Page.reload", { ignoreCache: true }, sessionId, 30_000);
  await loaded;
}

async function runBrowser(options, replay, chromeExecutable, storageSeeds) {
  const showcaseCapture = options.videoWindows.length > 0;
  const chrome = await launchChrome(chromeExecutable, {
    headed: options.video !== undefined || showcaseCapture,
    windowWidth: showcaseCapture ? 2560 : 1440,
    windowHeight: showcaseCapture ? 1200 : 1100,
  });
  const expectedRetailPbakEids = retailPbakAuditExpectedEids(options);
  let cdp;
  let videoRecorder;
  try {
    cdp = await ChromeCdp.connect(chrome.webSocketUrl);
    const sessionId = await attachPage(cdp);
    const failures = [];
    const networkRequests = [];
    const pageOrigin = new URL(options.url).origin;
    const recordFailure = (message) => failures.push(message);
    cdp.on("Runtime.exceptionThrown", (params, eventSession) => {
      if (eventSession !== sessionId) return;
      recordFailure(
        "uncaught exception: " +
          (params.exceptionDetails.exception?.description ??
            params.exceptionDetails.text),
      );
    });
    cdp.on("Runtime.consoleAPICalled", (params, eventSession) => {
      if (
        eventSession === sessionId &&
        ["error", "assert"].includes(params.type)
      ) {
        recordFailure(
          `console.${params.type}: ${params.args.map(remoteArgumentText).join(" ")}`,
        );
      }
    });
    cdp.on("Log.entryAdded", (params, eventSession) => {
      if (eventSession === sessionId && params.entry.level === "error") {
        recordFailure(`browser log error: ${params.entry.text}`);
      }
    });
    cdp.on("Network.loadingFailed", (params, eventSession) => {
      if (eventSession === sessionId) {
        recordFailure(`network load failed: ${params.errorText}`);
      }
    });
    cdp.on("Network.responseReceived", (params, eventSession) => {
      if (eventSession === sessionId && params.response.status >= 400) {
        recordFailure(
          `network response ${params.response.status}: ${params.response.url}`,
        );
      }
    });
    cdp.on("Network.requestWillBeSent", (params, eventSession) => {
      if (eventSession !== sessionId) return;
      const { method, url } = params.request;
      networkRequests.push({ method, url });
      if (method !== "GET") recordFailure(`unexpected network method ${method}: ${url}`);
      const parsed = new URL(url);
      if (!["data:", "blob:"].includes(parsed.protocol) && parsed.origin !== pageOrigin) {
        recordFailure(`cross-origin request: ${method} ${url}`);
      }
    });
    for (const domain of ["Page", "Runtime", "Log", "Network", "DOM"]) {
      await cdp.command(`${domain}.enable`, {}, sessionId);
    }
    await cdp.command(
      "Emulation.setDeviceMetricsOverride",
      {
        // The desktop shell's decorative monitor aperture is intentionally
        // wider than the selected game output.  Video evidence uses the
        // shell's responsive layout, where the viewport itself is the exact
        // selected 4:3 retail surface, instead of recording that stretched
        // decorative aperture.
        width: showcaseCapture ? 2560 : options.video === undefined ? 1440 : 332,
        height: showcaseCapture ? 1200 : 1100,
        deviceScaleFactor: 1,
        mobile: false,
      },
      sessionId,
    );
    await cdp.command(
      "Page.addScriptToEvaluateOnNewDocument",
      {
        source: `try {
          const preserveReloadedStorage = ${options.auditCardRoundTrip};
          const initialized =
            sessionStorage.getItem(${JSON.stringify(STORAGE_RELOAD_SENTINEL)}) === "1";
          if (!preserveReloadedStorage || !initialized) {
            localStorage.clear();
            sessionStorage.clear();
            const seeds = ${JSON.stringify(storageSeeds)};
            for (const [key, value] of Object.entries(seeds)) {
              localStorage.setItem(key, value);
            }
            if (preserveReloadedStorage) {
              sessionStorage.setItem(
                ${JSON.stringify(STORAGE_RELOAD_SENTINEL)},
                "1",
              );
            }
          }
          window.__crustBrowserSmokeFresh = !initialized;
        } catch (error) {
          throw new Error("browser smoke could not initialize local storage");
        }`
          + runtimeLogHistoryInstrumentation
          + (options.syntheticCookedIsoImport ? blobRangeInstrumentation : ""),
      },
      sessionId,
    );
    await cdp.command(
      "Storage.clearDataForOrigin",
      {
        origin: pageOrigin,
        storageTypes:
          "local_storage,indexeddb,cache_storage,service_workers",
      },
      sessionId,
    );
    await cdp.command("Page.navigate", { url: options.url }, sessionId);
    await waitFor(
      cdp,
      sessionId,
      "harness bootstrap",
      (snapshot) =>
        snapshot.bootstrap === "running" &&
        snapshot.harness?.mode === "manual-34ms",
      failures,
      30_000,
    );
    const expectedStorageKeys = Object.keys(storageSeeds).sort();
    const observedStorageKeys = await evaluate(
      cdp,
      sessionId,
      `(${JSON.stringify(STORAGE_KEYS)})
        .filter((key) => localStorage.getItem(key) !== null)
        .sort()`,
    );
    if (JSON.stringify(observedStorageKeys) !== JSON.stringify(expectedStorageKeys)) {
      throw new Error(
        `fresh browser profile storage keys differ from the requested local seeds: `
        + `expected ${expectedStorageKeys.join(", ") || "none"}; `
        + `observed ${observedStorageKeys.join(", ") || "none"}`,
      );
    }
    const mismatchedStorageKeys = await evaluate(
      cdp,
      sessionId,
      `Object.entries(${JSON.stringify(storageSeeds)})
        .filter(([key, value]) => localStorage.getItem(key) !== value)
        .map(([key]) => key)`,
    );
    if (mismatchedStorageKeys.length > 0) {
      throw new Error(
        `browser profile did not retain the exact local seed for: `
        + mismatchedStorageKeys.join(", "),
      );
    }
    if (options.syntheticCookedIsoImport) {
      await evaluate(
        cdp,
        sessionId,
        `(() => {
          const evidence = window.__crustBrowserSmokeBlobEvidence;
          if (!evidence) throw new Error("Blob range instrumentation is unavailable");
          evidence.blobRanges.length = 0;
          evidence.arrayBufferSizes.length = 0;
        })()`,
      );
    }
    const importNetworkRequestStart = networkRequests.length;
    const importLocalAssets = async (description) => {
      const { root } = await cdp.command(
        "DOM.getDocument",
        { depth: -1 },
        sessionId,
      );
      const { nodeId } = await cdp.command(
        "DOM.querySelector",
        { nodeId: root.nodeId, selector: "#gameFiles" },
        sessionId,
      );
      if (!nodeId) throw new Error("browser harness is missing #gameFiles");
      await cdp.command(
        "DOM.setFileInputFiles",
        { files: options.assets, nodeId },
        sessionId,
        30_000,
      );
      return waitFor(
        cdp,
        sessionId,
        description,
        (snapshot) =>
          snapshot.pairCount > 0 &&
          !snapshot.launchDisabled &&
          snapshot.runtimeState === "idle",
        failures,
        120_000,
      );
    };
    const imported = await importLocalAssets("local game-file import");

    if (options.syntheticCookedIsoImport) {
      // Leave a brief quiet window after the UI reaches idle so deferred upload
      // or fetch behavior cannot escape the post-selection network assertion.
      await delay(100);
      const finalImportSnapshot = await browserSnapshot(cdp, sessionId);
      const blobEvidence = await evaluate(
        cdp,
        sessionId,
        `(() => {
          const evidence = window.__crustBrowserSmokeBlobEvidence;
          return {
            blobRanges: evidence?.blobRanges.map((range) => ({ ...range })) ?? null,
            arrayBufferSizes: [...(evidence?.arrayBufferSizes ?? [])]
          };
        })()`,
      );
      const evidence = {
        ...blobEvidence,
        networkRequests: networkRequests.slice(importNetworkRequestStart),
      };
      const problems = syntheticCookedIsoImportFailures(
        finalImportSnapshot,
        evidence,
      );
      if (problems.length > 0) {
        throw new Error(
          `synthetic cooked-ISO import failed:\n${problems.join("\n")}`,
        );
      }
      return {
        assets: options.assets.length,
        files: finalImportSnapshot.fileCount,
        pairs: finalImportSnapshot.pairCount,
        layout: "ISO 2048",
        imageBytes: SYNTHETIC_COOKED_ISO_BYTES,
        blobRanges: evidence.blobRanges,
        arrayBufferSizes: evidence.arrayBufferSizes,
        postSelectionNetworkRequests: evidence.networkRequests.length,
        launched: false,
      };
    }

    const targetAvailable = await evaluate(
      cdp,
      sessionId,
      `Boolean(document.querySelector('#bootLevel option[value="${replay.bootLid}"]'))`,
    );
    if (!targetAvailable) {
      const optionsAvailable = await evaluate(
        cdp,
        sessionId,
        `[...document.querySelectorAll("#bootLevel option")].map(
          (option) => ({ value: option.value, label: option.textContent })
        )`,
      );
      throw new Error(
        `boot LID 0x${replay.bootLid.toString(16)} is unavailable; mounted options: ` +
          JSON.stringify(optionsAvailable),
      );
    }
    await evaluate(
      cdp,
      sessionId,
      `(() => {
        document.querySelector("#unlockAll").checked = ${replay.unlockAll};
        document.querySelector("#bootLevel").value = "${replay.bootLid}";
        document.querySelector("#outputAspect").value = ${JSON.stringify(options.outputAspect)};
        document.querySelector("#renderResolution").value = ${JSON.stringify(options.renderResolution)};
        document.querySelector("#cameraZoom").value = ${JSON.stringify(options.cameraZoom)};
        document.querySelector("#smoothMotion").checked = ${options.smoothMotion};
        document.querySelector("#extendedWorld").checked = ${options.extendedWorld};
        const harness = window.__crustTest;
        if (typeof harness?.syncPresentationFromControls !== "function") {
          throw new Error("browser presentation sync hook is unavailable");
        }
        harness.syncPresentationFromControls();
        if (harness.lastError != null) throw new Error(harness.lastError);
        document.querySelector("#launch").click();
      })()`,
    );
    await waitFor(
      cdp,
      sessionId,
      "runtime launch",
      (snapshot) => snapshot.runtimeState === "running",
      failures,
      120_000,
    );
    if (options.video !== undefined || showcaseCapture) {
      if (!replay.composition?.phases) {
        throw new Error(
          "video capture requires composed replay phase metadata for its chapter list",
        );
      }
      videoRecorder = showcaseCapture
        ? await SourceFrameWindowRecorder.create(options, cdp, sessionId)
        : await SourceFrameVideoRecorder.create(options, cdp, sessionId);
    }
    if (options.auditIsolatedRetailPbakLid !== undefined) {
      const queueResult = await evaluate(
        cdp,
        sessionId,
        `(() => {
          const harness = window.__crustTest;
          if (typeof harness?.queueTitleAttractMount !== "function") {
            return { error: "browser Title-attract mount hook is unavailable" };
          }
          harness.queueTitleAttractMount(${options.auditIsolatedRetailPbakLid});
          return { error: harness.lastError ?? null };
        })()`,
      );
      if (queueResult?.error != null) {
        throw new Error(
          `could not queue isolated retail PBAK mount: ${queueResult.error}`,
        );
      }
    }
    let stepped = 0;
    let hostCallbacks = 0;
    let zeroStepHostCallbacks = 0;
    let maximumConsecutiveZeroStepCallbacks = 0;
    // Manual harness mode cannot publish runtime globals until its first
    // cooperative step. Check that first result before the replay can
    // continue far enough to spend a life legitimately.
    let allLevelsLaunchChecked = !replay.unlockAll;
    let allLevelsLaunchEvidence = null;
    let finalSnapshot = await browserSnapshot(cdp, sessionId);
    let observedRetailExecution = retailExecutionObserved(false, finalSnapshot);
    let replayCurrentLid = finalSnapshot.debug?.currentLid;
    let replayMountedLid = finalSnapshot.debug?.mountedLid;
    const replayFrameLimit = options.replay !== undefined && options.framesExplicit
      ? options.frames
      : undefined;
    const stepReplayBatch = async (inputKind, input, frameCount, label) => {
      const sourceFrame = stepped;
      const captureSourceFrame = videoRecorder instanceof SourceFrameWindowRecorder
        ? videoRecorder.needsCapture(sourceFrame)
        : videoRecorder !== undefined;
      const batchStart = stepped + 1;
      const stepMethod = replayStepMethod(inputKind);
      const stepArguments = replayStepArguments(inputKind, input);
      let callbackSteps = [];
      let result;
      let mountedDestination = false;
      if (expectedRetailPbakEids !== null) {
        // MutationObserver evidence is delivered at a browser microtask
        // checkpoint. Run and yield exactly one host callback at a time in
        // audit mode so every log line captures that callback's counters,
        // including callbacks that only advance a blocked physical NSOpen.
        while (
          summarizeReplayHostCallbacks(callbackSteps, frameCount).executed
            < frameCount
        ) {
          const callbackResult = await evaluate(
            cdp,
            sessionId,
            `(async () => {
              const harness = window.__crustTest;
              const beforeStepCount = harness.stepCount;
              harness.${stepMethod}(${stepArguments});
              const simulationStepped = harness.lastSimulationStepped;
              const stepDelta = harness.stepCount - beforeStepCount;
              if (
                typeof simulationStepped !== "boolean"
                || stepDelta !== (simulationStepped ? 1 : 0)
              ) {
                throw new Error(
                  "browser harness cooperative-step accounting diverged"
                );
              }
              await new Promise((resolve) => setTimeout(resolve, 0));
              return {
                simulationStepped,
                snapshot: ${snapshotExpression},
                sourceFrameJpeg: simulationStepped && ${captureSourceFrame}
                  ? document.querySelector("#canvas").toDataURL("image/jpeg", 0.82)
                  : null,
                sourceFrameAudio: simulationStepped && ${captureSourceFrame && showcaseCapture}
                  ? (() => {
                      const bytes = window.__crustTest.takeAudioFramePcm16();
                      if (!(bytes instanceof Uint8Array)) return null;
                      let binary = "";
                      for (let offset = 0; offset < bytes.length; offset += 0x4000) {
                        binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x4000));
                      }
                      return btoa(binary);
                    })()
                  : null
              };
            })()`,
          );
          callbackSteps.push(callbackResult?.simulationStepped);
          // Validate and enforce the zero-step bound after every callback;
          // this keeps a stalled pager from turning an audit into an
          // unbounded CDP loop.
          summarizeReplayHostCallbacks(callbackSteps, frameCount);
          result = callbackResult;
          if (
            result?.snapshot?.harness?.lastError != null
            || result?.snapshot?.harness?.lastRequestedLid != null
          ) {
            break;
          }
        }
      } else {
        result = await evaluate(
          cdp,
          sessionId,
          `(() => {
            const callbackSteps = [];
            let executed = 0;
            let consecutiveZeroSteps = 0;
            while (executed < ${frameCount}) {
              const harness = window.__crustTest;
              const beforeStepCount = harness.stepCount;
              harness.${stepMethod}(${stepArguments});
              const simulationStepped = harness.lastSimulationStepped;
              const stepDelta = harness.stepCount - beforeStepCount;
              if (
                typeof simulationStepped !== "boolean"
                || stepDelta !== (simulationStepped ? 1 : 0)
              ) {
                throw new Error(
                  "browser harness cooperative-step accounting diverged"
                );
              }
              callbackSteps.push(simulationStepped);
              if (simulationStepped) {
                executed += 1;
                consecutiveZeroSteps = 0;
              } else {
                consecutiveZeroSteps += 1;
                if (consecutiveZeroSteps > ${REPLAY_ZERO_STEP_CALLBACK_LIMIT}) {
                  break;
                }
              }
              if (
                harness.lastError != null
                || harness.lastRequestedLid != null
              ) {
                break;
              }
            }
            return {
              callbackSteps,
              snapshot: ${snapshotExpression},
              sourceFrameJpeg: executed > 0 && ${captureSourceFrame}
                ? document.querySelector("#canvas").toDataURL("image/jpeg", 0.82)
                : null,
              sourceFrameAudio: executed > 0 && ${captureSourceFrame && showcaseCapture}
                ? (() => {
                    const bytes = window.__crustTest.takeAudioFramePcm16();
                    if (!(bytes instanceof Uint8Array)) return null;
                    let binary = "";
                    for (let offset = 0; offset < bytes.length; offset += 0x4000) {
                      binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x4000));
                    }
                    return btoa(binary);
                  })()
                : null
            };
          })()`,
        );
        callbackSteps = result?.callbackSteps;
      }
      let progress;
      try {
        progress = summarizeReplayHostCallbacks(callbackSteps, frameCount);
      } catch (error) {
        throw new Error(
          `${label}: ${error.message}; last snapshot:\n${JSON.stringify(result?.snapshot, null, 2)}`,
        );
      }
      const { executed } = progress;
      zeroStepHostCallbacks += progress.hostCallbacks - executed;
      maximumConsecutiveZeroStepCallbacks = Math.max(
        maximumConsecutiveZeroStepCallbacks,
        progress.maximumConsecutiveZeroSteps,
      );
      finalSnapshot = result.snapshot;
      observedRetailExecution = retailExecutionObserved(
        observedRetailExecution,
        finalSnapshot,
      );
      if (Number.isSafeInteger(finalSnapshot.debug?.currentLid)) {
        replayCurrentLid = finalSnapshot.debug.currentLid;
      }
      if (Number.isSafeInteger(finalSnapshot.debug?.mountedLid)) {
        replayMountedLid = finalSnapshot.debug.mountedLid;
      }
      const expectedHostCallbackCount = hostCallbacks + progress.hostCallbacks;
      if (finalSnapshot.harness.hostCallbackCount !== expectedHostCallbackCount) {
        throw new Error(
          `${label} issued ${finalSnapshot.harness.hostCallbackCount} host callbacks; expected ${expectedHostCallbackCount}`,
        );
      }
      const expectedStepCount = stepped + executed;
      if (finalSnapshot.harness.stepCount !== expectedStepCount) {
        throw new Error(
          `${label} issued ${finalSnapshot.harness.stepCount} cooperative steps; expected ${expectedStepCount}`,
        );
      }
      hostCallbacks = expectedHostCallbackCount;
      stepped += executed;
      if (!allLevelsLaunchChecked && executed > 0) {
        if (executed !== 1) {
          throw new Error(
            `all-level browser launch check requires one isolated first frame; received ${executed}`,
          );
        }
        const startupProblems = allLevelsFailures(finalSnapshot, {
          requireStartingLives: true,
          requireLivePlayer: replay.bootLid !== DEFAULT_BOOT_LID,
        });
        if (startupProblems.length > 0) {
          throw new Error(
            `all-level browser launch assertion failed:\n${startupProblems.join("\n")}`,
          );
        }
        allLevelsLaunchEvidence = {
          frame: finalSnapshot.debug.frame,
          currentLid: finalSnapshot.debug.currentLid,
          lifeCount: finalSnapshot.debug.browserTestGlobals?.lifeCount ?? null,
          playerLifeCount: finalSnapshot.debug.playerLifeCount,
          initialLifeCount:
            finalSnapshot.debug.browserTestGlobals?.initialLifeCount ?? null,
          levelsUnlocked:
            finalSnapshot.debug.browserTestGlobals?.levelsUnlocked ?? null,
          itemPool2: finalSnapshot.debug.browserTestGlobals?.itemPool2 ?? null,
        };
        allLevelsLaunchChecked = true;
      }
      const problems = [
        ...failures,
        ...snapshotFailures(finalSnapshot),
      ];
      if (problems.length > 0) {
        throw new Error(
          `${label} failed after frames ${batchStart}-${stepped}:\n${problems.join("\n")}`,
        );
      }
      if (captureSourceFrame && executed > 0) {
        if (executed !== 1) {
          throw new Error(
            `${label} executed ${executed} frames while per-source-frame capture requires one`,
          );
        }
        if (videoRecorder instanceof SourceFrameWindowRecorder) {
          await videoRecorder.capture(
            sourceFrame,
            result.sourceFrameJpeg,
            result.sourceFrameAudio,
          );
        } else {
          await videoRecorder.capture(result.sourceFrameJpeg);
        }
      }
      if (finalSnapshot.harness.lastRequestedLid != null) {
        const requestedLid = Number(finalSnapshot.harness.lastRequestedLid);
        const previousRuntimeLog = finalSnapshot.runtimeLog ?? "";
        finalSnapshot = await waitFor(
          cdp,
          sessionId,
          `destination 0x${requestedLid.toString(16)} mount`,
          (snapshot) =>
            destinationMountReady(snapshot, requestedLid, previousRuntimeLog),
          failures,
          120_000,
        );
        observedRetailExecution = retailExecutionObserved(
          observedRetailExecution,
          finalSnapshot,
        );
        replayCurrentLid = requestedLid;
        replayMountedLid = requestedLid;
        mountedDestination = true;
      }
      return validateReplayBatchExecution(executed, {
        mountedDestination,
        label,
      });
    };
    const stepReplayFrame = async (inputKind, held, label) => {
      const executed = await stepReplayBatch(inputKind, held, 1, label);
      if (executed > 1) {
        throw new Error(`${label} executed more than one frame`);
      }
      return executed;
    };
    const readRetailPbakLogEvidence = async () => {
      const entries = await evaluate(
        cdp,
        sessionId,
        `window.__crustBrowserSmokeRuntimeLogEvidence?.entries.map(
          (entry) => ({ ...entry })
        ) ?? null`,
      );
      if (!Array.isArray(entries)) {
        throw new Error("retail PBAK browser-log evidence is unavailable");
      }
      return entries;
    };
    const settleExpectation = async (
      expectation,
      maximumFrames,
      inputKind,
      held,
      label,
    ) => {
      let used = 0;
      while (
        used < maximumFrames
        && (replayFrameLimit === undefined || stepped < replayFrameLimit)
        && expectationFailures(expectation, finalSnapshot).length > 0
      ) {
        used += await stepReplayFrame(inputKind, held, label);
      }
      return used;
    };
    if (options.auditCardRoundTrip) {
      const stepUntil = async (predicate, maximumFrames, label) => {
        for (let used = 0; used < maximumFrames; used += 1) {
          if (predicate(finalSnapshot)) return used;
          await stepReplayFrame(PHYSICAL_INPUT_KIND, 0, label);
        }
        if (predicate(finalSnapshot)) return maximumFrames;
        throw new Error(
          `${label} did not reach its authored boundary in ${maximumFrames} frames:\n`
          + JSON.stringify(finalSnapshot, null, 2),
        );
      };

      const queued = await evaluate(
        cdp,
        sessionId,
        `(() => {
          const harness = window.__crustTest;
          if (typeof harness?.queueCardSaveScreen !== "function") {
            return { error: "browser card-save hook is unavailable" };
          }
          harness.queueCardSaveScreen();
          return { error: harness.lastError ?? null };
        })()`,
      );
      if (queued?.error != null) {
        throw new Error(`could not queue authored card save: ${queued.error}`);
      }
      await stepUntil(
        (snapshot) =>
          snapshot.debug?.titleState === 13
          && snapshot.debug?.retailMain?.state === 24
          && snapshot.debug?.retailMain?.pc === 2_273,
        320,
        "authored CardC save-screen entry",
      );
      const cardBeforeSave = await evaluate(
        cdp,
        sessionId,
        `localStorage.getItem(${JSON.stringify(CARD_STORAGE_KEY)})`,
      );
      if (cardBeforeSave !== null) {
        throw new Error("card storage changed before CardC received save confirmation");
      }

      await stepReplayFrame(PHYSICAL_INPUT_KIND, PAD_CROSS, "CardC save confirmation");
      await stepReplayFrame(PHYSICAL_INPUT_KIND, 0, "CardC save confirmation release");
      let authoredCardJson = null;
      for (let frame = 0; frame < 256 && authoredCardJson === null; frame += 1) {
        await stepReplayFrame(PHYSICAL_INPUT_KIND, 0, "CardC authored save write");
        authoredCardJson = await evaluate(
          cdp,
          sessionId,
          `localStorage.getItem(${JSON.stringify(CARD_STORAGE_KEY)})`,
        );
      }
      const cardWriteProblems = cardRoundTripStorageFailures(authoredCardJson);
      if (cardWriteProblems.length > 0) {
        throw new Error(
          `authored browser card write failed:\n${cardWriteProblems.join("\n")}`,
        );
      }
      finalSnapshot = await browserSnapshot(cdp, sessionId);
      if (finalSnapshot.cardState !== "1 / 15") {
        throw new Error(
          `authored card UI published ${JSON.stringify(finalSnapshot.cardState)} instead of one slot`,
        );
      }
      const saveSessionFrames = stepped;
      const saveSessionHostCallbacks = hostCallbacks;

      const preReloadStorage = await evaluate(
        cdp,
        sessionId,
        `(() => {
          localStorage.removeItem(${JSON.stringify(RESUME_STORAGE_KEY)});
          return {
            card: localStorage.getItem(${JSON.stringify(CARD_STORAGE_KEY)}),
            resume: localStorage.getItem(${JSON.stringify(RESUME_STORAGE_KEY)})
          };
        })()`,
      );
      if (preReloadStorage.card !== authoredCardJson || preReloadStorage.resume !== null) {
        throw new Error("could not isolate the authored card record before reload");
      }

      await reloadPage(cdp, sessionId);
      await waitFor(
        cdp,
        sessionId,
        "post-save page reload",
        (snapshot) =>
          snapshot.bootstrap === "running"
          && snapshot.harness?.mode === "manual-34ms",
        failures,
        30_000,
      );
      const reloadedStorage = await evaluate(
        cdp,
        sessionId,
        `({
          card: localStorage.getItem(${JSON.stringify(CARD_STORAGE_KEY)}),
          resume: localStorage.getItem(${JSON.stringify(RESUME_STORAGE_KEY)}),
          sentinel: sessionStorage.getItem(${JSON.stringify(STORAGE_RELOAD_SENTINEL)}),
          fresh: window.__crustBrowserSmokeFresh
        })`,
      );
      if (
        reloadedStorage.card !== authoredCardJson
        || reloadedStorage.sentinel !== "1"
        || reloadedStorage.fresh !== false
      ) {
        throw new Error(
          "Page.reload did not retain the exact authored card record and reload sentinel: "
          + JSON.stringify({
            cardMatches: reloadedStorage.card === authoredCardJson,
            sentinel: reloadedStorage.sentinel,
            fresh: reloadedStorage.fresh,
          }),
        );
      }
      const resumeWriteProblems = resumeRoundTripStorageFailures(
        reloadedStorage.resume,
      );
      if (resumeWriteProblems.length > 0) {
        throw new Error(
          `Page.reload lifecycle resume failed:\n${resumeWriteProblems.join("\n")}`,
        );
      }
      await evaluate(
        cdp,
        sessionId,
        `localStorage.removeItem(${JSON.stringify(RESUME_STORAGE_KEY)})`,
      );

      const reimported = await importLocalAssets(
        "local game-file re-import after Page.reload",
      );
      if (reimported.pairCount !== imported.pairCount) {
        throw new Error(
          `reload re-import found ${reimported.pairCount} pairs; expected ${imported.pairCount}`,
        );
      }
      const storageAfterReimport = await evaluate(
        cdp,
        sessionId,
        `({
          card: localStorage.getItem(${JSON.stringify(CARD_STORAGE_KEY)}),
          resume: localStorage.getItem(${JSON.stringify(RESUME_STORAGE_KEY)})
        })`,
      );
      if (
        storageAfterReimport.card !== authoredCardJson
        || storageAfterReimport.resume !== null
      ) {
        throw new Error("local asset re-import changed the isolated card record");
      }

      await evaluate(
        cdp,
        sessionId,
        `(() => {
          document.querySelector("#unlockAll").checked = false;
          document.querySelector("#bootLevel").value = "${DEFAULT_BOOT_LID}";
          document.querySelector("#launch").click();
        })()`,
      );
      await waitFor(
        cdp,
        sessionId,
        "post-reload Title launch",
        (snapshot) => snapshot.runtimeState === "running",
        failures,
        120_000,
      );
      stepped = 0;
      hostCallbacks = 0;
      zeroStepHostCallbacks = 0;
      maximumConsecutiveZeroStepCallbacks = 0;
      allLevelsLaunchChecked = true;
      finalSnapshot = await browserSnapshot(cdp, sessionId);
      observedRetailExecution = retailExecutionObserved(false, finalSnapshot);
      replayCurrentLid = finalSnapshot.debug?.currentLid;
      replayMountedLid = finalSnapshot.debug?.mountedLid;

      await stepUntil(
        (snapshot) =>
          snapshot.debug?.titleState === 5
          && snapshot.debug?.mountedLid === DEFAULT_BOOT_LID,
        2_000,
        "fresh Title MainMenu",
      );
      await stepReplayBatch(
        PHYSICAL_INPUT_KIND,
        0,
        32,
        "fresh MainMenu ready dwell",
      );
      await stepReplayFrame(PHYSICAL_INPUT_KIND, PAD_DOWN, "MainMenu Load selection");
      await stepReplayFrame(PHYSICAL_INPUT_KIND, 0, "MainMenu Load selection release");
      await stepReplayFrame(PHYSICAL_INPUT_KIND, PAD_CROSS, "MainMenu Load confirmation");
      await stepReplayFrame(PHYSICAL_INPUT_KIND, 0, "MainMenu Load confirmation release");
      await stepUntil(
        (snapshot) =>
          snapshot.debug?.titleState === 14
          && snapshot.cardState === "1 / 15",
        256,
        "authored Load screen and card rescan",
      );
      await stepReplayBatch(
        PHYSICAL_INPUT_KIND,
        0,
        96,
        "authored Load screen ready dwell",
      );
      await stepReplayFrame(PHYSICAL_INPUT_KIND, PAD_CROSS, "CardC LoadSelected confirmation");
      await stepReplayFrame(PHYSICAL_INPUT_KIND, 0, "CardC LoadSelected release");
      await stepUntil(
        (snapshot) =>
          snapshot.debug?.titleState === 15
          && snapshot.runtimeLog.includes(
            "Restored retail progression and audio options from the selected virtual-card slot.",
          ),
        256,
        "CardC load-to-Map handoff",
      );
      const globals = finalSnapshot.debug?.browserTestGlobals;
      for (const [name, expected] of Object.entries({
        initialLifeCount: 7 << 8,
        lifeCount: 7 << 8,
        levelsUnlocked: 8,
        gemCount: 1,
        keyCount: 0,
        itemPool1: 0x2000_0000,
        itemPool2: 0,
      })) {
        if (globals?.[name] !== expected) {
          throw new Error(
            `card-loaded ${name} is ${JSON.stringify(globals?.[name])}; expected ${expected}`,
          );
        }
      }
      const cardAfterLoad = await evaluate(
        cdp,
        sessionId,
        `localStorage.getItem(${JSON.stringify(CARD_STORAGE_KEY)})`,
      );
      if (cardAfterLoad !== authoredCardJson) {
        throw new Error("LoadSelected mutated the persisted card bytes or timestamps");
      }

      await stepReplayBatch(
        PHYSICAL_INPUT_KIND,
        0,
        150,
        "restored Map camera settle",
      );
      await stepReplayFrame(PHYSICAL_INPUT_KIND, PAD_CROSS, "restored Hog Wild selection");
      await stepReplayFrame(
        PHYSICAL_INPUT_KIND,
        0,
        "restored Hog Wild selection release",
      );
      await stepUntil(
        (snapshot) =>
          snapshot.debug?.currentLid === 0x11
          && snapshot.debug?.mountedLid === 0x11,
        256,
        "restored Hog Wild gameplay mount",
      );
      const gameplayMountExecutions = finalSnapshot.debug?.retailExecutions;
      if (
        !Number.isSafeInteger(gameplayMountExecutions)
        || gameplayMountExecutions < 0
      ) {
        throw new Error(
          `Hog Wild mount has an invalid execution count: ${JSON.stringify(gameplayMountExecutions)}`,
        );
      }
      await stepUntil(
        (snapshot) =>
          retailGameplayReadyAfterMount(snapshot, 0x11, gameplayMountExecutions),
        512,
        "restored Hog Wild live gameplay",
      );
      await stepReplayBatch(
        PHYSICAL_INPUT_KIND,
        0,
        90,
        "restored Hog Wild opening presentation dwell",
      );
      if (
        !retailGameplayReadyAfterMount(
          finalSnapshot,
          0x11,
          gameplayMountExecutions,
        )
      ) {
        throw new Error(
          `Hog Wild stopped being gameplay-ready during its opening presentation: ${JSON.stringify({
            runtimeState: finalSnapshot.runtimeState,
            currentLid: finalSnapshot.debug?.currentLid,
            mountedLid: finalSnapshot.debug?.mountedLid,
            retailExecutions: finalSnapshot.debug?.retailExecutions,
            livePlayers: finalSnapshot.debug?.browserTestObjects?.filter(
              (object) => object?.player === true && object?.faulted !== true,
            ).length,
          })}`,
        );
      }
      const finalCardJson = await evaluate(
        cdp,
        sessionId,
        `localStorage.getItem(${JSON.stringify(CARD_STORAGE_KEY)})`,
      );
      if (finalCardJson !== authoredCardJson) {
        throw new Error("gameplay mount changed the persisted card record");
      }
      const finalProblems = [...failures, ...snapshotFailures(finalSnapshot)];
      if (finalProblems.length > 0) {
        throw new Error(`browser card round trip failed:\n${finalProblems.join("\n")}`);
      }

      const screenshot = await cdp.command(
        "Page.captureScreenshot",
        { format: "png", captureBeyondViewport: true },
        sessionId,
        30_000,
      );
      const screenshotBytes = Buffer.from(screenshot.data, "base64");
      await mkdir(dirname(options.screenshot), { recursive: true });
      await writeFile(options.screenshot, screenshotBytes);
      return {
        assets: options.assets.length,
        pairs: imported.pairCount,
        cardRoundTrip: true,
        pageReloads: 1,
        payloadBytes: STORAGE_PAYLOAD_BYTES,
        occupiedSlots: 1,
        lifecycleResumeReauthored: true,
        resumeRemovedBeforeLoad: true,
        saveSessionFrames,
        saveSessionHostCallbacks,
        loadSessionFrames: stepped,
        loadSessionHostCallbacks: hostCallbacks,
        currentLid: finalSnapshot.debug.currentLid,
        mountedLid: finalSnapshot.debug.mountedLid,
        gameplayReady: true,
        postMountRetailExecutions:
          finalSnapshot.debug.retailExecutions - gameplayMountExecutions,
        livePlayerObjects: finalSnapshot.debug.browserTestObjects.filter(
          (object) => object?.player === true && object?.faulted !== true,
        ).length,
        browserEventFailures: failures.length,
        windowConsoleErrors: finalSnapshot.consoleErrors.length,
        glError: finalSnapshot.debug.glError,
        retailExecutionErrors: finalSnapshot.debug.retailExecutionErrors,
        retailFaultedObjects: finalSnapshot.debug.retailFaultedObjects,
        retailZoneEventFailures: finalSnapshot.debug.retailZoneEventFailures,
        screenshot: options.screenshot,
        screenshotSha256: createHash("sha256")
          .update(screenshotBytes)
          .digest("hex"),
      };
    }
    let directBonusReadyFrames = 0;
    let directBonusCeremonyFrames = 0;
    if (options.auditDirectBonusReturn) {
      // A newly launched manual runtime has not published its first metrics
      // snapshot yet; its cumulative execution baseline is exactly zero.
      const mountExecutions = finalSnapshot.debug?.retailExecutions ?? 0;
      if (!Number.isSafeInteger(mountExecutions) || mountExecutions < 0) {
        throw new Error(
          `direct-bonus mount has an invalid execution count: ${JSON.stringify(mountExecutions)}`,
        );
      }
      while (
        directBonusReadyFrames < 512
        && !retailGameplayReadyAfterMount(
          finalSnapshot,
          DIRECT_BONUS_AUDIT_LID,
          mountExecutions,
        )
      ) {
        directBonusReadyFrames += await stepReplayFrame(
          PHYSICAL_INPUT_KIND,
          0,
          "direct-bonus live WillC readiness",
        );
      }
      if (!retailGameplayReadyAfterMount(
        finalSnapshot,
        DIRECT_BONUS_AUDIT_LID,
        mountExecutions,
      )) {
        throw new Error(
          `direct-bonus audit did not reach a live WillC player in 512 frames:\n${JSON.stringify(finalSnapshot, null, 2)}`,
        );
      }
      const queued = await evaluate(
        cdp,
        sessionId,
        `(() => {
          const harness = window.__crustTest;
          if (typeof harness?.queueDirectBonusState32Boundary !== "function") {
            return { error: "browser direct-bonus state-32 hook is unavailable" };
          }
          harness.queueDirectBonusState32Boundary();
          return { error: harness.lastError ?? null };
        })()`,
      );
      if (queued?.error != null) {
        throw new Error(
          `could not queue the separately proven direct-bonus state-32 boundary: ${queued.error}`,
        );
      }
      // WillC state 32 performs the native bonus-key ceremony before CardC
      // accepts input. The existing legally-local runtime golden reaches its
      // first ready Cross edge on tick 300. Preserve the exact physical edge
      // sequence, including the occupied-card overwrite tail; an empty card
      // returns on the first confirmation and the loop stops at the mount.
      directBonusCeremonyFrames += await stepReplayBatch(
        PHYSICAL_INPUT_KIND,
        0,
        300,
        "direct-bonus authored key ceremony",
      );
      for (const [held, label] of [
        [PAD_CROSS, "direct-bonus CardC initial confirmation"],
        [0, "direct-bonus CardC initial confirmation release"],
        [0, "direct-bonus CardC overwrite-dialog dwell"],
        [PAD_DOWN, "direct-bonus CardC overwrite Yes selection"],
        [0, "direct-bonus CardC overwrite Yes release"],
        [PAD_CROSS, "direct-bonus CardC overwrite confirmation"],
        [0, "direct-bonus CardC overwrite confirmation release"],
      ]) {
        if (finalSnapshot.debug?.currentLid !== DIRECT_BONUS_AUDIT_LID) break;
        directBonusCeremonyFrames += await stepReplayFrame(
          PHYSICAL_INPUT_KIND,
          held,
          label,
        );
      }
    }
    let segmentSettleFramesUsed = 0;
    let skippedReplayFrames = 0;
    const segmentTrace = [];
    let retailPbakAuditComplete = false;
    const videoPhaseByFirstSegment = new Map(
      (replay.composition?.phases ?? []).map((phase) => [
        phase.firstSegment,
        phase,
      ]),
    );
    let replayStoppedAtFrameLimit = false;
    replaySegments:
    for (const [segmentIndex, segment] of replay.segments.entries()) {
      if (replayFrameLimit !== undefined && stepped >= replayFrameLimit) {
        replayStoppedAtFrameLimit = true;
        break;
      }
      const videoPhase = videoPhaseByFirstSegment.get(segmentIndex + 1);
      if (videoPhase) {
        videoRecorder?.startChapter(videoPhase.id, segmentIndex + 1, stepped);
      }
      let remainingFrames = segment.frames;
      while (remainingFrames > 0) {
        if (replayFrameLimit !== undefined && stepped >= replayFrameLimit) {
          replayStoppedAtFrameLimit = true;
          break;
        }
        if (
          replayLidConditionKnown(
            segment.while,
            replayCurrentLid,
            replayMountedLid,
          )
          && !replayLidConditionMatches(
            segment.while,
            replayCurrentLid,
            replayMountedLid,
          )
        ) {
          skippedReplayFrames += remainingFrames;
          remainingFrames = 0;
          break;
        }
        let batchFrames = nextReplayBatchFrameCount(remainingFrames, {
          isolateFirstFrame: !allLevelsLaunchChecked,
        });
        if (replayFrameLimit !== undefined) {
          batchFrames = Math.min(batchFrames, replayFrameLimit - stepped);
        }
        if (videoRecorder instanceof SourceFrameVideoRecorder) {
          batchFrames = 1;
        } else if (videoRecorder instanceof SourceFrameWindowRecorder) {
          batchFrames = videoRecorder.batchFrameCount(stepped, batchFrames);
        }
        const executed = await stepReplayBatch(
          segment.inputKind,
          segment.inputKind === SNAPSHOT_INPUT_KIND ? segment : segment.held,
          batchFrames,
          "browser replay",
        );
        remainingFrames -= executed;
        if (expectedRetailPbakEids !== null) {
          const logEvidence = await readRetailPbakLogEvidence();
          let partialEvidence;
          try {
            partialEvidence = parseRetailPbakEvidence(logEvidence, {
              allowIncomplete: true,
            });
          } catch (error) {
            throw new Error(
              `${error.message}; recent evidence: ${JSON.stringify(logEvidence.slice(-12))}`,
            );
          }
          if (
            retailPbakAuditCoverageComplete(
              partialEvidence,
              expectedRetailPbakEids,
              finalSnapshot,
            )
          ) {
            skippedReplayFrames += remainingFrames;
            remainingFrames = 0;
            retailPbakAuditComplete = true;
            break replaySegments;
          }
        }
      }
      if (
        replayStoppedAtFrameLimit
        || (replayFrameLimit !== undefined && stepped >= replayFrameLimit)
      ) {
        replayStoppedAtFrameLimit = true;
        break;
      }
      const settleFramesUsed = await settleExpectation(
        segment.expect,
        segment.settleFrames,
        segment.inputKind,
        segment.settleHeld,
        `browser replay segment ${segmentIndex + 1} settle`,
      );
      segmentSettleFramesUsed += settleFramesUsed;
      if (
        replayFrameLimit !== undefined
        && stepped >= replayFrameLimit
        && expectationFailures(segment.expect, finalSnapshot).length > 0
      ) {
        replayStoppedAtFrameLimit = true;
        break;
      }
      assertExpected(
        segment.expect,
        finalSnapshot,
        `segment ${segmentIndex + 1}`,
      );
      if (
        replay.traceFromSegment !== undefined
        && segmentIndex + 1 >= replay.traceFromSegment
      ) {
        segmentTrace.push({
          segment: segmentIndex + 1,
          stepped,
          hostCallbacks,
          settleFramesUsed,
          held: segment.held,
          currentLid: finalSnapshot.debug?.currentLid,
          mountedLid: finalSnapshot.debug?.mountedLid,
          titleState: finalSnapshot.debug?.titleState,
          retailTitleScreen: finalSnapshot.debug?.retailTitleScreen,
          retailTitleNextScreen: finalSnapshot.debug?.retailTitleNextScreen,
          retailTitlePhase: finalSnapshot.debug?.retailTitlePhase,
          retailTitleFadeCounter: finalSnapshot.debug?.retailTitleFadeCounter,
          retailPadHeld: finalSnapshot.debug?.retailPadHeld,
          retailPadTapped: finalSnapshot.debug?.retailPadTapped,
          retailPadHeldPrevious: finalSnapshot.debug?.retailPadHeldPrevious,
          retailPadTappedPrevious: finalSnapshot.debug?.retailPadTappedPrevious,
          retailPadHeldPrevious2: finalSnapshot.debug?.retailPadHeldPrevious2,
          retailFrame: finalSnapshot.debug?.retailFrame,
          retailDrawCount: finalSnapshot.debug?.retailDrawCount,
          retailProcessDrawCount: finalSnapshot.debug?.retailProcessDrawCount,
          retailRandomSeed: finalSnapshot.debug?.retailRandomSeed,
          retailRandomSeedB: finalSnapshot.debug?.retailRandomSeedB,
          retailPathProgress: finalSnapshot.debug?.retailPathProgress,
          retailCameraZone: finalSnapshot.debug?.retailCameraZone,
          retailCameraPath: finalSnapshot.debug?.retailCameraPath,
          retailCameraGameState: finalSnapshot.debug?.retailCameraGameState,
          retailHardRestarts: finalSnapshot.debug?.retailHardRestarts,
          retailLoadStates: finalSnapshot.debug?.retailLoadStates,
          retailDeathCameraFrames:
            finalSnapshot.debug?.retailDeathCameraFrames,
          paused: finalSnapshot.debug?.paused,
          retailCurrentZone: finalSnapshot.debug?.retailCurrentZone,
          retailMainHaltReason: finalSnapshot.debug?.retailMainHaltReason,
          retailMain: finalSnapshot.debug?.retailMain
            ? { ...finalSnapshot.debug.retailMain }
            : null,
          retailTitleObjects: finalSnapshot.debug?.currentLid === DEFAULT_BOOT_LID
            && Array.isArray(finalSnapshot.debug?.browserTestObjects)
            ? finalSnapshot.debug.browserTestObjects.map((object) => ({ ...object }))
            : null,
        });
      }
    }
    finalSnapshot = await browserSnapshot(cdp, sessionId);
    observedRetailExecution = retailExecutionObserved(
      observedRetailExecution,
      finalSnapshot,
    );
    const settleFramesUsed = replayStoppedAtFrameLimit
      ? 0
      : await settleExpectation(
          replay.expect,
          replay.settleFrames,
          PHYSICAL_INPUT_KIND,
          0,
          "browser replay final settle",
        );
    let directBonusReturnSettleFrames = 0;
    if (options.auditDirectBonusReturn) {
      while (
        directBonusReturnSettleFrames < 512
        && directBonusReturnAuditFailures(finalSnapshot).length > 0
      ) {
        directBonusReturnSettleFrames += await stepReplayFrame(
          PHYSICAL_INPUT_KIND,
          0,
          "direct-bonus Title/Main Menu settle",
        );
      }
      const auditProblems = directBonusReturnAuditFailures(finalSnapshot);
      if (auditProblems.length > 0) {
        throw new Error(
          `direct-bonus browser return audit failed:\n${auditProblems.join("\n")}`,
        );
      }
    }
    const finalProblems = [
      ...failures,
      ...snapshotFailures(finalSnapshot),
    ];
    if (finalProblems.length > 0) {
      throw new Error(`browser replay failed:\n${finalProblems.join("\n")}`);
    }
    if (finalSnapshot.harness.stepCount !== stepped) {
      throw new Error(
        `harness issued ${finalSnapshot.harness.stepCount} steps; expected ${stepped}`,
      );
    }
    if (finalSnapshot.harness.hostCallbackCount !== hostCallbacks) {
      throw new Error(
        `harness issued ${finalSnapshot.harness.hostCallbackCount} host callbacks; expected ${hostCallbacks}`,
      );
    }
    if (!(finalSnapshot.debug.frame > 0)) {
      throw new Error("cooperative scheduler did not issue a simulation frame");
    }
    if (!(finalSnapshot.debug.mountedPages > 0)) {
      throw new Error("mounted stream did not expose any NSF pages");
    }
    if (!(finalSnapshot.debug.mountedEntries > 0)) {
      throw new Error("mounted stream did not expose any indexed entries");
    }
    if (!observedRetailExecution) {
      throw new Error("retail GOOL runtime did not execute any objects");
    }
    if (replay.unlockAll) {
      const allLevelProblems = allLevelsFailures(finalSnapshot);
      if (allLevelProblems.length > 0) {
        throw new Error(
          `all-level browser assertion failed:\n${allLevelProblems.join("\n")}`,
        );
      }
    }
    const allLevelsStorage = replay.unlockAll
      ? await evaluate(
          cdp,
          sessionId,
          `({
            [${JSON.stringify(CARD_STORAGE_KEY)}]: localStorage.getItem(${JSON.stringify(CARD_STORAGE_KEY)}),
            [${JSON.stringify(RESUME_STORAGE_KEY)}]: localStorage.getItem(${JSON.stringify(RESUME_STORAGE_KEY)})
          })`,
        )
      : null;
    if (allLevelsStorage !== null) {
      const storageProblems = allLevelsStorageFailures(
        storageSeeds,
        allLevelsStorage,
      );
      if (storageProblems.length > 0) {
        throw new Error(
          `all-level browser storage assertion failed:\n${storageProblems.join("\n")}`,
        );
      }
    }
    const retailPbakLogEvidence = await readRetailPbakLogEvidence();
    const retailPbakEvidence = parseRetailPbakEvidence(
      retailPbakLogEvidence,
      { allowTrailingRepeat: expectedRetailPbakEids !== null },
    );
    const retailPbakAuditReturn = expectedRetailPbakEids !== null
      && retailPbakAuditTitleReady(finalSnapshot)
      ? {
          stepCount: finalSnapshot.harness.stepCount,
          hostCallbackCount: finalSnapshot.harness.hostCallbackCount,
          runtimeState: finalSnapshot.runtimeState,
          runtimeStatus: finalSnapshot.runtimeStatus,
          currentLid: finalSnapshot.debug.currentLid,
          mountedLid: finalSnapshot.debug.mountedLid,
          mountedPages: finalSnapshot.debug.mountedPages,
          mountedEntries: finalSnapshot.debug.mountedEntries,
          lastRequestedLid: finalSnapshot.harness.lastRequestedLid,
        }
      : null;
    if (expectedRetailPbakEids !== null) {
      const auditProblems = retailPbakAuditFailures(retailPbakEvidence, {
        expectedEids: expectedRetailPbakEids,
      });
      if (!retailPbakAuditComplete || auditProblems.length > 0) {
        throw new Error(
          `retail PBAK audit did not complete cleanly:\n${auditProblems.join("\n")}`,
        );
      }
    }
    const screenshot = await cdp.command(
      "Page.captureScreenshot",
      { format: "png", captureBeyondViewport: true },
      sessionId,
      30_000,
    );
    const screenshotBytes = Buffer.from(screenshot.data, "base64");
    await mkdir(dirname(options.screenshot), { recursive: true });
    await writeFile(options.screenshot, screenshotBytes);
    const finalExpectationFailures = replayStoppedAtFrameLimit
      ? []
      : expectationFailures(replay.expect, finalSnapshot);
    if (finalExpectationFailures.length > 0) {
      throw new Error(
        [
          "final replay expectation failed:",
          ...finalExpectationFailures,
          `failure screenshot: ${options.screenshot}`,
          "final snapshot:",
          JSON.stringify(finalSnapshot, null, 2),
        ].join("\n"),
      );
    }
    const video = videoRecorder instanceof SourceFrameVideoRecorder
      ? await videoRecorder.finish()
      : null;
    const videoWindows = videoRecorder instanceof SourceFrameWindowRecorder
      ? await videoRecorder.finish(stepped)
      : null;
    return {
      assets: options.assets.length,
      pairs: imported.pairCount,
      frames: stepped,
      hostCallbacks,
      zeroStepHostCallbacks,
      maximumConsecutiveZeroStepCallbacks,
      replayFrames: replay.totalFrames,
      replayComplete: !replayStoppedAtFrameLimit,
      replayFrameLimit: replayFrameLimit ?? null,
      skippedReplayFrames,
      settleFramesUsed,
      segmentSettleFramesUsed,
      lastInputKind: finalSnapshot.harness.lastInputKind,
      lastHeld: finalSnapshot.harness.lastHeld,
      lastRequestedLid: finalSnapshot.harness.lastRequestedLid,
      runtimeState: finalSnapshot.runtimeState,
      runtimeStatus: finalSnapshot.runtimeStatus,
      currentLid: finalSnapshot.debug.currentLid,
      mountedLid: finalSnapshot.debug.mountedLid,
      titleState: finalSnapshot.debug.titleState,
      mountedPages: finalSnapshot.debug.mountedPages,
      mountedEntries: finalSnapshot.debug.mountedEntries,
      browserEventFailures: failures.length,
      windowConsoleErrors: finalSnapshot.consoleErrors.length,
      runtimeFaultLines: (finalSnapshot.runtimeLog ?? "")
        .split("\n")
        .filter((line) => line.startsWith("! ")).length,
      glError: finalSnapshot.debug.glError,
      retailRuntimeError: finalSnapshot.debug.retailRuntimeError,
      retailRuntimeWarning: finalSnapshot.debug.retailRuntimeWarning,
      retailExecutionErrors: finalSnapshot.debug.retailExecutionErrors,
      retailFaultedObjects: finalSnapshot.debug.retailFaultedObjects,
      retailZoneEventFailures: finalSnapshot.debug.retailZoneEventFailures,
      retailExecutions: finalSnapshot.debug.retailExecutions,
      retailExecutionObserved: observedRetailExecution,
      retailFrame: finalSnapshot.debug.retailFrame,
      retailDrawCount: finalSnapshot.debug.retailDrawCount,
      retailProcessDrawCount: finalSnapshot.debug.retailProcessDrawCount,
      retailRandomSeed: finalSnapshot.debug.retailRandomSeed,
      retailRandomSeedB: finalSnapshot.debug.retailRandomSeedB,
      retailHardRestarts: finalSnapshot.debug.retailHardRestarts,
      retailLoadStates: finalSnapshot.debug.retailLoadStates,
      retailDeathCameraFrames: finalSnapshot.debug.retailDeathCameraFrames,
      paused: finalSnapshot.debug.paused,
      retailCurrentZone: finalSnapshot.debug.retailCurrentZone,
      retailLiveObjects: finalSnapshot.debug.retailLiveObjects,
      retailAuthoredSpawnRejections:
        finalSnapshot.debug.retailAuthoredSpawnRejections,
      retailFailedSpawns: finalSnapshot.debug.retailFailedSpawns,
      retailMain: finalSnapshot.debug.retailMain
        ? { ...finalSnapshot.debug.retailMain }
        : null,
      unlockAll: replay.unlockAll,
      allLevelsLaunchEvidence,
      playerLifeCount: finalSnapshot.debug.playerLifeCount,
      allLevelsStorage: allLevelsStorage === null
        ? null
        : {
            cardExpected: Object.hasOwn(storageSeeds, CARD_STORAGE_KEY),
            cardPresent: allLevelsStorage[CARD_STORAGE_KEY] !== null,
            cardExact:
              allLevelsStorage[CARD_STORAGE_KEY]
                === (Object.hasOwn(storageSeeds, CARD_STORAGE_KEY)
                  ? storageSeeds[CARD_STORAGE_KEY]
                  : null),
            resumeExpected: Object.hasOwn(storageSeeds, RESUME_STORAGE_KEY),
            resumePresent: allLevelsStorage[RESUME_STORAGE_KEY] !== null,
            resumeExact:
              allLevelsStorage[RESUME_STORAGE_KEY]
                === (Object.hasOwn(storageSeeds, RESUME_STORAGE_KEY)
                  ? storageSeeds[RESUME_STORAGE_KEY]
                  : null),
          },
      browserTestGlobals: finalSnapshot.debug.browserTestGlobals,
      retailPbakEvidence,
      retailPbakAuditComplete,
      retailPbakAuditExpectedEids: expectedRetailPbakEids,
      retailPbakAuditReturn,
      directBonusReturnAudit: options.auditDirectBonusReturn,
      directBonusReadyFrames,
      directBonusCeremonyFrames,
      directBonusReturnSettleFrames,
      postSelectionNetworkRequests:
        networkRequests.length - importNetworkRequestStart,
      segmentTrace,
      screenshot: options.screenshot,
      screenshotSha256: createHash("sha256")
        .update(screenshotBytes)
        .digest("hex"),
      video,
      videoWindows,
    };
  } finally {
    await videoRecorder?.abort();
    await cdp?.close().catch(() => {});
    await terminate(chrome.child);
    await removeTemporaryTree(chrome.profile);
  }
}

export async function run(options) {
  let syntheticDirectory;
  try {
    let runOptions = options;
    if (options.syntheticCookedIsoImport) {
      if (
        options.assets.length > 0
        || options.replay !== undefined
        || options.cardStorageSeed !== undefined
        || options.resumeStorageSeed !== undefined
      ) {
        throw new Error(
          "synthetic cooked-ISO import verification does not accept external assets, replay data, or storage seeds",
        );
      }
      syntheticDirectory = await mkdtemp(
        resolve(tmpdir(), "crust-synthetic-cooked-iso-"),
      );
      const syntheticPath = resolve(
        syntheticDirectory,
        "synthetic-retail-catalog.iso",
      );
      await writeFile(syntheticPath, createSyntheticRetailCookedIso(), {
        flag: "wx",
      });
      runOptions = { ...options, assets: [syntheticPath] };
    }

    await validateAssets(runOptions.assets);
    const loadedReplay = runOptions.syntheticCookedIsoImport
      ? undefined
      : await loadReplay(runOptions.replay, runOptions);
    const replay = loadedReplay === undefined
      ? undefined
      : applyTerminalProgressionRequirements(loadedReplay, runOptions);
    const storageSeeds = await loadStorageSeeds(runOptions);
    const chromeExecutable =
      runOptions.chrome ?? (await firstExisting(CHROME_CANDIDATES));
    if (!chromeExecutable) {
      throw new Error(
        "Chrome/Chromium was not found; pass --chrome or set CRUST_CHROME_BIN",
      );
    }
    await access(chromeExecutable).catch((error) => {
      throw new Error(`cannot execute Chrome at ${chromeExecutable}: ${error.message}`);
    });
    let server;
    try {
      if (runOptions.startServer) {
        server = await startHarnessServer(runOptions.url);
      }
      return await runBrowser(runOptions, replay, chromeExecutable, storageSeeds);
    } finally {
      await terminate(server);
    }
  } finally {
    if (syntheticDirectory) {
      await rm(syntheticDirectory, { recursive: true, force: true });
    }
  }
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.help) {
    process.stdout.write(usage());
    return;
  }
  const result = await run(options);
  process.stdout.write(
    `browser harness smoke passed: ${JSON.stringify(result)}\n`,
  );
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    process.stderr.write(`browser harness smoke failed: ${error.stack ?? error}\n`);
    process.exitCode = 1;
  });
}
