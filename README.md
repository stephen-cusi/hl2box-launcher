# hl2box launcher

A companion launcher for hl2box (hl2sb). The engine's own `hl2_launcher`
starts the game and nothing else; this launcher fills the gap a no-steam
install has: no Steam client means no workshop, no identity management, no
addon browser. It is a separate executable - it never patches, wraps or
replaces `hl2_launcher`. The Launch button simply starts the game with the
command line shown on the front page.

## What it does

- **Launch** - edit the command line (extra args, start map), see the full
  command, start the game.
- **Player** - name, SteamID and avatar. The SteamID can be generated or
  pasted in any form (`7656119...`, `STEAM_0:Y:Z`, `[U:1:N]`). The name
  rides the command line as `+name`, the id as `-steamid <id64>`. The
  avatar is stored as a 184x184 PNG under `data/avatars/`, written under
  both the steamid64 key and the player-name key - the two files the
  game's own AvatarImage control looks up.
- **Addons** - the addon manager. Lists every folder and `.gma` in
  `addons/`, shows the disable state, toggles it (this is the engine's own
  `addons_disabled.txt`, read back on the next map load), imports `.gma`
  files, shows gma metadata, the embedded workshop preview image and a
  viewer for text entries inside the archive (lua/json/txt/vmt...).
- **Workshop** - paste a workshop id or item URL, fetch the details and
  download the `.gma` straight into `addons/` using the anonymous Steam
  web API. No login, no Steam client.
- **Mounts** - every Source game directory found next to the engine
  (hl2, episodic, portal, cstrike, ...) can be mounted with a checkbox.
  Writes a marked block into `gameinfo.txt`; hand-written SearchPath lines
  are never touched. Unmount = tick off and apply.
- **Logs** - tail `hl2sb_lua.log` (Lua output), `ds_debug.log` (server
  debug), `console.log` (when `-condebug`/`con_log` is on) and the newest
  crash-dump report, with auto refresh.
- **About** - launcher version/build and the engine build stamp.

## Languages and theme

UI strings live in flat JSON files with `{{name}}` placeholders (the
format every web i18n editor - i18next, Lokalise, Crowdin - opens
directly):

```text
crates/hl2box-launcher/lang/zh-CN.json
crates/hl2box-launcher/lang/en.json
```

`zh-CN` and `en` are compiled in as fallbacks. At startup every
`<locale>.json` in `lang/` next to the executable is loaded on top, so a
new language (or a typo fix) is a dropped-in file, no rebuild; the
language picker lists everything found. `auto` resolves through the OS
locale (exact tag, then language part: `zh-TW` picks up `zh-CN` until a
`zh-TW.json` appears).

CJK glyphs need a CJK font: the launcher loads the platform UI font at
startup (Microsoft YaHei on Windows, PingFang on macOS, Noto/WQY on
Linux) instead of shipping megabytes of font data.

The theme picker (top right) offers follow-system / light / dark.
Follow-system reads the OS answer (Windows `AppsUseLightTheme` registry
key, macOS `AppleInterfaceStyle`, GNOME `color-scheme`) and re-checks
every few seconds, so flipping the system theme flips the launcher
live.

## Layout

```text
launcher/
  crates/hl2box-core/       pure logic: identity, steamid, gma parsing,
                            addon management, mounts, launch args, logs,
                            workshop client. No GUI, no unsafe.
  crates/hl2box-launcher/   desktop GUI (eframe/egui).
  crates/hl2box-ffi/        C ABI over hl2box-core for native embedders,
                            ships include/hl2box.h.
```

The desktop GUI and the FFI are both thin layers over the same core, so
behaviour cannot drift between platforms.

## Building

Rust 1.80 or newer (stable).

```sh
# Windows
cd launcher
cargo build --release
# -> target/release/hl2box-launcher.exe

# Linux
cargo build --release
# -> target/release/hl2box-launcher
# GUI dependencies: whatever your distro calls the xcb/libx11 dev packages
# (Debian: libxcb1-dev libxkbcommon-dev libwayland-dev, see eframe docs)

# macOS
cargo build --release
```

Drop the executable next to `hl2_launcher` in the engine root (or anywhere
and set the engine root on the Launch page - the first run guesses from
the executable location). Settings live in `hl2box-launcher.json` beside
the executable.

Build the FFI library for a native embedder:

```sh
cd launcher
cargo build -p hl2box-ffi --release
# -> target/release/hl2box.dll / libhl2box.so / libhl2box.dylib
# staticlib: target/release/libhl2box.a
```

## Android

The Android launcher (separate project) embeds this crate through the FFI:

```sh
# from the launcher/ directory, with the NDK's clang in PATH
cargo ndk -t arm64-v8a -t armeabi-v7a -p hl2box-ffi -o <jniLibs dir> build --release
```

