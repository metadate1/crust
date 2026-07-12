window.__consoleErrors = [];
window.addEventListener("error", (event) => {
  window.__consoleErrors.push(String(event.error?.stack || event.message || event.error));
});
window.addEventListener("unhandledrejection", (event) => {
  window.__consoleErrors.push(String(event.reason?.stack || event.reason));
});

async function start() {
  window.__crustBootstrap = "loading-wasm";
  const { default: init, boot } = await import("./pkg/crust_web.js");
  await init();
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
