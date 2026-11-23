# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

An Alfred workflow ("Better Dictionaries") for searching macOS dictionaries. This fork replaces the upstream Python + Meilisearch backend with `bd`, a single native Rust binary in `bd/`. The workflow itself is only `bd`, four tiny shell scripts, `info.plist`, and the bundled `AlfredExtraPane.app` (live preview pane). Apple Silicon only.

The git repo is this directory (`alfred-better-dictionaries/`); its parent folder is not a repo.

## Commands

```bash
cd bd && cargo test --release --locked          # all tests (what CI runs)
cd bd && cargo test --release <name>            # single test, e.g. cargo test --release run_away
cd bd && cargo fmt --check && cargo clippy --release
cd bd && cargo deny check                       # advisories/licenses/sources (CI job; config in bd/deny.toml)
./build.sh                                      # release bd + "Better Dictionaries.arm64.alfredworkflow"
./upgrade.sh [workflow folder] [--no-migrate] [--keep-backup]   # patch the installed workflow in place
```

CI (`.github/workflows/build.yml`): Linux tests, `cargo-deny`, then a macOS build + smoke test of the packaged workflow; pushes to the default branch publish a GitHub release tagged with the `info.plist` version. Quit Alfred Preferences before `upgrade.sh`.

## Architecture

`bd` subcommands (dispatched in `bd/src/main.rs`): `import`, `list-unimported`, `migrate`, `setup`, `search`, `lookup`, `factory-reset`, `package`, `upgrade`, `say-ipa`, `version`. The shell scripts are thin wrappers (`lookup.sh` → `bd lookup`, `search.sh` → `bd search`, `sayipa.sh` → `bd say-ipa`), and also clear the Gatekeeper quarantine flag and run `setup.sh` once (marker files `.dequarantined`, `.setup-done`).

Data flow:
1. **Import** (`appledict.rs` parses Apple's `Body.data`; `item.rs` extracts title/forms/text from each definition) → per dictionary, `defs.pack` (compressed source definitions) plus `index.bin` (memory-mapped index; layout documented at the top of `index.rs`). No HTML is pre-rendered.
2. **Query** (`query.rs`) runs over the mmap'd index with tiered ranking (exact title → inflected form → title prefix → title words → typos → definition words). The tier table is in the module doc; ties are deterministic (shorter title, then dictionary order; in the typo tier, fewer edits come before title length).
3. **Lookup output** (`lookup.rs`) builds Alfred script-filter JSON, combining dictionary + thesaurus as one search (with `d:`/`t:` scope prefixes; on equal tier and title length, dictionaries rank before thesauri). Each shown result's HTML is rendered on demand into `cache/` (pruned after a week unused; a cached page whose size no longer matches its definition is rewritten). A definition whose `d:title` doesn't match the index entry (index and pack out of step) is shown as "Definition unavailable" and never cached.
4. **Workflow editing** (`graph.rs`) adds/removes per-dictionary keyword/hotkey objects in `info.plist` on import and factory reset. `pkg.rs` implements `package` (release-ready folder for `build.sh`) and `upgrade` (in-place patch of an installed workflow, including migration from the old Meilisearch layout).
5. First `lookup` auto-imports the built-in dictionary and thesaurus in the background (`bd setup`, guarded by `setup.lock`; failed retries back off 60 s × 2ⁿ up to 24 h, stored in `setup.tried`, but only once something is imported). Imports and reindexes hold `<data>/<id>.lock` and write `defs.pack`/`index.bin` to temp files, then rename them. Progress goes through `ui.rs` (osascript notifications, status file, log); `BD_QUIET=1` suppresses the banners. `say.rs` does IPA speech via Amazon Polly using the system `curl` (no TLS stack in `bd`).

Runtime paths come from `alfred_workflow_data` (data dir) and `BD_WORKFLOW_DIR` (workflow dir); outside Alfred set both, e.g. `alfred_workflow_data=/tmp/wfdata BD_WORKFLOW_DIR=<wfbuild or installed workflow> bd/target/release/bd lookup "word"`.

## Things to know

- `wfbuild/` and `AlfredExtraPane.app` are build products/gitignored; `build.sh` regenerates `wfbuild/` via `bd package` and patches the pane's hard-coded Alfred 4 path to Alfred 6 (then re-signs).
- Version lives in `bd/Cargo.toml` and `info.plist`, and is quoted in the README ("Rebuilt in Rust" section); bump all together. Release tag = `info.plist` version.
- Search output is meant to be deterministic; the README documents benchmarks and ranking behavior, so update it with ranking or storage changes.
- Keep dependencies lean and `cargo deny` clean; `bd` deliberately has no Python, no server process, and no bundled TLS.
