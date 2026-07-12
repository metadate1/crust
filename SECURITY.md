# Security and privacy

Treat every disc, NSD, NSF, card JSON and resume JSON as untrusted input. Parsers are bounds-checked
and unsafe Rust is forbidden. Please report malformed-input crashes or unexpectedly unbounded
allocation privately to the repository owner.

The browser application has no telemetry and no asset upload endpoint. Its content security policy
allows only same-origin code and Wasm loading. Game bytes remain in browser memory for the session;
only two documented 128-byte progression/options formats use `localStorage`.

