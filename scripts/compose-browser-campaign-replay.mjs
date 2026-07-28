#!/usr/bin/env node

import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  composeCampaignReplayFromFile,
  writeComposedReplay,
} from "./browser-campaign-replay.mjs";

export function usage() {
  return `Usage:
  node scripts/compose-browser-campaign-replay.mjs --manifest PATH [options]

Options:
  --manifest PATH  Ignored/local ordered campaign manifest
  --output PATH    Ignored/local harness replay JSON
  --check          Validate and summarize without writing a replay
  --force          Replace an existing output file
  --help           Show this help

The manifest and all referenced fragments must opt in with
localDiagnosticOnly=true and canonicalCampaign=false. Repository-local output
is accepted only under target/, local-data/, artifacts/, captures/, or
recordings/.
`;
}

export function parseArguments(argv) {
  const options = {
    manifest: undefined,
    output: undefined,
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
      case "--manifest":
        options.manifest = resolve(value());
        break;
      case "--output":
        options.output = resolve(value());
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
  if (!options.help && options.manifest === undefined) {
    throw new Error("--manifest is required");
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

export async function run(options) {
  const composed = await composeCampaignReplayFromFile(options.manifest);
  const summary = {
    phases: composed.replay.composition.phaseIds.length,
    handoffs: composed.replay.composition.insertedHandoffs.length,
    segments: composed.replay.segments.length,
    frames: composed.replay.segments.reduce(
      (sum, segment) => sum + segment.frames,
      0,
    ),
    bootLid: composed.replay.bootLid,
    finalLid: composed.replay.expect.currentLid,
  };
  if (options.check) return { ...summary, checked: true };
  const output = await writeComposedReplay(options.output, composed.replay, {
    force: options.force,
    protectedPaths: [
      composed.manifestPath,
      ...composed.fragmentPaths,
    ],
  });
  return { ...summary, output };
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.help) {
    process.stdout.write(usage());
    return;
  }
  const result = await run(options);
  process.stdout.write(`browser campaign replay composed: ${JSON.stringify(result)}\n`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    process.stderr.write(
      `browser campaign replay composition failed: ${error.stack ?? error}\n`,
    );
    process.exitCode = 1;
  });
}
