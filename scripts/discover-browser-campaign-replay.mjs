#!/usr/bin/env node

import { readdir, readFile } from "node:fs/promises";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  discoverLongestCampaignManifest,
  isLocalArtifactPath,
  writeDiscoveredCampaignManifest,
} from "./browser-campaign-replay.mjs";

const EXPORTED_FRAGMENT_NAME =
  /^lid-[0-9a-f]{2}-draw-[0-9]+-.+-to-[0-9a-f]{2}\.json$/i;

export function usage() {
  return `Usage:
  node scripts/discover-browser-campaign-replay.mjs --fragments PATH [options]

Options:
  --fragments PATH           Ignored/local directory of exported fragments
  --output PATH              Ignored/local discovered manifest JSON
  --trace-input-profile NAME Begin browser tracing at this captured profile
  --check                    Discover and validate without writing a manifest
  --force                    Replace an existing ignored manifest
  --help                     Show this help

The directory must contain exactly one unambiguous, fully connected campaign
path exported as lid-*-draw-*-*-to-*.json files. Discovery orders existing
captures by exact checkpoint, progression, and physical-pad continuity. It
does not generate, alter, or bridge controller input.
`;
}

export function parseArguments(argv) {
  const options = {
    fragments: undefined,
    output: undefined,
    traceInputProfile: undefined,
    check: false,
    force: false,
    help: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const value = () => {
      index += 1;
      if (index >= argv.length) throw new Error(`${argument} requires a value`);
      return argv[index];
    };
    switch (argument) {
      case "--fragments":
        options.fragments = resolve(value());
        break;
      case "--output":
        options.output = resolve(value());
        break;
      case "--trace-input-profile":
        options.traceInputProfile = value();
        break;
      case "--check":
        options.check = true;
        break;
      case "--force":
        options.force = true;
        break;
      case "--help":
      case "-h":
        options.help = true;
        break;
      default:
        throw new Error(`unknown argument: ${argument}`);
    }
  }
  if (!options.help && options.fragments === undefined) {
    throw new Error("--fragments is required");
  }
  if (!options.help && !options.check && options.output === undefined) {
    throw new Error("--output is required unless --check is used");
  }
  if (options.check && options.output !== undefined) {
    throw new Error("--output cannot be combined with --check");
  }
  if (options.check && options.force) {
    throw new Error("--force cannot be combined with --check");
  }
  return options;
}

async function readFragment(path) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    throw new Error(`could not read campaign fragment ${path}: ${error.message}`);
  }
}

export async function loadExportedCampaignFragments(
  fragmentDirectory,
  { referenceDirectory = fragmentDirectory } = {},
) {
  const absoluteDirectory = resolve(fragmentDirectory);
  if (!isLocalArtifactPath(absoluteDirectory)) {
    throw new Error(
      "campaign fragment directory must be outside the repository or under " +
        "an ignored local artifact directory",
    );
  }
  let directoryEntries;
  try {
    directoryEntries = await readdir(absoluteDirectory, {
      withFileTypes: true,
    });
  } catch (error) {
    throw new Error(
      `could not read campaign fragment directory ${absoluteDirectory}: ` +
        error.message,
    );
  }
  const fragmentPaths = directoryEntries
    .filter(
      (entry) => entry.isFile() && EXPORTED_FRAGMENT_NAME.test(entry.name),
    )
    .map((entry) => resolve(absoluteDirectory, entry.name))
    .sort();
  if (fragmentPaths.length === 0) {
    throw new Error(
      `campaign fragment directory ${absoluteDirectory} contains no ` +
        "lid-*-draw-*-*-to-*.json exports",
    );
  }
  const absoluteReferenceDirectory = resolve(referenceDirectory);
  const fragmentEntries = await Promise.all(
    fragmentPaths.map(async (path) => ({
      fragment: relative(absoluteReferenceDirectory, path),
      document: await readFragment(path),
    })),
  );
  return {
    directory: absoluteDirectory,
    fragmentEntries,
    fragmentPaths,
  };
}

export async function run(options) {
  const referenceDirectory =
    options.output === undefined
      ? resolve(options.fragments)
      : dirname(resolve(options.output));
  const loaded = await loadExportedCampaignFragments(options.fragments, {
    referenceDirectory,
  });
  const manifest = discoverLongestCampaignManifest(loaded.fragmentEntries, {
    traceInputProfile: options.traceInputProfile,
    requireComplete: true,
    rejectAmbiguous: true,
  });
  const frames = loaded.fragmentEntries.reduce(
    (sum, { document }) => sum + Number(document.frames),
    0,
  );
  const summary = {
    fragments: loaded.fragmentPaths.length,
    phases: manifest.phases.length,
    frames,
    bootLid: manifest.bootLid,
    finalLid: manifest.phases.at(-1).exit.currentLid,
  };
  if (options.check) {
    return { ...summary, checked: true };
  }
  const output = await writeDiscoveredCampaignManifest(
    options.output,
    manifest,
    {
      force: options.force,
      protectedPaths: loaded.fragmentPaths,
    },
  );
  return { ...summary, output };
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.help) {
    process.stdout.write(usage());
    return;
  }
  const result = await run(options);
  process.stdout.write(
    `browser campaign replay discovered: ${JSON.stringify(result)}\n`,
  );
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    process.stderr.write(
      `browser campaign replay discovery failed: ${error.stack ?? error}\n`,
    );
    process.exitCode = 1;
  });
}
