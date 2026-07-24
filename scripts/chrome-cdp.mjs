import { setTimeout as delay } from "node:timers/promises";

function messageText(data) {
  if (typeof data === "string") return data;
  if (data instanceof ArrayBuffer) {
    return new TextDecoder().decode(data);
  }
  if (ArrayBuffer.isView(data)) {
    return new TextDecoder().decode(data);
  }
  return String(data);
}

/**
 * Small Chrome DevTools Protocol client used by the legally-local browser
 * harness. Keeping it here avoids adding a browser-driver dependency or a
 * downloaded browser to the repository.
 */
export class ChromeCdp {
  #nextId = 1;
  #pending = new Map();
  #listeners = new Map();
  #socket;

  static async connect(webSocketUrl, timeoutMs = 10_000) {
    const socket = new WebSocket(webSocketUrl);
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(
        () => reject(new Error("timed out connecting to Chrome DevTools")),
        timeoutMs,
      );
      socket.addEventListener(
        "open",
        () => {
          clearTimeout(timeout);
          resolve();
        },
        { once: true },
      );
      socket.addEventListener(
        "error",
        () => {
          clearTimeout(timeout);
          reject(new Error("Chrome DevTools WebSocket failed to open"));
        },
        { once: true },
      );
    });
    return new ChromeCdp(socket);
  }

  constructor(socket) {
    this.#socket = socket;
    socket.addEventListener("message", (event) => {
      const message = JSON.parse(messageText(event.data));
      if (message.id !== undefined) {
        const pending = this.#pending.get(message.id);
        if (!pending) return;
        this.#pending.delete(message.id);
        if (message.error) {
          pending.reject(
            new Error(
              `${pending.method}: ${message.error.message}` +
                (message.error.data ? ` (${message.error.data})` : ""),
            ),
          );
        } else {
          pending.resolve(message.result ?? {});
        }
        return;
      }
      for (const listener of this.#listeners.get(message.method) ?? []) {
        listener(message.params ?? {}, message.sessionId);
      }
    });
    socket.addEventListener("close", () => {
      const error = new Error("Chrome DevTools WebSocket closed");
      for (const pending of this.#pending.values()) pending.reject(error);
      this.#pending.clear();
    });
  }

  command(method, params = {}, sessionId = undefined, timeoutMs = 20_000) {
    const id = this.#nextId++;
    const payload = { id, method, params };
    if (sessionId !== undefined) payload.sessionId = sessionId;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`${method}: timed out after ${timeoutMs} ms`));
      }, timeoutMs);
      this.#pending.set(id, {
        method,
        resolve: (result) => {
          clearTimeout(timeout);
          resolve(result);
        },
        reject: (error) => {
          clearTimeout(timeout);
          reject(error);
        },
      });
      this.#socket.send(JSON.stringify(payload));
    });
  }

  on(method, listener) {
    const listeners = this.#listeners.get(method) ?? new Set();
    listeners.add(listener);
    this.#listeners.set(method, listeners);
    return () => listeners.delete(listener);
  }

  async close() {
    if (this.#socket.readyState === WebSocket.CLOSED) return;
    this.#socket.close();
    for (let attempt = 0; attempt < 20; attempt += 1) {
      if (this.#socket.readyState === WebSocket.CLOSED) return;
      await delay(25);
    }
  }
}
