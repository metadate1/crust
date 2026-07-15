window.__consoleErrors = [];
window.addEventListener("error", (event) => {
  window.__consoleErrors.push(String(event.error?.stack || event.message || event.error));
});
window.addEventListener("unhandledrejection", (event) => {
  window.__consoleErrors.push(String(event.reason?.stack || event.reason));
});

async function start() {
  window.__crustBootstrap = "loading-build";
  const response = await fetch("./build-info.json", { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`Could not load build identity (${response.status})`);
  }
  const build = await response.json();
  if (build?.schema !== 1 || typeof build.build_id !== "string") {
    throw new Error("Build identity is missing or incompatible");
  }
  window.__crustBuild = Object.freeze(build);
  document.documentElement.dataset.crustBuild = build.build_id;
  const log = document.querySelector("#runtimeLog");
  if (log) log.textContent += `\n> web build ${build.build_id}`;

  window.__crustBootstrap = "loading-wasm";
  const version = encodeURIComponent(build.build_id);
  const { default: init, boot } = await import(`./pkg/crust_web.js?build=${version}`);
  const wasm = new URL(`./pkg/crust_web_bg.wasm?build=${version}`, window.location.href);
  await init({ module_or_path: wasm });
  window.__crustBootstrap = "starting-rust";
  boot();
  window.__crustBootstrap = "running";
}

start().catch((error) => {
  window.__crustBootstrap = "failed";
  window.__consoleErrors.push(String(error?.stack || error));
  const status = document.querySelector("#runtimeStatus");
  if (status) status.textContent = "Runtime bootstrap failed";
  const log = document.querySelector("#runtimeLog");
  if (log) log.textContent += `\n! ${String(error?.stack || error)}`;
  throw error;
});
