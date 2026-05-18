---
date: 2026-05-17
type: implementation-spec
size: small (~1h)
scope: Expand glob patterns in [workspace.members] when extracting Rust crate names
parent_prd: prd-rust-python-manifest-parsing-2026-05-04.md
roadmap_tag: "#fw1-glob"
languages: Rust
---

# Spec: Glob Workspace Members Expansion (FW-1)

**Author:** Kostik (drafted by Claude)
**Date:** 2026-05-17
**Status:** Ready for implementation

---

## Problem

`crates/seshat-scanner/src/manifest.rs:197` `extract_crate_names()` parses
`[workspace.members]` entries from `Cargo.toml` and feeds them into
`workspace_crates` (the list of internal crate names used by
`is_likely_internal` / `resolve_internal_crate_import` in
`crates/seshat-graph/src/dependencies.rs`).

Today, glob patterns are **deliberately skipped** via `is_glob_pattern()`
(`manifest.rs:254`). Comment at `:194` says: "Glob patterns (e.g. `crates/*`)
are skipped — they cannot be resolved at manifest-parse time without
filesystem access. The scan orchestrator handles glob expansion separately."
The scan orchestrator does **not**, in fact, handle this — verified by the
test `extract_crate_names_workspace_members_with_glob_skipped` at `:1050`
which asserts the glob form yields no extra crates.

Net effect: any Rust workspace using the idiomatic `members = ["crates/*"]`
pattern ends up with an **empty or under-populated** `workspace_crates`
list. `query_dependencies`, `query_code_pattern` blast_radius,
`map_diff_impact`, wrapper-facade detection, and the `validate_approach`
duplicate-check all silently miss internal-to-internal edges.

## Goal

`extract_crate_names()` resolves glob members by walking the filesystem
relative to the manifest's directory, reading each matched directory's
`Cargo.toml` (via the existing `read_inner_crate_name()` helper), and
appending the authoritative crate names to the returned `Vec<String>`.

Non-glob members continue to behave exactly as today.

## Non-Goals

- Recursive glob (`crates/**`) — not standard Cargo semantics; out of scope.
- `[workspace.exclude]` parsing — separate concern, not blocking FW-1.
- JS/TS / Python equivalents — covered by `spec-jsts-monorepo-detection.md`
  and FW-2 / FW-4 respectively.

## Design

### Code locations

| File | Symbol | Change |
|---|---|---|
| `crates/seshat-scanner/src/manifest.rs:197` | `extract_crate_names` | replace the `continue` skip-glob branch with `expand_glob_member()` |
| `crates/seshat-scanner/src/manifest.rs` | new `fn expand_glob_member(manifest_dir: &Path, pattern: &str) -> Vec<PathBuf>` | uses the `glob` crate (already a transitive dep — confirm in `Cargo.toml` before adding; otherwise pull from workspace) |
| `crates/seshat-scanner/src/manifest.rs:1050` | test `extract_crate_names_workspace_members_with_glob_skipped` | rename + invert assertion: glob now resolves |

### Glob semantics (must match Cargo)

Cargo resolves `[workspace.members]` globs via the `glob` crate with default
options (`MatchOptions::default()`). FW-1 mirrors that:

- `crates/*` → every direct subdirectory of `<manifest_dir>/crates/`
- Each matched path is treated as a directory; if it does not contain
  `Cargo.toml`, it is silently skipped (Cargo does the same)
- Only one level deep for `*` (no recursion into nested workspaces)
- `?` and character classes (`[abc]`) honoured by `glob` already
- Patterns are joined with `manifest_dir` before expansion so relative paths
  work the same as the non-glob branch
- Absolute patterns (e.g. `members = ["/etc/*"]`) are rejected up front —
  `Path::join` keeps absolute paths verbatim, so without a guard a glob
  would escape `manifest_dir`
