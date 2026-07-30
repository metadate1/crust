import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  SYNTHETIC_COOKED_ISO_BYTES,
  SYNTHETIC_RETAIL_LEVEL_IDS,
  createSyntheticRetailCookedIso,
  expectedSyntheticCookedIsoBlobRanges,
} from "./synthetic-retail-iso.mjs";

const LOGICAL_SECTOR_SIZE = 2_048;

function directoryIdentifiers(image, lba) {
  const bytes = image.subarray(
    lba * LOGICAL_SECTOR_SIZE,
    (lba + 1) * LOGICAL_SECTOR_SIZE,
  );
  const identifiers = [];
  for (let offset = 0; offset < bytes.length && bytes[offset] !== 0;) {
    const length = bytes[offset];
    const identifierLength = bytes[offset + 32];
    const identifier = bytes.subarray(
      offset + 33,
      offset + 33 + identifierLength,
    );
    identifiers.push(
      identifier.length === 1 && identifier[0] <= 1
        ? identifier[0] === 0 ? "." : ".."
        : identifier.toString("ascii"),
    );
    offset += length;
  }
  return identifiers;
}

test("synthetic cooked ISO reproduces the non-proprietary 40-sector catalog", () => {
  const image = createSyntheticRetailCookedIso();
  assert.equal(image.length, SYNTHETIC_COOKED_ISO_BYTES);
  assert.equal(image.subarray(16 * 2_048 + 1, 16 * 2_048 + 6).toString(), "CD001");
  assert.equal(image[16 * 2_048], 1);
  assert.equal(image.readUInt16LE(16 * 2_048 + 128), 2_048);
  assert.equal(image.readUInt16BE(16 * 2_048 + 130), 2_048);
  assert.equal(image[17 * 2_048], 255);

  assert.deepEqual(directoryIdentifiers(image, 20), [
    ".", "..", "S0", "S1", "S2", "S3",
  ]);
  const streamNames = [21, 22, 23, 24]
    .flatMap((lba) => directoryIdentifiers(image, lba))
    .filter((name) => name !== "." && name !== "..");
  assert.equal(SYNTHETIC_RETAIL_LEVEL_IDS.length, 44);
  assert.equal(streamNames.length, 88);
  assert.equal(new Set(streamNames).size, 88);
  for (const level of SYNTHETIC_RETAIL_LEVEL_IDS) {
    const stem = `S${level.toString(16).padStart(7, "0").toUpperCase()}`;
    assert(streamNames.includes(`${stem}.NSD;1`));
    assert(streamNames.includes(`${stem}.NSF;1`));
  }
  assert(image.subarray(30 * 2_048, 30 * 2_048 + 1).equals(Buffer.from([0])));
});

test("synthetic level identifiers stay locked to the Rust retail catalog", async () => {
  const source = await readFile(
    new URL("../crates/formats/src/stream/catalog.rs", import.meta.url),
    "utf8",
  );
  const catalogIds = [...source.matchAll(/^\s*level!\((0x[0-9a-f]+),/gm)]
    .map((match) => Number.parseInt(match[1].slice(2), 16));
  assert.deepEqual(catalogIds, SYNTHETIC_RETAIL_LEVEL_IDS);
});

test("expected browser reads prove raw-first detection and bounded cooked ranges", () => {
  const ranges = expectedSyntheticCookedIsoBlobRanges();
  assert.equal(ranges.length, 7);
  assert.deepEqual(ranges[0], {
    sourceSize: SYNTHETIC_COOKED_ISO_BYTES,
    start: 16 * 2_352,
    end: 17 * 2_352,
  });
  assert.deepEqual(
    ranges.slice(1).map(({ start, end }) => [start, end]),
    [16, 20, 21, 22, 23, 24].map((lba) => [
      lba * 2_048,
      (lba + 1) * 2_048,
    ]),
  );
  assert(ranges.every(({ sourceSize, start, end }) =>
    Number.isSafeInteger(start)
    && Number.isSafeInteger(end)
    && start >= 0
    && end > start
    && end <= sourceSize
    && end - start < sourceSize
  ));
});
