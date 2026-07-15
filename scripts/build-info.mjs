import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readdir, readFile, stat, writeFile } from "node:fs/promises";
import { join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SCHEMA_VERSION = 1;
const SOURCE_ROOTS = [
  ".cargo",
  "crates",
  "web",
  "Cargo.lock",
  "Cargo.toml",
  "package.json",
  "rust-toolchain.toml",
  "rustfmt.toml",
  "scripts/build-info.mjs",
  "scripts/build-web.sh",
  "scripts/serve.mjs",
];
const ARTIFACTS = [
  "bootstrap.js",
  "index.html",
  "styles.css",
  "pkg/crust_web.js",
  "pkg/crust_web_bg.wasm",
];

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

async function collectFiles(path) {
  const metadata = await stat(path);
  if (metadata.isFile()) return [path];
  if (!metadata.isDirectory()) return [];

  const entries = await readdir(path, { withFileTypes: true });
  const nested = await Promise.all(
    entries
      .filter((entry) => !entry.isSymbolicLink())
      .map((entry) => collectFiles(join(path, entry.name))),
  );
  return nested.flat();
}

async function sourceFiles(root) {
  const files = [];
  for (const sourceRoot of SOURCE_ROOTS) {
    const path = join(root, sourceRoot);
    if (await exists(path)) files.push(...(await collectFiles(path)));
  }
  return files.sort((left, right) => {
    const leftName = relative(root, left).replaceAll("\\", "/");
    const rightName = relative(root, right).replaceAll("\\", "/");
    return leftName < rightName ? -1 : leftName > rightName ? 1 : 0;
  });
}

async function hashFiles(root, files) {
  const hash = createHash("sha256");
  for (const path of files) {
    const name = relative(root, path).replaceAll("\\", "/");
    const bytes = await readFile(path);
    hash.update(name);
    hash.update("\0");
    hash.update(String(bytes.length));
    hash.update("\0");
    hash.update(bytes);
    hash.update("\0");
  }
  return hash.digest("hex");
}

async function hashFile(path) {
  return createHash("sha256").update(await readFile(path)).digest("hex");
}

export async function sourceFingerprint(rootPath) {
  const root = resolve(rootPath);
  return hashFiles(root, await sourceFiles(root));
}

export async function artifactFingerprints(rootPath, distPath = undefined) {
  const root = resolve(rootPath);
  const dist = resolve(distPath ?? join(root, "dist"));
  const output = {};
  for (const artifact of ARTIFACTS) {
    const path = join(dist, artifact);
    if (!(await exists(path))) {
      throw new Error(`missing web artifact: dist/${artifact}`);
    }
    output[artifact] = await hashFile(path);
  }
  return output;
}

function gitIdentity(root) {
  const options = { cwd: root, encoding: "utf8" };
  const commit = execFileSync("git", ["rev-parse", "HEAD"], options).trim();
  const status = execFileSync(
    "git",
    ["status", "--porcelain=v1", "--untracked-files=all"],
    options,
  ).trim();
  return { commit, dirty: status.length > 0 };
}

function buildId(git, sourceSha256, artifacts) {
  const artifactHash = createHash("sha256");
  for (const [name, sha256] of Object.entries(artifacts).sort(([left], [right]) =>
    left < right ? -1 : left > right ? 1 : 0,
  )) {
    artifactHash.update(name);
    artifactHash.update("\0");
    artifactHash.update(sha256);
    artifactHash.update("\0");
  }
  return [
    git.commit.slice(0, 12),
    sourceSha256.slice(0, 12),
    artifactHash.digest("hex").slice(0, 12),
    git.dirty ? "dirty" : "clean",
  ].join("-");
}

export async function writeBuildInfo(
  rootPath,
  identity = undefined,
  distPath = undefined,
  expectedSourceSha256 = undefined,
) {
  const root = resolve(rootPath);
  const dist = resolve(distPath ?? join(root, "dist"));
  const sourceSha256 = await sourceFingerprint(root);
  if (
    expectedSourceSha256 !== undefined &&
    sourceSha256 !== expectedSourceSha256
  ) {
    throw new Error("runtime sources changed while the Wasm distribution was building");
  }
  const artifacts = await artifactFingerprints(root, dist);
  const git = identity ?? gitIdentity(root);
  const identityBuildId = buildId(git, sourceSha256, artifacts);
  const info = {
    schema: SCHEMA_VERSION,
    build_id: identityBuildId,
    commit: git.commit,
    dirty: git.dirty,
    source_sha256: sourceSha256,
    artifacts,
    built_at: new Date().toISOString(),
  };
  await writeFile(
    join(dist, "build-info.json"),
    `${JSON.stringify(info, null, 2)}\n`,
  );
  return info;
}

export async function verifyBuildInfo(rootPath, identity = undefined) {
  const root = resolve(rootPath);
  const manifestPath = join(root, "dist", "build-info.json");
  let info;
  try {
    info = JSON.parse(await readFile(manifestPath, "utf8"));
  } catch (error) {
    throw new Error(`web build metadata is missing or invalid: ${error.message}`);
  }
  if (
    info.schema !== SCHEMA_VERSION ||
    typeof info.build_id !== "string" ||
    typeof info.commit !== "string" ||
    typeof info.dirty !== "boolean" ||
    typeof info.source_sha256 !== "string" ||
    typeof info.artifacts !== "object" ||
    info.artifacts === null
  ) {
    throw new Error("web build metadata uses an unsupported schema");
  }

  const sourceSha256 = await sourceFingerprint(root);
  if (sourceSha256 !== info.source_sha256) {
    throw new Error(
      "web distribution is stale: source fingerprint differs; run `npm run build`",
    );
  }

  const artifacts = await artifactFingerprints(root);
  for (const [name, sha256] of Object.entries(artifacts)) {
    if (info.artifacts?.[name] !== sha256) {
      throw new Error(
        `web distribution is corrupt or stale: dist/${name} differs; run \`npm run build\``,
      );
    }
  }
  const recordedGit = { commit: info.commit, dirty: info.dirty };
  if (info.build_id !== buildId(recordedGit, sourceSha256, artifacts)) {
    throw new Error("web build metadata has an invalid build identity");
  }
  const git = identity ?? gitIdentity(root);
  if (git.commit !== info.commit || git.dirty !== info.dirty) {
    throw new Error(
      "web distribution was built for a different Git state; run `npm run build`",
    );
  }
  return info;
}

async function main() {
  const [
    command,
    root = resolve(fileURLToPath(new URL("..", import.meta.url))),
    dist,
    expectedSourceSha256,
  ] = process.argv.slice(2);
  if (command === "fingerprint") {
    process.stdout.write(`${await sourceFingerprint(root)}\n`);
    return;
  }
  if (command === "verify") {
    const info = await verifyBuildInfo(root);
    process.stdout.write(`verified crust web build: ${info.build_id}\n`);
    return;
  }
  if (command !== "write") {
    throw new Error(
      "usage: node scripts/build-info.mjs fingerprint|verify [repository-root] | " +
        "write [repository-root] [dist-path] [expected-source-sha256]",
    );
  }
  const info = await writeBuildInfo(
    root,
    undefined,
    dist,
    expectedSourceSha256,
  );
  process.stdout.write(`crust web build: ${info.build_id}\n`);
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    process.stderr.write(`${error.stack || error}\n`);
    process.exitCode = 1;
  });
}
