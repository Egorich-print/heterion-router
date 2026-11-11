# Third-Party Notices

Heterion Router is licensed under **GPL-3.0-or-later** (see `LICENSE`). This
file lists the third-party material that ships with the project and where its
license terms can be found.

## Rust dependencies

The gateway and the desktop shell are Rust crates. All dependency versions —
and therefore all dependency licenses — are pinned in the lockfiles:

- `Cargo.lock` — the workspace (`crates/heterion-router-*`);
- `desktop/gpui/Cargo.lock` — the GPUI desktop shell, which stands outside the
  workspace.

The dependencies are predominantly MIT, Apache-2.0 and BSD-licensed, which is
compatible with GPL-3.0-or-later; their per-crate licenses are recorded in the
lockfiles and on crates.io. No vendored copies of those sources are included in
this repository, so there is no separate bundle of third-party license texts to
ship here.

## Web dashboard

`desktop/ui` uses Svelte and Vite, both MIT-licensed (see `desktop/ui/package.json`
and the `node_modules` lockfile `desktop/ui/package-lock.json`).

## Frozen upstream JS material

The original JS implementation is **not** part of this repository. Its material
lives in the archived OmniRoute-Rust repository
(<https://github.com/Egorich-print/OmniRoute-Rust>), in the local archive
`/Volumes/Netac_SSD/ai-workstation/Archives/OmniRoute/`, and in the upstream
project <https://github.com/diegosouzapw/OmniRoute>. The upstream copyright
(`Copyright (c) 2026 diegosouzapw`) is preserved where that material was derived
from — see the notice in `LICENSE` and the frozen material under `docs/history/`.

The third-party native-transport notices (`wreq-js`) belonged to that frozen JS
tree and stay with the reference implementation.

Heterion Router grew out of that project, but the two are **not affiliated**: the
upstream authors neither maintain nor endorse this repository. Almost all of the
code here was written anew — the gateway is a new Rust implementation and the
macOS desktop app was written from scratch on GPUI; only the small Svelte
dashboard remains on JavaScript, and it was written during the port as well. What
the two share is the idea, some data formats and a few compatibility constants;
the upstream copyright above is preserved for the material that was derived from
it. See the "Происхождение проекта" section in `README.md`.