- Windows: `Path::join` produces `\` separators which `glob` doesn't match;
  the joined string is normalised to `/` on Windows before expansion
- Per-entry I/O errors from the `glob` iterator (permission denied, transient
  FS) are logged at `warn` and the entry is skipped rather than silently
  dropped

### Missing-`Cargo.toml` policy (unified)

Both glob-expanded and literal members are treated the same: if the inner
`Cargo.toml` cannot be read, the member is silently skipped. This diverges
from Cargo itself (which errors on missing literal members), but Seshat
runs against in-progress trees where half-applied changes are routine —
staying quiet here keeps `workspace_crates` free of fake names synthesised
from directory basenames, which would otherwise pollute downstream
`is_likely_internal` / `query_dependencies` results.

A literal member with no inner `Cargo.toml` therefore produces **no** crate
name (no basename fallback). Existing tests that passed a synthetic
`Path::new("Cargo.toml")` + in-memory content must move to `tempdir()`
fixtures with real inner manifests — the new glob tests already follow that
pattern.

### Pseudocode

```rust
fn expand_glob_member(manifest_dir: &Path, pattern: &str) -> Vec<PathBuf> {
    if Path::new(pattern).is_absolute() {
        tracing::warn!(pattern = %pattern, "absolute workspace member glob; skipping");
        return Vec::new();
    }
    let joined = manifest_dir.join(pattern);
    let Some(pattern_str) = joined.to_str() else {
        tracing::warn!(pattern = %pattern, "non-UTF8 workspace member glob; skipping");
        return Vec::new();
    };

    #[cfg(windows)]
    let pattern_owned = pattern_str.replace('\\', "/");
    #[cfg(windows)]
    let pattern_str: &str = pattern_owned.as_str();

    let iter = match glob::glob(pattern_str) {
        Ok(it) => it,
        Err(e) => {
            tracing::warn!(pattern = %pattern_str, error = %e, "invalid workspace member glob");
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for entry in iter {
        match entry {
            Ok(p) if p.is_dir() => out.push(p),
            Ok(_) => {}
            Err(e) => tracing::warn!(pattern = %pattern_str, error = %e, "glob entry error"),
        }
    }
    out
}
```

Then in `extract_crate_names()` — note the **unified** handling: no basename
fallback for either branch, both silently skip when the inner `Cargo.toml`
is unreadable.

```rust
for member in &ws.members {
    let dirs = if is_glob_pattern(member) {
        expand_glob_member(manifest_dir, member)
    } else {
        vec![manifest_dir.join(member)]
    };
    for dir in dirs {
        let Some(crate_name) = read_inner_crate_name(&dir.join("Cargo.toml"))
        else {
            continue; // glob OR literal: no inner manifest → skip
        };
        if !crate_name.is_empty() {
            names.push(crate_name.replace('-', "_"));
        }
    }
}
```

### Dependency

`glob = "0.3"` (or current pin) — add to `[workspace.dependencies]` in
root `Cargo.toml` if not already present, then to
`crates/seshat-scanner/Cargo.toml` as `glob.workspace = true`.

## Acceptance Criteria

- [ ] `extract_crate_names()` returns crate names for `members = ["crates/*"]`
  given a directory tree with `crates/foo/Cargo.toml` (`[package].name = "foo"`)
  and `crates/bar/Cargo.toml` (`[package].name = "bar"`). Order does not
  matter; deduplication via the existing `Vec` reorder-then-dedup pass is fine.
- [ ] Renamed test `extract_crate_names_workspace_members_with_glob_expanded`
  (was `_skipped`) asserts both `foo` and `bar` end up in the returned `Vec`.
- [ ] New unit test: glob with one matched dir missing `Cargo.toml` (e.g.
  `crates/empty/.gitkeep`) — empty dir is silently skipped, other matches
  still resolve.
- [ ] New unit test: invalid glob pattern (e.g. `crates/[`) does not panic
  and returns the rest of the manifest's crate names unaffected. (Warn-level
  logging is emitted by `expand_glob_member` but not asserted — keeping the
  scanner free of a `tracing-test` dep for one log line.)
- [ ] New unit test: non-glob member alongside a glob (`members = ["legacy-crate", "crates/*"]`)
  produces both `legacy_crate` and the globbed names; assertions check
  independent presence (not a sort-then-eq) so a single broken branch
  cannot be masked.
- [ ] New unit test: literal member whose directory lacks `Cargo.toml` is
  silently skipped (unification AC — no basename fallback).
- [ ] New unit test: `expand_glob_member` happy path resolves subdirs.
- [ ] New unit test: `expand_glob_member` filters non-directory entries.
- [ ] New unit test: `expand_glob_member` returns empty for a valid pattern
  with zero matches (distinct from the invalid-pattern case, which is
  covered separately).
- [ ] New unit test: `expand_glob_member` rejects absolute patterns
  (e.g. `/etc/*`) without escaping `manifest_dir`.
- [ ] Manual: run `seshat scan` over a real Rust workspace with
  `members = ["crates/*"]`, then `seshat status` shows non-zero
  `workspace_crates` count; `query_dependencies` on a file inside one
  globbed crate shows internal dependents from another globbed crate.
- [ ] `cargo test -p seshat-scanner` passes.
- [ ] `cargo clippy --workspace -- -D warnings` clean.
- [ ] `cargo fmt --check` clean.

## Implementation Order (suggested)

1. Confirm `glob` crate availability (`cargo tree | grep '^glob'` or check
   root `Cargo.toml`). Add if missing.
2. Write `expand_glob_member()` + its three unit tests (happy path, missing
   inner `Cargo.toml`, invalid pattern). TDD red → green.
3. Wire into `extract_crate_names()`; flip the existing
   `_with_glob_skipped` test (rename + invert).
4. Run `cargo test -p seshat-scanner -p seshat-graph` — graph tests at
   `dependencies.rs:1894` seed `workspace_crates` manually and should still
   pass unchanged.
5. Smoke test against the seshat repo itself (uses `members = ["crates/*"]`
   in its root `Cargo.toml`): `cargo run --bin seshat -- scan .`, verify
   the persisted `workspace_crates` matches the actual crate list.

## Risks

- **Performance:** glob expansion does I/O. For a workspace with 50+ crates
  this is still <50ms. Not a regression concern.
- **Path normalisation:** `dir.file_name()` returns the last path
  component, which for `crates/seshat-graph` is `seshat-graph` — same as
  the existing `rsplit('/').next()` path. `read_inner_crate_name()` is the
  preferred source either way; the basename is only a fallback.
- **Symlinks:** `glob` follows symlinks by default. If the workspace uses
  symlinked crate dirs, they resolve correctly. No special handling needed.

## Downstream Beneficiaries

After FW-1, the following start working correctly on glob workspaces:

- `query_dependencies` — internal dependents across globbed crates
- `query_code_pattern` `dependent_files` / `blast_radius`
- `map_diff_impact` — affected symbols across globbed crates
- Wrapper-facade detection (one internal crate wrapping another)
- `validate_approach` duplicate-check across globbed crates

The Seshat repo itself currently relies on the per-crate `[package].name`
fallback in `[workspace.dependencies]` declarations to keep things working
for the dogfooding scenario. Post-FW-1, the workspace-members path is the
primary, and the dependency-declarations path becomes a redundant backstop
(no behaviour change, but cleaner).
