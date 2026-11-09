# macOS packaging for the GPUI desktop shell

The GPUI shell replaces the retired Tauri v2 shell. This directory holds the
bundle metadata and the icon set it ships with; `desktop/scripts/build-app.sh`
assembles the `.app` from them.

```
desktop/gpui/packaging/
├── Info.plist          bundle metadata (identifier, version, icon, name)
└── icons/              the icon set, copied from the old Tauri icons
```

The icons are **copies**: `desktop/src-tauri/icons/` is being removed with the
Tauri shell, so this directory keeps the packaging self-contained. The files
are byte-identical to the ones the Tauri bundle used.

## Build

```sh
desktop/scripts/build-app.sh
```

That builds both binaries, assembles the bundle, signs it ad-hoc and verifies
it. To re-assemble without rebuilding (fast path while iterating on the
metadata):

```sh
desktop/scripts/build-app.sh --no-build
```

No network access is needed: both cargo builds use committed lockfiles and the
script itself never fetches anything.

## Output

```
desktop/gpui/dist/Heterion Router.app/
└── Contents/
    ├── Info.plist
    ├── MacOS/
    │   ├── heterion-router-desktop     the shell
    │   └── heterion-router-gateway     the sidecar
    └── Resources/
        ├── icon.icns                   the Dock / Finder icon
        └── …                           the rest of the icon set
```

Both binaries sit side by side in `Contents/MacOS/` on purpose: the shell looks
the gateway up next to its own executable
(`desktop/gpui/src/sidecar.rs`, `current_exe().parent()`), so the sidecar must
be a sibling of the app binary, not a resource.

## Run

```sh
open 'desktop/gpui/dist/Heterion Router.app'
```

The shell spawns its own gateway on port **20129** and exits it when the shell
goes away. Override with `HETERION_ROUTER_SIDECAR_PORT`. The launchd service
on **20128** is a separate thing and is untouched by the bundle.

## What the script verifies

- both binaries are present in `Contents/MacOS/` and executable;
- `Info.plist` parses (`plutil -lint`);
- `CFBundleExecutable` in the plist names the binary the bundle actually
  ships, and `CFBundleIdentifier` is `com.heterion.router`;
- the bundle verifies against its own signature
  (`codesign --verify --deep --strict`).

The script fails loudly with a message on `stderr` and a non-zero exit status.

## Known limitations

- **Ad-hoc signing, not notarized.** The bundle is signed with
  `codesign --force --deep --sign -`, the same posture the old Tauri bundle
  had. It runs on the machine that built it; it is not distributable to other
  Macs and Gatekeeper will complain on first launch elsewhere.
- **No auto-update.** Nothing updates the bundle in place. Rebuild and replace
  the `.app` by hand.
- **No installer.** No `.dmg`, no notarization ticket, no `pkg`. The deliverable
  is the `.app` directory.
- **`NSLocalNetworkUsageDescription` is deliberately absent.** The shell talks
  to a loopback gateway (`127.0.0.1`) and binds a loopback instance lock
  (`127.0.0.1:20130`); loopback traffic never triggers the macOS local network
  privacy prompt. Add the key only if the shell ever starts talking to a
  non-loopback address.
- **`LSMinimumSystemVersion` is 11.0.** gpui 0.2.2 does not state a floor of
  its own; 11.0 is the conservative pick for a Metal-based app.