or plain cargo with the NDK target/linker configured. The crate builds as
`cdylib` (`libhl2box.so`) and `staticlib`; declare the JNI natives for the
functions in `crates/hl2box-ffi/include/hl2box.h` (see that header for the
string/ownership conventions). Core logic and the FFI are `no unsafe`
apart from the C boundary itself, and the `net` feature (workshop) can be
turned off with `--no-default-features` for a smaller library.

## Files the launcher touches

| File | Meaning |
| --- | --- |
| `hl2sb/addons_disabled.txt` | addon enable/disable (engine's own format) |
| `hl2sb/gameinfo.txt` | launcher mounts live in a marked block only |
| `hl2sb/data/hl2box/identity.json` | name / steam id / avatar pointer |
| `hl2sb/data/avatars/<key>.png` | avatar PNGs (steamid64 + player-name keys) |
| `hl2box-launcher.json` (beside the exe) | launcher settings |
| `lang/*.json` (beside the exe) | UI translations |

Nothing else is written; the launcher never modifies engine binaries or
the game's config files.

## Changes in 0.2

- Addons page: list mode (overview, files, Lua listing grouped by realm,
  text viewer) and folder mode (browse the real `addons/` directory).
- Full-text search across every addon's Lua; click a hit to open the file at
  that line.
- Text viewer: line numbers, find with prev/next, wrap, copy all, Lua
  syntax colouring (long comments and strings span lines).
- Logs: level filter with counts, text filter, follow tail, virtualised list.
- Launch: start-map picker (scans `maps/` and `hl2/maps`), named presets for
  extra args + start map, live checks that block launch on hard errors.
- Workshop downloads show real progress.
- Light/dark palette with an HL2-orange accent and GMod-blue highlights; new
  icon (`res/launcher.ico`, `crates/hl2box-launcher/assets/`). Generator:
  `tools/make_icon.py`.

## Changes in 0.3

- Logs are parsed into time, level, source tag and message. Engine stamps
  (`[637.3787]`) and Lua stamps (`L 10/27/2026 - 03:13:21:`) both work.
- Source chips are built from the `[Tag]`s in the file, so `[lua]`, `[HL2SB]`
  and `[HL2SB wp]` can be shown or hidden independently.
- Repeated consecutive lines fold into one row with a count (`x12`).
- Session banners (`>>> Engine closed ...`) stand out as separators.
- Prev / next error buttons jump the table to the next error.
- Click a row to copy it; "copy visible" and "save filtered..." work on the
  current filter.
- Engine log is found at `<engine root>/engine.log` (older builds used
  `hl2sb_engine.log`, still accepted).
- Crash handler log (`hl2sb_crash.log`) is listed.
- Decoding: UTF-8, UTF-16 with BOM, or GBK (Chinese Windows console). The
  reader keeps whichever decodes cleanly instead of showing replacement marks.
- Binary crash dumps (`.mdmp` / `.dmp`) are never decoded as text. They show a
  banner and their printable strings (module paths, exception name, build).

## Changes in 0.4

- Crash dumps (`.mdmp` / `.dmp`) in `hl2sb/dumps/` are analysed by the
  launcher instead of shown as text. The Logs page shows, for the newest dump:
  the exception (code, name, faulting address), the faulting code disassembled
  with the NULL register or the RIP-relative global it reads annotated, the
  registers at the exception, the modules with their symbol status, the call
  chain where return addresses can be verified, and notes on what the dump
  does not contain.
- Symbols come from the PDB next to each binary: `<engine>/hl2sb/bin/<name>.pdb`
  and `<engine>/bin/<name>.pdb`. A PDB is used only when its GUID matches the
  module's CodeView record. Public symbols are demangled.
- The "show raw strings" button switches back to the printable-strings view.
- Analysis is cached per file version; re-analysis happens only when the dump
  changes.

## Changes in 0.5

- Windows: `hl2box-launcher.exe` embeds its icon (`res/launcher.ico`) and a
  version resource (file/product name, company, copyright, version 0.5.0).
  The resource is compiled by `crates/hl2box-launcher/build.rs` only when the
  target is Windows. Building for Windows needs the resource compiler from the
  Windows SDK (installed with Visual Studio Build Tools); without it the build
  stops with a message rather than producing an exe without its icon.
- Icon redrawn as a flat mark: steel plate, HL2 orange lambda, one blue
  square. No gradients, glow or gloss. Generator: `tools/make_icon.py`.
- Mountable games: symbolic links are not followed (a `garrysmod` link to the
  Steam install is no longer offered), and tooling, cache, dump, and mod-copy
  folders (e.g. `hl2sb_clear`) are skipped.
- Crash dumps: the source picker lists every dump in `hl2sb/dumps/`, newest
  first (up to 30), not only the newest one.
- Crash analysis runs on a worker thread. Symbols are read only for the
  modules the fault or the call chain lands in; other modules show "not on the
  fault or call path". A 1.5 GB `server.pdb` is opened only when needed.
