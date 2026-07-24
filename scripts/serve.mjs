import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { verifyBuildInfo } from "./build-info.mjs";

const repositoryRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const distribution = process.env.CRUST_WEB_DIST || "dist";
const root = resolve(repositoryRoot, distribution);
if (!root.startsWith(`${repositoryRoot}/`)) {
  throw new Error("CRUST_WEB_DIST must stay inside the crust repository");
}
const port = Number(process.env.PORT || 4174);
const types = new Map([
  [".css", "text/css; charset=utf-8"],
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".mjs", "text/javascript; charset=utf-8"],
  [".wasm", "application/wasm"],
]);

const build = await verifyBuildInfo(repositoryRoot, undefined, root);

createServer(async (request, response) => {
  try {
    await verifyBuildInfo(repositoryRoot, undefined, root);
  } catch (error) {
    response.writeHead(503, { "Content-Type": "text/plain; charset=utf-8" });
    response.end(`Web distribution changed after server start: ${error.message}\n`);
    return;
  }
  const pathname = decodeURIComponent(new URL(request.url || "/", "http://local").pathname);
  const requested = pathname === "/" ? "/index.html" : pathname;
  const file = normalize(join(root, requested));
  if (!file.startsWith(`${root}/`) || !existsSync(file) || !statSync(file).isFile()) {
    response.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
    response.end("Not found\n");
    return;
  }
  response.writeHead(200, {
    "Cache-Control": "no-store",
    "Cross-Origin-Opener-Policy": "same-origin",
    "Cross-Origin-Resource-Policy": "same-origin",
    "Content-Type": types.get(extname(file)) || "application/octet-stream",
  });
  createReadStream(file).pipe(response);
}).listen(port, "127.0.0.1", () => {
  process.stdout.write(
    `crust dev server: http://127.0.0.1:${port} (${build.build_id})\n`,
  );
});
