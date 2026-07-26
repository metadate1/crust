import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, dirname, extname, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

import { ChromeCdp } from "./chrome-cdp.mjs";

const repositoryRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const DEFAULT_URL = "http://127.0.0.1:4175/";
const DEFAULT_BOOT_LID = 0x19;
const DEFAULT_FRAMES = 120;
const REPLAY_BATCH_FRAME_LIMIT = 128;
const ALL_LEVELS_MAX_LIVES = 999 << 8;
const ALL_LEVELS_UNLOCK_GATE = 99;
const ALL_LEVELS_SECRET_PATH_BITS = (1 << 10) | (1 << 20);
const STORAGE_KEYS = [
  "c1.virtual-memory-card.v1",
  "c1.browser-resume.v1",
];
const SUPPORTED_ASSET_EXTENSIONS = new Set([".bin", ".iso", ".nsd", ".nsf"]);
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

export function usage() {
  return `Usage:
  node scripts/browser-harness-smoke.mjs --asset PATH [--asset PATH ...] [options]

Options:
  --asset PATH       Legally owned BIN/ISO/NSD/NSF file (repeatable)
  --replay PATH      Run-length replay JSON; overrides --lid and --frames
  --lid NUMBER       Direct-boot stream id (default: 0x19)
  --frames NUMBER    Number of zero-input frames (default: 120)
  --unlock-all       Enable the temporary all-level/max-lives option
  --url URL          Local harness URL (default: ${DEFAULT_URL})
  --no-server        Use an already-running harness server
  --chrome PATH      Chrome/Chromium executable
  --screenshot PATH  PNG output (default: target/browser-test-artifacts/smoke.png)
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
    unlockAll: false,
    url: environment.CRUST_BROWSER_HARNESS_URL ?? DEFAULT_URL,
    startServer: true,
    chrome: environment.CRUST_CHROME_BIN,
    replay: undefined,
    screenshot: resolve(
      repositoryRoot,
      "target/browser-test-artifacts/smoke.png",
    ),
    help: false,
  };
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
      case "--replay":
        options.replay = value();
        break;
      case "--lid":
        options.bootLid = parseWholeNumber(value(), "--lid", 0xff);
        break;
      case "--frames":
        options.frames = parseWholeNumber(value(), "--frames", 1_000_000);
        if (options.frames === 0) throw new Error("--frames must be at least 1");
        break;
      case "--unlock-all":
        options.unlockAll = true;
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
      case "--help":
      case "-h":
        options.help = true;
        break;
      default:
        throw new Error(`unknown argument: ${argument}`);
    }
  }
  options.assets = options.assets.map((path) => resolve(path));
  if (options.replay) options.replay = resolve(options.replay);
  const url = new URL(options.url);
  if (
    url.protocol !== "http:" ||
    !["127.0.0.1", "localhost", "::1"].includes(url.hostname)
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
        "minFrame",
        "minRetailFrame",
        "minRetailExecutions",
      ].includes(name)
    ) {
      throw new Error(`${label}.${name} is not a supported expectation`);
    }
    expectation[name] = parseWholeNumber(
      value,
      `${label}.${name}`,
      Number.MAX_SAFE_INTEGER,
    );
  }
  return expectation;
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
    normalized.segments.push({
      frames,
      held: parseWholeNumber(
        segment.held,
        `replay.segments[${index}].held`,
        0xffff,
      ),
      expect: normalizeExpectation(
        segment.expect,
        `replay.segments[${index}].expect`,
      ),
      settleFrames,
      settleHeld: parseWholeNumber(
        segment.settleHeld ?? 0,
        `replay.segments[${index}].settleHeld`,
        0xffff,
      ),
    });
  }
  normalized.totalFrames = normalized.segments.reduce(
    (sum, segment) => sum + segment.frames,
    0,
  );
  normalized.maximumFrames = totalFrames + normalized.settleFrames;
  return normalized;
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
  for (const name of ["mountedLid", "currentLid", "titleState"]) {
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
  if (expectation.liveObject !== undefined) {
    failures.push(
      ...liveObjectExpectationFailures(expectation.liveObject, snapshot),
    );
  }
  return failures;
}

export function allLevelsFailures(
  snapshot,
  { requireStartingLives = false } = {},
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

async function launchChrome(executable) {
  const profile = await mkdtemp(resolve(tmpdir(), "crust-browser-smoke-"));
  const output = [];
  const child = spawn(
    executable,
    [
      "--headless=new",
      "--remote-debugging-port=0",
      `--user-data-dir=${profile}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-background-networking",
      "--disable-component-update",
      "--disable-sync",
      "--metrics-recording-only",
      "--autoplay-policy=no-user-gesture-required",
      "--window-size=1440,1100",
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
      await rm(profile, { recursive: true, force: true });
      throw new Error(`could not launch Chrome: ${spawnError.message}`);
    }
    if (child.exitCode !== null) {
      await rm(profile, { recursive: true, force: true });
      throw new Error(`Chrome exited before DevTools was ready:\n${output.join("")}`);
    }
    await delay(25);
  }
  if (!webSocketUrl) {
    await terminate(child);
    await rm(profile, { recursive: true, force: true });
    throw new Error(`Chrome did not publish a DevTools endpoint:\n${output.join("")}`);
  }
  return { child, profile, webSocketUrl };
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

