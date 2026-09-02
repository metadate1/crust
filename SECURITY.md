# Security and privacy reports

Treat every disc image, NSD/NSF stream, card record, and resume record as untrusted input.

Please report these problems privately through GitHub's private vulnerability reporting feature:

- a malformed file crashes or escapes the parser;
- an input causes unexpectedly large memory or CPU use;
- game data, credentials, or local paths are disclosed; or
- the browser sends data after a user selects local game files.

Do not attach proprietary game data to a public issue. If a report needs a reproducer, begin with a
small synthetic file or describe how the maintainer can create one.

The browser application has no telemetry, analytics, or game-file upload endpoint. Selected game
files remain local to the browser session. Two small progression and options records can be stored
in browser `localStorage`; [PRIVACY.md](PRIVACY.md) explains this boundary.

CRUST forbids unsafe Rust and uses bounds-checked parsers, but those controls do not guarantee that
the project has no security defects.

