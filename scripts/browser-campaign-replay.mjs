import { randomUUID } from "node:crypto";
import {
  access,
  mkdir,
  readFile,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { normalizeReplay } from "./browser-harness-smoke.mjs";

const repositoryRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const TITLE_LID = 0x19;
const PAD_UP = 0x1000;
const PAD_DOWN = 0x4000;
const PAD_LEFT = 0x8000;
const PAD_RIGHT = 0x2000;
const LOCAL_OUTPUT_ROOTS = new Set([
  "artifacts",
  "captures",
  "local-data",
  "recordings",
  "target",
]);
const CHECKPOINT_FIELDS = [
  "currentLid",
  "mountedLid",
  "retailDrawCount",
  "retailProcessDrawCount",
  "retailRandomSeed",
  "retailRandomSeedB",
  "retailHardRestarts",
  "retailLoadStates",
  "retailDeathCameraFrames",
  "titleState",
];
const PROGRESSION_FIELDS = [
  "gameState",
  "titleState",
  "savedTitleState",
  "currentMapLevel",
  "levelCount",
  "levelsUnlocked",
  "islandCameraState",
];
const PAD_SNAPSHOT_FIELDS = [
  "tapped",
  "held",
  "tappedPrevious",
  "heldPrevious",
  "heldPrevious2",
];
const ALL_CHECKPOINT_FIELDS = new Set([
  ...CHECKPOINT_FIELDS,
]);
const ALL_PROGRESSION_FIELDS = new Set(PROGRESSION_FIELDS);
const ALL_PAD_SNAPSHOT_FIELDS = new Set(PAD_SNAPSHOT_FIELDS);
const PHASE_KEYS = new Set([
  "entry",
  "exit",
  "fragment",
  "id",
  "inputKind",
  "settleFrames",
]);
const HANDOFF_KEYS = new Set([
  "after",
  "before",
  "kind",
  "phases",
]);
const MANIFEST_KEYS = new Set([
  "bootLid",
  "canonicalCampaign",
  "localDiagnosticOnly",
  "phases",
  "schema",
  "settleFrames",
  "titleMapHandoffs",
  "traceFromPhase",
  "unlockAll",
]);

function isObject(value) {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

function assertOnlyKeys(value, allowed, label) {
  const unexpected = Object.keys(value).filter((key) => !allowed.has(key));
  if (unexpected.length > 0) {
    throw new Error(`${label} contains unsupported fields: ${unexpected.join(", ")}`);
  }
}

function wholeNumber(raw, label, maximum = Number.MAX_SAFE_INTEGER) {
  const value =
    typeof raw === "number"
      ? raw
      : typeof raw === "string" && /^0x[0-9a-f]+$/i.test(raw)
        ? Number.parseInt(raw.slice(2), 16)
        : Number(raw);
  if (!Number.isSafeInteger(value) || value < 0 || value > maximum) {
    throw new Error(`${label} must be a whole number from 0 through ${maximum}`);
  }
  return value;
}

function nonEmptyString(raw, label) {
  if (typeof raw !== "string" || raw.trim() === "") {
    throw new Error(`${label} must be a non-empty string`);
  }
  return raw;
}

function normalizeCheckpoint(raw, label) {
  if (!isObject(raw)) throw new Error(`${label} must be an object`);
  assertOnlyKeys(raw, ALL_CHECKPOINT_FIELDS, label);
  const missing = CHECKPOINT_FIELDS.filter((field) => raw[field] === undefined);
  if (missing.length > 0) {
    throw new Error(
      `${label} is missing exact continuity fields: ${missing.join(", ")}`,
    );
  }
  const checkpoint = {};
  for (const field of CHECKPOINT_FIELDS) {
    checkpoint[field] = wholeNumber(
      raw[field],
      `${label}.${field}`,
      field.endsWith("Lid") ? 0xff : 0xffff_ffff,
    );
  }
  if (checkpoint.currentLid !== checkpoint.mountedLid) {
    throw new Error(
      `${label}.currentLid and ${label}.mountedLid must identify the same completed mount`,
    );
  }
  return checkpoint;
}

function normalizeProgression(raw, label) {
  if (!isObject(raw)) throw new Error(`${label} must be an object`);
  assertOnlyKeys(raw, ALL_PROGRESSION_FIELDS, label);
  const missing = PROGRESSION_FIELDS.filter((field) => raw[field] === undefined);
  if (missing.length > 0) {
    throw new Error(
      `${label} is missing exact progression fields: ${missing.join(", ")}`,
    );
  }
  return Object.fromEntries(
    PROGRESSION_FIELDS.map((field) => [
      field,
      wholeNumber(raw[field], `${label}.${field}`, 0xffff_ffff),
    ]),
  );
}

function hasOpposingPhysicalDirections(held) {
  return (
    ((held & PAD_UP) !== 0 && (held & PAD_DOWN) !== 0)
    || ((held & PAD_LEFT) !== 0 && (held & PAD_RIGHT) !== 0)
  );
}

function normalizePadSnapshot(raw, label) {
  if (!isObject(raw)) throw new Error(`${label} must be an object`);
  assertOnlyKeys(raw, ALL_PAD_SNAPSHOT_FIELDS, label);
  const missing = PAD_SNAPSHOT_FIELDS.filter((field) => raw[field] === undefined);
  if (missing.length > 0) {
    throw new Error(
      `${label} is missing exact physical-pad fields: ${missing.join(", ")}`,
    );
  }
  const snapshot = Object.fromEntries(
    PAD_SNAPSHOT_FIELDS.map((field) => [
      field,
      wholeNumber(raw[field], `${label}.${field}`, 0xffff_ffff),
    ]),
  );
  if ((snapshot.tapped & ~snapshot.held) !== 0) {
    throw new Error(`${label}.tapped must be a subset of held`);
  }
  if ((snapshot.tappedPrevious & ~snapshot.heldPrevious) !== 0) {
    throw new Error(`${label}.tappedPrevious must be a subset of heldPrevious`);
  }
  return snapshot;
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

function replayPad(initial, segments) {
  let snapshot = initial;
  for (const segment of segments) {
    // A constant physical word reaches a stable five-word history after
    // three updates, so very long local route fragments remain cheap to
    // validate without weakening the exact final-state check.
    const updates = Math.min(segment.frames, 3);
    for (let index = 0; index < updates; index += 1) {
      snapshot = advancePadSnapshot(snapshot, segment.held);
    }
  }
  return snapshot;
}

function normalizeInputKind(raw, label) {
  if (raw === undefined) return undefined;
  if (raw !== "physical" && raw !== "recorded") {
    throw new Error(`${label} must be "physical" or "recorded"`);
  }
  return raw;
}

function normalizePhase(raw, label) {
  if (!isObject(raw)) throw new Error(`${label} must be an object`);
  assertOnlyKeys(raw, PHASE_KEYS, label);
  const phase = {
    id: nonEmptyString(raw.id, `${label}.id`),
    fragment: nonEmptyString(raw.fragment, `${label}.fragment`),
    entry: normalizeCheckpoint(raw.entry, `${label}.entry`),
    exit: normalizeCheckpoint(raw.exit, `${label}.exit`),
    inputKind: normalizeInputKind(raw.inputKind, `${label}.inputKind`),
    settleFrames:
      raw.settleFrames === undefined
        ? undefined
        : wholeNumber(raw.settleFrames, `${label}.settleFrames`, 10_000),
  };
  return phase;
}

function normalizeHandoff(raw, label) {
  if (!isObject(raw)) throw new Error(`${label} must be an object`);
  assertOnlyKeys(raw, HANDOFF_KEYS, label);
  if (raw.kind !== "title-map") {
    throw new Error(`${label}.kind must equal "title-map"`);
  }
  if (!Array.isArray(raw.phases) || raw.phases.length === 0) {
    throw new Error(`${label}.phases must be a non-empty array`);
  }
  const handoff = {
    kind: "title-map",
    after: nonEmptyString(raw.after, `${label}.after`),
    before: nonEmptyString(raw.before, `${label}.before`),
    phases: raw.phases.map((phase, index) =>
      normalizePhase(phase, `${label}.phases[${index}]`),
    ),
  };
  if (!handoff.phases.some((phase) => phase.entry.currentLid === TITLE_LID)) {
    throw new Error(
      `${label} does not contain an authored Title / Island Map (LID 0x19) phase`,
    );
  }
  return handoff;
}

function checkpointDifference(left, right) {
  const fields = new Set([...Object.keys(left), ...Object.keys(right)]);
  return [...fields]
    .filter((field) => left[field] !== right[field])
    .map(
      (field) =>
        `${field} ${JSON.stringify(left[field])} != ${JSON.stringify(right[field])}`,
    );
}

function assertCheckpointContinuity(leftPhase, rightPhase, label) {
  const differences = checkpointDifference(leftPhase.exit, rightPhase.entry);
  if (differences.length > 0) {
    throw new Error(
      `${label} is discontinuous between ${JSON.stringify(leftPhase.id)} and ` +
        `${JSON.stringify(rightPhase.id)}: ${differences.join(", ")}`,
    );
  }
}

export function normalizeCampaignManifest(raw) {
  if (!isObject(raw)) throw new Error("campaign manifest must be an object");
  assertOnlyKeys(raw, MANIFEST_KEYS, "campaign manifest");
  if (raw.schema !== 1) throw new Error("campaign manifest.schema must equal 1");
  if (raw.localDiagnosticOnly !== true) {
    throw new Error("campaign manifest.localDiagnosticOnly must equal true");
  }
  if (raw.canonicalCampaign !== false) {
    throw new Error("campaign manifest.canonicalCampaign must equal false");
  }
  if (!Array.isArray(raw.phases) || raw.phases.length === 0) {
    throw new Error("campaign manifest.phases must be a non-empty array");
  }
  const phases = raw.phases.map((phase, index) =>
    normalizePhase(phase, `campaign manifest.phases[${index}]`),
  );
  if (
    raw.titleMapHandoffs !== undefined
    && !Array.isArray(raw.titleMapHandoffs)
  ) {
    throw new Error("campaign manifest.titleMapHandoffs must be an array");
  }
  const titleMapHandoffs = (raw.titleMapHandoffs ?? []).map((handoff, index) =>
    normalizeHandoff(
      handoff,
      `campaign manifest.titleMapHandoffs[${index}]`,
    ),
  );
  const bootLid = wholeNumber(
    raw.bootLid,
    "campaign manifest.bootLid",
    0xff,
  );
  if (bootLid !== phases[0].entry.currentLid) {
    throw new Error(
      "campaign manifest.bootLid must equal the first phase entry LID",
    );
  }
  const unlockAll = raw.unlockAll ?? false;
  if (typeof unlockAll !== "boolean") {
    throw new Error("campaign manifest.unlockAll must be a boolean");
  }
  const traceFromPhase =
    raw.traceFromPhase === undefined
      ? undefined
      : nonEmptyString(raw.traceFromPhase, "campaign manifest.traceFromPhase");
  const settleFrames = wholeNumber(
    raw.settleFrames ?? 0,
    "campaign manifest.settleFrames",
    10_000,
  );

  const basePhaseIndexes = new Map();
  for (const [index, phase] of phases.entries()) {
    if (basePhaseIndexes.has(phase.id)) {
      throw new Error(`duplicate campaign phase id ${JSON.stringify(phase.id)}`);
    }
    basePhaseIndexes.set(phase.id, index);
  }
  const handoffByAfter = new Map();
  for (const handoff of titleMapHandoffs) {
    const afterIndex = basePhaseIndexes.get(handoff.after);
    const beforeIndex = basePhaseIndexes.get(handoff.before);
    if (afterIndex === undefined || beforeIndex === undefined) {
      throw new Error(
        `title-map handoff ${JSON.stringify(handoff.after)} -> ` +
          `${JSON.stringify(handoff.before)} references an unknown base phase`,
      );
    }
    if (beforeIndex !== afterIndex + 1) {
      throw new Error(
        `title-map handoff ${JSON.stringify(handoff.after)} -> ` +
          `${JSON.stringify(handoff.before)} must connect adjacent ordered phases`,
      );
    }
    if (handoffByAfter.has(handoff.after)) {
      throw new Error(
        `more than one title-map handoff follows ${JSON.stringify(handoff.after)}`,
      );
    }
    handoffByAfter.set(handoff.after, handoff);
  }

  const orderedPhases = [];
  const insertedHandoffs = [];
  for (const [index, phase] of phases.entries()) {
    orderedPhases.push(phase);
    const handoff = handoffByAfter.get(phase.id);
    if (handoff) {
      orderedPhases.push(...handoff.phases);
      insertedHandoffs.push({
        kind: handoff.kind,
        after: handoff.after,
        before: handoff.before,
        phaseIds: handoff.phases.map(({ id }) => id),
      });
    }
    if (index === phases.length - 1 && handoff) {
      throw new Error("a title-map handoff cannot follow the final campaign phase");
    }
  }

  const phaseIds = new Set();
  for (const phase of orderedPhases) {
    if (phaseIds.has(phase.id)) {
      throw new Error(`duplicate composed phase id ${JSON.stringify(phase.id)}`);
    }
    phaseIds.add(phase.id);
  }
  if (traceFromPhase !== undefined && !phaseIds.has(traceFromPhase)) {
    throw new Error(
      `campaign manifest.traceFromPhase references unknown phase ` +
        JSON.stringify(traceFromPhase),
    );
  }
  for (let index = 1; index < orderedPhases.length; index += 1) {
    assertCheckpointContinuity(
      orderedPhases[index - 1],
      orderedPhases[index],
      "campaign phase chain",
    );
  }

  return {
    schema: 1,
    bootLid,
    unlockAll,
    traceFromPhase,
    settleFrames,
    phases: orderedPhases,
    insertedHandoffs,
  };
}

function exactMetadataMatch(left, right) {
  return checkpointDifference(left, right).length === 0;
}

function discoveredPathOrder(left, right) {
  if (left.length !== right.length) return left.length - right.length;
  const leftFrames = left.reduce((sum, node) => sum + node.frames, 0);
  const rightFrames = right.reduce((sum, node) => sum + node.frames, 0);
  if (leftFrames !== rightFrames) return leftFrames - rightFrames;
  return right
    .map((node) => node.fragment)
    .join("\0")
    .localeCompare(left.map((node) => node.fragment).join("\0"));
}

// Discovers the longest exactly composable path among legally local capture
// fragments. This deliberately does not bridge a discontinuity: checkpoint,
// progression, and physical-pad history must all match the same rules used by
// `composeCampaignReplay`.
export function discoverLongestCampaignManifest(
  fragmentEntries,
  { traceInputProfile } = {},
) {
  if (!Array.isArray(fragmentEntries) || fragmentEntries.length === 0) {
    throw new Error("fragmentEntries must be a non-empty array");
  }
  const fragmentNames = new Set();
  const nodes = fragmentEntries.map((entry, index) => {
    if (!isObject(entry)) {
      throw new Error(`fragmentEntries[${index}] must be an object`);
    }
    const fragment = nonEmptyString(
      entry.fragment,
      `fragmentEntries[${index}].fragment`,
    );
    if (fragmentNames.has(fragment)) {
      throw new Error(`duplicate campaign fragment ${JSON.stringify(fragment)}`);
    }
    fragmentNames.add(fragment);
    const document = entry.document;
    if (!isObject(document)) {
      throw new Error(`fragmentEntries[${index}].document must be an object`);
    }
    if (
      document.localDiagnosticOnly !== true
      || document.canonicalCampaign !== false
    ) {
      throw new Error(
        `fragmentEntries[${index}].document must opt in as a local, noncanonical capture`,
      );
    }
    const entryCheckpoint = normalizeCheckpoint(
      document.entryCheckpoint,
      `fragmentEntries[${index}].document.entryCheckpoint`,
    );
    const exitCheckpoint = normalizeCheckpoint(
      document.exitCheckpoint,
      `fragmentEntries[${index}].document.exitCheckpoint`,
    );
    return {
      fragment,
      document,
      entry: entryCheckpoint,
      exit: exitCheckpoint,
      entryProgression: normalizeProgression(
        document.entryProgression,
        `fragmentEntries[${index}].document.entryProgression`,
      ),
      exitProgression: normalizeProgression(
        document.exitProgression,
        `fragmentEntries[${index}].document.exitProgression`,
      ),
      initialPad: normalizePadSnapshot(
        document.initialPad,
        `fragmentEntries[${index}].document.initialPad`,
      ),
      finalPad: normalizePadSnapshot(
        document.finalPad,
        `fragmentEntries[${index}].document.finalPad`,
      ),
      frames: wholeNumber(
        document.frames,
        `fragmentEntries[${index}].document.frames`,
        10_000_000,
      ),
      inputProfile:
        document.inputProfile === undefined
          ? "captured"
          : nonEmptyString(
              document.inputProfile,
              `fragmentEntries[${index}].document.inputProfile`,
            ),
    };
  });
  const bootPad = {
    tapped: 0,
    held: 0,
    tappedPrevious: 0,
    heldPrevious: 0,
    heldPrevious2: 0,
  };
  const follows = (left, right) => {
    if (!exactMetadataMatch(left.exit, right.entry)) return false;
    if (!exactMetadataMatch(left.exitProgression, right.entryProgression)) {
      return false;
    }
    const expectedPad =
      left.entry.currentLid === left.exit.currentLid
        ? left.finalPad
        : advancePadSnapshot(left.finalPad, left.finalPad.held);
    return exactMetadataMatch(expectedPad, right.initialPad);
  };
  const longestFrom = (node, seen) => {
    let best = [node];
    for (const candidate of nodes) {
      if (seen.has(candidate.fragment) || !follows(node, candidate)) continue;
      const path = [
        node,
        ...longestFrom(
          candidate,
          new Set([...seen, candidate.fragment]),
        ),
      ];
      if (discoveredPathOrder(path, best) > 0) best = path;
    }
    return best;
  };
  const roots = nodes.filter((node) =>
    exactMetadataMatch(node.initialPad, bootPad),
  );
  if (roots.length === 0) {
    throw new Error("no fragment begins with the browser's physical boot pad");
  }
  let longest = [];
  for (const root of roots) {
    const path = longestFrom(root, new Set([root.fragment]));
    if (discoveredPathOrder(path, longest) > 0) longest = path;
  }
  const phases = longest.map((node, index) => ({
    id:
      `phase-${String(index + 1).padStart(2, "0")}-` +
      node.inputProfile.replaceAll(/[^a-z0-9]+/gi, "-").replaceAll(/^-|-$/g, ""),
    fragment: node.fragment,
    entry: node.entry,
    exit: node.exit,
  }));
  const tracedIndex =
    traceInputProfile === undefined
      ? -1
      : longest.findIndex((node) => node.inputProfile === traceInputProfile);
  if (traceInputProfile !== undefined && tracedIndex === -1) {
    throw new Error(
      `longest exact path does not contain input profile ${JSON.stringify(traceInputProfile)}`,
    );
  }
  return {
    schema: 1,
    localDiagnosticOnly: true,
    canonicalCampaign: false,
    bootLid: longest[0].entry.currentLid,
    unlockAll: false,
    ...(tracedIndex === -1 ? {} : { traceFromPhase: phases[tracedIndex].id }),
    phases,
    titleMapHandoffs: [],
  };
}

function expectationConflict(left, right) {
  return Object.keys(left)
    .filter((field) => right[field] !== undefined && left[field] !== right[field])
    .map(
      (field) =>
        `${field} ${JSON.stringify(left[field])} != ${JSON.stringify(right[field])}`,
    );
}

function mergeExpectations(left, right, label) {
  const conflicts = expectationConflict(left, right);
  if (conflicts.length > 0) {
    throw new Error(`${label} conflicts with exact phase exit: ${conflicts.join(", ")}`);
  }
  return { ...left, ...right };
}

function validatePhysicalInputWords(normalizedReplay, phase) {
  const label = `fragment for phase ${JSON.stringify(phase.id)}`;
  for (const [index, segment] of normalizedReplay.segments.entries()) {
    if (segment.inputKind !== "physical") continue;
    for (const [field, held] of [
      ["held", segment.held],
      ["settleHeld", segment.settleHeld],
    ]) {
      if (hasOpposingPhysicalDirections(held)) {
        throw new Error(
          `${label} segment ${index + 1}.${field} contains opposing physical ` +
            `directions; use inputKind "recorded" for an exact PBAK word or ` +
            "export a physically valid route",
        );
      }
    }
  }
}

function validateFragmentMetadata(fragment, normalizedReplay, phase, manifest) {
  const label = `fragment for phase ${JSON.stringify(phase.id)}`;
  if (!isObject(fragment)) throw new Error(`${label} must be an object`);
  if (fragment.localDiagnosticOnly !== true) {
    throw new Error(`${label}.localDiagnosticOnly must equal true`);
  }
  if (fragment.canonicalCampaign !== false) {
    throw new Error(`${label}.canonicalCampaign must equal false`);
  }
  const level = wholeNumber(fragment.level, `${label}.level`, 0xff);
  if (level !== phase.entry.currentLid || normalizedReplay.bootLid !== level) {
    throw new Error(
      `${label} level/bootLid must equal the phase entry LID ` +
        `0x${phase.entry.currentLid.toString(16).padStart(2, "0")}`,
    );
  }
  if (normalizedReplay.unlockAll !== manifest.unlockAll) {
    throw new Error(`${label}.unlockAll does not match the campaign manifest`);
  }
  validatePhysicalInputWords(normalizedReplay, phase);
  const initialDrawCount = wholeNumber(
    fragment.initialDrawCount,
    `${label}.initialDrawCount`,
  );
  if (initialDrawCount !== phase.entry.retailDrawCount) {
    throw new Error(
      `${label}.initialDrawCount does not match phase.entry.retailDrawCount`,
    );
  }
  const frames = wholeNumber(fragment.frames, `${label}.frames`, 5_000_000);
  if (frames !== normalizedReplay.totalFrames) {
    throw new Error(
      `${label}.frames (${frames}) does not equal its segment frame sum ` +
        `(${normalizedReplay.totalFrames})`,
    );
  }
  const fragmentExitConflicts = expectationConflict(
    normalizedReplay.expect,
    phase.exit,
  );
  if (fragmentExitConflicts.length > 0) {
    throw new Error(
      `${label}.expect conflicts with phase.exit: ${fragmentExitConflicts.join(", ")}`,
    );
  }

  if (fragment.transition == null) {
    if (phase.exit.currentLid !== phase.entry.currentLid) {
      throw new Error(
        `${label} has no authored transition but its phase changes LID`,
      );
    }
  } else {
    if (!isObject(fragment.transition)) {
      throw new Error(`${label}.transition must be an object or null`);
    }
    const transitionFrame = wholeNumber(
      fragment.transition.frame,
      `${label}.transition.frame`,
      5_000_000,
    );
    const transitionLid = wholeNumber(
      fragment.transition.lid,
      `${label}.transition.lid`,
      0xff,
    );
    if (transitionFrame !== frames) {
      throw new Error(`${label}.transition.frame must equal fragment.frames`);
    }
    if (transitionLid !== phase.exit.currentLid) {
      throw new Error(
        `${label}.transition.lid does not match the exact phase exit LID`,
      );
    }
  }

  const requiredMetadata = [
    "entryCheckpoint",
    "exitCheckpoint",
    "entryProgression",
    "exitProgression",
    "initialPad",
    "finalPad",
  ];
  const missingMetadata = requiredMetadata.filter(
    (field) => fragment[field] === undefined,
  );
  if (missingMetadata.length > 0) {
    throw new Error(
      `${label} is missing exact capture metadata: ${missingMetadata.join(", ")}`,
    );
  }

  const entryCheckpoint = normalizeCheckpoint(
    fragment.entryCheckpoint,
    `${label}.entryCheckpoint`,
  );
  const exitCheckpoint = normalizeCheckpoint(
    fragment.exitCheckpoint,
    `${label}.exitCheckpoint`,
  );
  for (const [name, captured, declared] of [
    ["entryCheckpoint", entryCheckpoint, phase.entry],
    ["exitCheckpoint", exitCheckpoint, phase.exit],
  ]) {
    const differences = checkpointDifference(captured, declared);
    if (differences.length > 0) {
      throw new Error(
        `${label}.${name} does not match the exact manifest checkpoint: ` +
          differences.join(", "),
      );
    }
  }

  const entryProgression = normalizeProgression(
    fragment.entryProgression,
    `${label}.entryProgression`,
  );
  const exitProgression = normalizeProgression(
    fragment.exitProgression,
    `${label}.exitProgression`,
  );
  const initialPad = normalizePadSnapshot(
    fragment.initialPad,
    `${label}.initialPad`,
  );
  const finalPad = normalizePadSnapshot(
    fragment.finalPad,
    `${label}.finalPad`,
  );
  const replayedFinalPad = replayPad(initialPad, normalizedReplay.segments);
  const differences = checkpointDifference(replayedFinalPad, finalPad);
  if (differences.length > 0) {
    throw new Error(
      `${label}.finalPad does not match its ordered input segments: ` +
        differences.join(", "),
    );
  }
  return {
    entryCheckpoint,
    exitCheckpoint,
    entryProgression,
    exitProgression,
    initialPad,
    finalPad,
  };
}

function guardedPhaseSegments(fragment, normalizedReplay, phase) {
  const permitsIntermediateMounts = fragment.captureKind === "publisher-title";
  const guard = {
    currentLid: phase.entry.currentLid,
    mountedLid: phase.entry.mountedLid,
  };
  const segments = normalizedReplay.segments.map((segment, index) => {
    if (segment.while !== undefined) {
      const conflicts = expectationConflict(segment.while, guard);
      if (
        conflicts.length > 0
        || Object.keys(segment.while).length !== Object.keys(guard).length
      ) {
        throw new Error(
          `fragment for phase ${JSON.stringify(phase.id)} segment ${index + 1} ` +
            "has a LID guard that does not match its exact entry mount",
        );
      }
    }
    const composed = { ...segment };
    if (permitsIntermediateMounts) {
      delete composed.while;
    } else {
      composed.while = { ...guard };
    }
    return composed;
  });
  const last = segments.at(-1);
  const fragmentExpectation = mergeExpectations(
    last.expect,
    normalizedReplay.expect,
    `fragment for phase ${JSON.stringify(phase.id)} final fragment expectation`,
  );
  last.expect = mergeExpectations(
    fragmentExpectation,
    phase.exit,
    `fragment for phase ${JSON.stringify(phase.id)} final expectation`,
  );
  if (phase.entry.currentLid !== phase.exit.currentLid) {
    // A standalone exported fragment asks for one destination execution so
    // its smoke test proves that the newly mounted stream is runnable. In a
    // composed campaign that extra frame belongs to the following phase and
    // would shift every native-origin input by one frame. Exact phase exit
    // checkpoint fields remain authoritative and may still advance the
    // destination deliberately when they describe a later checkpoint.
    delete last.expect.minRetailExecutions;
  }
  const phaseSettleFrames =
    phase.settleFrames ?? normalizedReplay.settleFrames;
  const combinedSettleFrames = last.settleFrames + phaseSettleFrames;
  if (combinedSettleFrames > 10_000) {
    throw new Error(
      `fragment for phase ${JSON.stringify(phase.id)} requires more than ` +
        "10,000 final settle frames",
    );
  }
  last.settleFrames = combinedSettleFrames;
  return segments;
}

export async function composeCampaignReplay(rawManifest, loadFragment) {
  if (typeof loadFragment !== "function") {
    throw new Error("loadFragment must be a function");
  }
  const manifest = normalizeCampaignManifest(rawManifest);
  const outputSegments = [];
  let traceFromSegment;
  let previousCaptureMetadata;
  let previousPhase;
  for (const phase of manifest.phases) {
    if (phase.id === manifest.traceFromPhase) {
      traceFromSegment = outputSegments.length + 1;
    }
    const fragment = await loadFragment(phase.fragment, phase);
    if (!isObject(fragment)) {
      throw new Error(
        `fragment loader returned no JSON object for phase ${JSON.stringify(phase.id)}`,
      );
    }
    const fragmentWithInputKind = {
      ...fragment,
      segments: Array.isArray(fragment.segments)
        ? fragment.segments.map((segment) => ({
            ...segment,
            inputKind: segment.inputKind ?? phase.inputKind,
          }))
        : fragment.segments,
    };
    const normalizedReplay = normalizeReplay(fragmentWithInputKind);
    const captureMetadata = validateFragmentMetadata(
      fragment,
      normalizedReplay,
      phase,
      manifest,
    );
    if (captureMetadata.initialPad !== undefined && outputSegments.length === 0) {
      const bootPad = {
        tapped: 0,
        held: 0,
        tappedPrevious: 0,
        heldPrevious: 0,
        heldPrevious2: 0,
      };
      const differences = checkpointDifference(
        captureMetadata.initialPad,
        bootPad,
      );
      if (differences.length > 0) {
        throw new Error(
          `first fragment ${JSON.stringify(phase.id)} does not begin with the ` +
            `browser's physical boot pad: ${differences.join(", ")}`,
        );
      }
    }
    if (
      previousCaptureMetadata?.exitProgression !== undefined
      && captureMetadata.entryProgression !== undefined
    ) {
      const differences = checkpointDifference(
        previousCaptureMetadata.exitProgression,
        captureMetadata.entryProgression,
      );
      if (differences.length > 0) {
        throw new Error(
          `captured progression is discontinuous between ` +
            `${JSON.stringify(previousPhase.id)} and ${JSON.stringify(phase.id)}: ` +
            differences.join(", "),
        );
      }
    }
    if (
      previousCaptureMetadata?.finalPad !== undefined
      && captureMetadata.initialPad !== undefined
    ) {
      const expectedInitialPad =
        previousPhase.entry.currentLid === previousPhase.exit.currentLid
          ? previousCaptureMetadata.finalPad
          : advancePadSnapshot(
              previousCaptureMetadata.finalPad,
              previousCaptureMetadata.finalPad.held,
            );
      const differences = checkpointDifference(
        expectedInitialPad,
        captureMetadata.initialPad,
      );
      if (differences.length > 0) {
        throw new Error(
          `captured physical-pad history is discontinuous between ` +
            `${JSON.stringify(previousPhase.id)} and ${JSON.stringify(phase.id)}: ` +
            differences.join(", "),
        );
      }
    }
    outputSegments.push(
      ...guardedPhaseSegments(fragment, normalizedReplay, phase),
    );
    previousCaptureMetadata = captureMetadata;
    previousPhase = phase;
  }
  const finalCheckpoint = manifest.phases.at(-1).exit;
  const replay = {
    schema: 1,
    bootLid: manifest.bootLid,
    unlockAll: manifest.unlockAll,
    settleFrames: manifest.settleFrames,
    segments: outputSegments,
    expect: { ...finalCheckpoint },
  };
  if (traceFromSegment !== undefined) {
    replay.traceFromSegment = traceFromSegment;
  }
  const normalized = normalizeReplay(replay);
  return {
    schema: 1,
    localDiagnosticOnly: true,
    canonicalCampaign: false,
    bootLid: normalized.bootLid,
    unlockAll: normalized.unlockAll,
    ...(normalized.traceFromSegment === undefined
      ? {}
      : { traceFromSegment: normalized.traceFromSegment }),
    settleFrames: normalized.settleFrames,
    segments: normalized.segments,
    expect: normalized.expect,
    composition: {
      schema: 1,
      crossLidExitPolicy: "exact-checkpoint-at-destination-mount",
      phaseIds: manifest.phases.map(({ id }) => id),
      insertedHandoffs: manifest.insertedHandoffs,
    },
  };
}

async function readJson(path, label) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    throw new Error(`could not read ${label} ${path}: ${error.message}`);
  }
}

export async function composeCampaignReplayFromFile(manifestPath) {
  const absoluteManifestPath = resolve(manifestPath);
  const manifest = await readJson(absoluteManifestPath, "campaign manifest");
  const manifestDirectory = dirname(absoluteManifestPath);
  const fragmentPaths = new Map();
  const replay = await composeCampaignReplay(manifest, async (reference) => {
    const path = isAbsolute(reference)
      ? resolve(reference)
      : resolve(manifestDirectory, reference);
    fragmentPaths.set(reference, path);
    return readJson(path, "campaign fragment");
  });
  return {
    replay,
    manifestPath: absoluteManifestPath,
    fragmentPaths: [...fragmentPaths.values()],
  };
}

export function isLocalArtifactPath(path) {
  const absolutePath = resolve(path);
  const relativePath = relative(repositoryRoot, absolutePath);
  if (
    relativePath === "" ||
    relativePath === ".." ||
    relativePath.startsWith(`..${sep}`) ||
    isAbsolute(relativePath)
  ) {
    return relativePath !== "";
  }
  return LOCAL_OUTPUT_ROOTS.has(relativePath.split(sep)[0]);
}

export async function writeComposedReplay(
  outputPath,
  replay,
  { force = false, protectedPaths = [] } = {},
) {
  const absoluteOutputPath = resolve(outputPath);
  if (!isLocalArtifactPath(absoluteOutputPath)) {
    throw new Error(
      "composed replay output must be outside the repository or under an " +
        "ignored local artifact directory",
    );
  }
  for (const protectedPath of protectedPaths) {
    if (absoluteOutputPath === resolve(protectedPath)) {
      throw new Error("composed replay output must not overwrite an input file");
    }
  }
  if (!force) {
    try {
      await access(absoluteOutputPath);
      throw new Error(
        `composed replay output already exists: ${absoluteOutputPath}; use --force to replace it`,
      );
    } catch (error) {
      if (!String(error?.message).includes("ENOENT")) throw error;
    }
  }
  await mkdir(dirname(absoluteOutputPath), { recursive: true });
  const temporaryPath = `${absoluteOutputPath}.${randomUUID()}.tmp`;
  try {
    await writeFile(
      temporaryPath,
      `${JSON.stringify(replay, null, 2)}\n`,
      { encoding: "utf8", mode: 0o600, flag: "wx" },
    );
    await rename(temporaryPath, absoluteOutputPath);
  } finally {
    await rm(temporaryPath, { force: true });
  }
  return absoluteOutputPath;
}
