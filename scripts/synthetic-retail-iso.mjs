const LOGICAL_SECTOR_SIZE = 2_048;
const RAW_SECTOR_SIZE = 2_352;
const SECTOR_COUNT = 40;

// Exact public stream identifiers from the checked-in NTSC-U catalog. The
// fixture contains no retail payload: every synthetic stream is one zero byte.
export const SYNTHETIC_RETAIL_LEVEL_IDS = Object.freeze([
  0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0c, 0x0e, 0x0f,
  0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
  0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26,
  0x28, 0x29, 0x2a, 0x2c, 0x2d, 0x2e, 0x33, 0x34, 0x37, 0x38, 0x39,
]);

export const SYNTHETIC_COOKED_ISO_BYTES =
  SECTOR_COUNT * LOGICAL_SECTOR_SIZE;

function writeBothEndianU16(bytes, offset, value) {
  bytes.writeUInt16LE(value, offset);
  bytes.writeUInt16BE(value, offset + 2);
}

function writeBothEndianU32(bytes, offset, value) {
  bytes.writeUInt32LE(value, offset);
  bytes.writeUInt32BE(value, offset + 4);
}

function directoryRecord(identifier, extent, size, directory) {
  const identifierBytes = Buffer.from(identifier);
  const length =
    33 + identifierBytes.length + Number(identifierBytes.length % 2 === 0);
  const bytes = Buffer.alloc(length);
  bytes[0] = length;
  writeBothEndianU32(bytes, 2, extent);
  writeBothEndianU32(bytes, 10, size);
  bytes[25] = directory ? 2 : 0;
  writeBothEndianU16(bytes, 28, 1);
  bytes[32] = identifierBytes.length;
  identifierBytes.copy(bytes, 33);
  return bytes;
}

function appendRecord(sector, cursor, record) {
  if (cursor.offset + record.length > sector.length) {
    throw new Error("synthetic ISO directory does not fit one logical sector");
  }
  record.copy(sector, cursor.offset);
  cursor.offset += record.length;
}

function sector(image, lba) {
  const start = lba * LOGICAL_SECTOR_SIZE;
  return image.subarray(start, start + LOGICAL_SECTOR_SIZE);
}

export function createSyntheticRetailCookedIso() {
  const image = Buffer.alloc(SYNTHETIC_COOKED_ISO_BYTES);
  const primary = sector(image, 16);
  primary[0] = 1;
  primary.write("CD001", 1, "ascii");
  primary[6] = 1;
  writeBothEndianU32(primary, 80, SECTOR_COUNT);
  writeBothEndianU16(primary, 128, LOGICAL_SECTOR_SIZE);
  directoryRecord(Buffer.from([0]), 20, LOGICAL_SECTOR_SIZE, true).copy(
    primary,
    156,
  );

  const terminator = sector(image, 17);
  terminator[0] = 255;
  terminator.write("CD001", 1, "ascii");
  terminator[6] = 1;

  const root = sector(image, 20);
  const rootCursor = { offset: 0 };
  appendRecord(
    root,
    rootCursor,
    directoryRecord(Buffer.from([0]), 20, LOGICAL_SECTOR_SIZE, true),
  );
  appendRecord(
    root,
    rootCursor,
    directoryRecord(Buffer.from([1]), 20, LOGICAL_SECTOR_SIZE, true),
  );
  for (let directory = 0; directory < 4; directory += 1) {
    appendRecord(
      root,
      rootCursor,
      directoryRecord(
        `S${directory}`,
        21 + directory,
        LOGICAL_SECTOR_SIZE,
        true,
      ),
    );
  }

  for (let directory = 0; directory < 4; directory += 1) {
    const streamDirectory = sector(image, 21 + directory);
    const cursor = { offset: 0 };
    appendRecord(
      streamDirectory,
      cursor,
      directoryRecord(Buffer.from([0]), 21, LOGICAL_SECTOR_SIZE, true),
    );
    appendRecord(
      streamDirectory,
      cursor,
      directoryRecord(Buffer.from([1]), 20, LOGICAL_SECTOR_SIZE, true),
    );
    for (const level of SYNTHETIC_RETAIL_LEVEL_IDS) {
      if (level >> 4 !== directory) continue;
      for (const extension of ["NSD", "NSF"]) {
        const name = `S${level.toString(16).padStart(7, "0").toUpperCase()}.${extension};1`;
        appendRecord(
          streamDirectory,
          cursor,
          directoryRecord(name, 30, 1, false),
        );
      }
    }
  }

  return image;
}

export function expectedSyntheticCookedIsoBlobRanges() {
  return [
    {
      sourceSize: SYNTHETIC_COOKED_ISO_BYTES,
      start: 16 * RAW_SECTOR_SIZE,
      end: 17 * RAW_SECTOR_SIZE,
    },
    ...[16, 20, 21, 22, 23, 24].map((lba) => ({
      sourceSize: SYNTHETIC_COOKED_ISO_BYTES,
      start: lba * LOGICAL_SECTOR_SIZE,
      end: (lba + 1) * LOGICAL_SECTOR_SIZE,
    })),
  ];
}