const snapshotExpression = `(() => {
  const debug = window.__crustDebug || {};
  const harness = window.__crustTest || {};
  const browserTestObjects =
    typeof debug.snapshotRetailObjects === "function"
      ? debug.snapshotRetailObjects()
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
    runtimeLog: document.querySelector("#runtimeLog")?.textContent ?? "",
    consoleErrors: [...(window.__consoleErrors || [])],
    harness: pick(harness, [
      "mode", "frameDurationMs", "stepCount", "lastError", "lastHeld",
      "lastTimestampMs", "lastRequestedLid"
    ]),
    debug: {
      ...pick(debug, [
        "frame", "currentLid", "titleState", "pairs", "mountedLid",
        "mountedPages", "mountedEntries", "glError", "paused", "retailFrame",
        "retailDrawCount", "retailProcessDrawCount", "retailRandomSeed",
        "retailRandomSeedB", "retailCurrentZone", "retailLiveObjects",
        "retailFaultedObjects", "retailExecutions", "retailExecutionErrors",
        "retailZoneEventFailures", "retailRuntimeError", "retailRuntimeWarning"
      ]),
      retailMain: debug.retailMain ? { ...debug.retailMain } : null,
      browserTestGlobals: debug.browserTestGlobals
        ? { ...debug.browserTestGlobals }
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
    throw new Error(`${label} expectation failed:\n${failures.join("\n")}`);
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

async function runBrowser(options, replay, chromeExecutable) {
  const chrome = await launchChrome(chromeExecutable);
  let cdp;
  try {
    cdp = await ChromeCdp.connect(chrome.webSocketUrl);
    const sessionId = await attachPage(cdp);
    const failures = [];
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
        width: 1440,
        height: 1100,
        deviceScaleFactor: 1,
        mobile: false,
      },
      sessionId,
    );
    await cdp.command(
      "Page.addScriptToEvaluateOnNewDocument",
      {
        source: `try {
          localStorage.clear();
          sessionStorage.clear();
        } catch (error) {
          throw new Error("browser smoke could not clear storage: " + error);
        }
        window.__crustBrowserSmokeFresh = true;`,
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
    const freshKeys = await evaluate(
      cdp,
      sessionId,
      `(${JSON.stringify(STORAGE_KEYS)}).filter((key) => localStorage.getItem(key) !== null)`,
    );
    if (freshKeys.length > 0) {
      throw new Error(`fresh browser profile retained storage keys: ${freshKeys.join(", ")}`);
    }

    const { root } = await cdp.command("DOM.getDocument", { depth: -1 }, sessionId);
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
    const imported = await waitFor(
      cdp,
      sessionId,
      "local game-file import",
      (snapshot) =>
        snapshot.pairCount > 0 &&
        !snapshot.launchDisabled &&
        snapshot.runtimeState === "idle",
      failures,
      120_000,
    );

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

    let stepped = 0;
    // Manual harness mode cannot publish runtime globals until its first
    // cooperative step. Check that first result before the replay can
    // continue far enough to spend a life legitimately.
    let allLevelsLaunchChecked = !replay.unlockAll;
    let finalSnapshot;
    const stepReplayBatch = async (held, frameCount, label) => {
      const batchStart = stepped + 1;
      const result = await evaluate(
        cdp,
        sessionId,
        `(() => {
          let executed = 0;
          while (executed < ${frameCount}) {
            window.__crustTest.step(${held});
            executed += 1;
            if (
              window.__crustTest.lastError != null
              || window.__crustTest.lastRequestedLid != null
            ) {
              break;
            }
          }
          return {
            executed,
            snapshot: ${snapshotExpression}
          };
        })()`,
      );
      const executed = result?.executed;
      if (
        !Number.isSafeInteger(executed)
        || executed < 1
        || executed > frameCount
      ) {
        throw new Error(
          `${label} returned an invalid batch count: ${JSON.stringify(executed)}`,
        );
      }
      finalSnapshot = result.snapshot;
      stepped += executed;
      if (!allLevelsLaunchChecked) {
        if (executed !== 1) {
          throw new Error(
            `all-level browser launch check requires one isolated first frame; received ${executed}`,
          );
        }
        const startupProblems = allLevelsFailures(finalSnapshot, {
          requireStartingLives: true,
        });
        if (startupProblems.length > 0) {
          throw new Error(
            `all-level browser launch assertion failed:\n${startupProblems.join("\n")}`,
          );
        }
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
      if (finalSnapshot.harness.lastRequestedLid != null) {
        finalSnapshot = await waitFor(
          cdp,
          sessionId,
          `destination 0x${Number(finalSnapshot.harness.lastRequestedLid).toString(16)} mount`,
          (snapshot) => snapshot.runtimeState === "running",
          failures,
          120_000,
        );
      }
      return executed;
    };
    const stepReplayFrame = async (held, label) => {
      const executed = await stepReplayBatch(held, 1, label);
      if (executed !== 1) {
        throw new Error(`${label} did not execute exactly one frame`);
      }
    };
    const settleExpectation = async (
      expectation,
      maximumFrames,
      held,
      label,
    ) => {
      let used = 0;
      while (
        used < maximumFrames
        && expectationFailures(expectation, finalSnapshot).length > 0
      ) {
        await stepReplayFrame(held, label);
        used += 1;
      }
      return used;
    };
    let segmentSettleFramesUsed = 0;
    for (const [segmentIndex, segment] of replay.segments.entries()) {
      let remainingFrames = segment.frames;
      while (remainingFrames > 0) {
        const batchFrames = nextReplayBatchFrameCount(remainingFrames, {
          isolateFirstFrame: !allLevelsLaunchChecked,
        });
        const executed = await stepReplayBatch(
          segment.held,
          batchFrames,
          "browser replay",
        );
        remainingFrames -= executed;
      }
      segmentSettleFramesUsed += await settleExpectation(
        segment.expect,
        segment.settleFrames,
        segment.settleHeld,
        `browser replay segment ${segmentIndex + 1} settle`,
      );
      assertExpected(
        segment.expect,
        finalSnapshot,
        `segment ${segmentIndex + 1}`,
      );
    }
    finalSnapshot = await browserSnapshot(cdp, sessionId);
    const settleFramesUsed = await settleExpectation(
      replay.expect,
      replay.settleFrames,
      0,
      "browser replay final settle",
    );
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
    if (!(finalSnapshot.debug.frame > 0)) {
      throw new Error("cooperative scheduler did not issue a simulation frame");
    }
    if (!(finalSnapshot.debug.mountedPages > 0)) {
      throw new Error("mounted stream did not expose any NSF pages");
    }
    if (!(finalSnapshot.debug.mountedEntries > 0)) {
      throw new Error("mounted stream did not expose any indexed entries");
    }
    if (!(finalSnapshot.debug.retailExecutions > 0)) {
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
    const screenshot = await cdp.command(
      "Page.captureScreenshot",
      { format: "png", captureBeyondViewport: true },
      sessionId,
      30_000,
    );
    const screenshotBytes = Buffer.from(screenshot.data, "base64");
    await mkdir(dirname(options.screenshot), { recursive: true });
    await writeFile(options.screenshot, screenshotBytes);
    const finalExpectationFailures = expectationFailures(
      replay.expect,
      finalSnapshot,
    );
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
    return {
      assets: options.assets.length,
      pairs: imported.pairCount,
      frames: stepped,
      replayFrames: replay.totalFrames,
      settleFramesUsed,
      segmentSettleFramesUsed,
      currentLid: finalSnapshot.debug.currentLid,
      mountedLid: finalSnapshot.debug.mountedLid,
      retailExecutions: finalSnapshot.debug.retailExecutions,
      retailFrame: finalSnapshot.debug.retailFrame,
      retailDrawCount: finalSnapshot.debug.retailDrawCount,
      retailProcessDrawCount: finalSnapshot.debug.retailProcessDrawCount,
      retailRandomSeed: finalSnapshot.debug.retailRandomSeed,
      retailRandomSeedB: finalSnapshot.debug.retailRandomSeedB,
      retailCurrentZone: finalSnapshot.debug.retailCurrentZone,
      retailLiveObjects: finalSnapshot.debug.retailLiveObjects,
      retailMain: finalSnapshot.debug.retailMain
        ? { ...finalSnapshot.debug.retailMain }
        : null,
      unlockAll: replay.unlockAll,
      browserTestGlobals: finalSnapshot.debug.browserTestGlobals,
      screenshot: options.screenshot,
      screenshotSha256: createHash("sha256")
        .update(screenshotBytes)
        .digest("hex"),
    };
  } finally {
    await cdp?.close().catch(() => {});
    await terminate(chrome.child);
    await rm(chrome.profile, { recursive: true, force: true });
  }
}

export async function run(options) {
  await validateAssets(options.assets);
  const replay = await loadReplay(options.replay, options);
  const chromeExecutable =
    options.chrome ?? (await firstExisting(CHROME_CANDIDATES));
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
    if (options.startServer) server = await startHarnessServer(options.url);
    return await runBrowser(options, replay, chromeExecutable);
  } finally {
    await terminate(server);
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
