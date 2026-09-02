# Privacy

CRUST is designed to use game files locally in the browser.

## Game files

When you select a disc image or extracted stream files, the browser gives CRUST access to those
files on your device. CRUST reads them for the current session. It does not upload them to a CRUST
server, copy them into the repository, or keep them after the page closes.

You must select the files again after a reload.

## Saved browser data

CRUST can store two small, versioned records in the browser's `localStorage`:

- game progression; and
- options.

Each record contains a documented 128-byte retail payload inside a CRUST storage envelope. The
records stay in that browser profile for that site until you clear them.

## Network activity

The application has no telemetry, analytics, advertising, account system, or game-file upload
endpoint. Its content security policy limits connections to the same origin used to load the
application files.

If a future deployment adds a server feature, analytics, or another data flow, this document and
the application must be updated before that feature is published.

## Reports

Do not attach a disc image, BIOS, extracted stream, save, screenshot, recording, or other game data
to a public issue. Report a suspected data disclosure through the private process in
[SECURITY.md](SECURITY.md).
