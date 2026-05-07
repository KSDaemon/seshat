# Smoke Tests: Merge-aware Decisions and DB Freshness

Manual verification steps for the work that landed under
`feat/merge-aware-decisions` (epic 14.1, stories US-001 through US-016).

This is **not** a substitute for `cargo test --workspace` — that's the
authoritative regression suite. These steps verify the end-to-end user
experience against a real shell, real git history, and a real Claude
Code / MCP client.

## Pre-flight

```bash
# Build the binary fresh
cargo build --release

# Wipe any pre-existing DB to exercise the fresh-V11/V12 path.
# Replace <project> with the directory name of the project you scan.
rm -f ~/.local/share/seshat/repos/<project>.db

# Path the binary somewhere convenient
export PATH="$PWD/target/release:$PATH"
seshat --version
```

If you are validating against the actual `seshat` repo, use a SECOND
clone (e.g. `walt-chat-backend` or `seshat-merge-aware-decisions` itself)
as the project under test, not the repo holding the build.

## US-001: V11 + V12 migrations land cleanly

**What to verify:** the migrations run without error on a fresh DB, and
the new tables exist with the right columns.

```bash
# In an empty git repo:
mkdir -p /tmp/seshat-smoke && cd /tmp/seshat-smoke
git init -q && touch README.md && git add . && git commit -qm "init"

seshat scan
sqlite3 ~/.local/share/seshat/repos/seshat-smoke.db \
  "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name;"
```

**Expected:** the table list includes `branches` and `decisions`. Both
the `branches.branch_id` PK and `decisions.description_hash` PK indexes
appear under `sqlite_master WHERE type='index'`.

## US-002: DecisionRepository round-trip

**What to verify:** writes via the MCP `record_decision` tool land in
the new `decisions` table.

```bash
mcp call record_decision \
  --params '{"description":"Use anyhow for all errors","nature":"convention","weight":"strong"}' \
  -- seshat serve

sqlite3 ~/.local/share/seshat/repos/seshat-smoke.db \
  "SELECT description_hash, state, nature, weight FROM decisions;"
```

**Expected:** one row with `state='recorded'`, `nature='convention'`,
`weight='strong'`. No new row in `nodes`.

## US-003: BranchRepository registers branches explicitly

**What to verify:** `list_branches` reads from the registry, not from
`SELECT DISTINCT branch_id FROM nodes`.

```bash
sqlite3 ~/.local/share/seshat/repos/seshat-smoke.db "SELECT * FROM branches;"
```

**Expected:** at least one row with `branch_id='main'` (or whatever git
returned), `last_scanned_commit` populated, `last_scanned_at`
non-NULL.

## US-004: MCP record/update/remove operate on `decisions`

**What to verify:** mutation tools no longer create `nodes` rows.

```bash
HASH=$(sqlite3 ~/.local/share/seshat/repos/seshat-smoke.db \
  "SELECT description_hash FROM decisions LIMIT 1;")

mcp call update_decision \
  --params "{\"description_hash\":\"$HASH\",\"reason\":\"updated for smoke test\"}" \
  -- seshat serve

mcp call remove_decision \
  --params "{\"description_hash\":\"$HASH\"}" \
  -- seshat serve

sqlite3 ~/.local/share/seshat/repos/seshat-smoke.db \
  "SELECT COUNT(*) FROM decisions; SELECT COUNT(*) FROM nodes WHERE ext_data LIKE '%\"source\":\"user\"%';"
```

**Expected:** `decisions` count = 0; `nodes` user-source count = 0.

## US-005: TUI confirm/reject/partial write to `decisions`

**What to verify:** the review TUI's three actions all write decision
rows, not user nodes.

```bash
# Use a project with at least one auto-detected convention. seshat-merge-aware-decisions
# itself works.
cd /path/to/seshat-merge-aware-decisions
seshat scan
seshat review
# In the TUI, press "y" to approve, "n" to reject, "p" to mark partial.

sqlite3 ~/.local/share/seshat/repos/seshat-merge-aware-decisions.db \
  "SELECT state, COUNT(*) FROM decisions GROUP BY state;"
```

**Expected:** rows for each state you exercised. No `nodes` row with
`ext_data->>'source' = 'user'`.

## US-006 + US-007: review query and counter use the new join

**What to verify:** the TUI does not re-show conventions you decided
on, even after a fresh scan.

```bash
seshat scan        # rescan
seshat review      # reopen the TUI
```

**Expected:** decided conventions are NOT in the queue. The TUI header's
"approved" counter shows the project-wide approved count
(`SELECT COUNT(*) FROM decisions WHERE state='approved'`), which you
can verify against the SQLite query.

## US-008: persist_conventions skips decided hashes during rescan

**What to verify:** rescan does not re-insert auto-detected nodes for
descriptions that already have a decision row.

```bash
sqlite3 ~/.local/share/seshat/repos/seshat-merge-aware-decisions.db \
  "SELECT n.description FROM nodes n JOIN decisions d ON d.description_hash = n.description_hash;"
```

**Expected:** zero rows. If non-zero, persist_conventions failed to
filter on `description_hash IN (decisions.description_hash)`.

## US-009: scan paths write `last_scanned_commit`

**What to verify:** every scan path updates the sentinel.

```bash
git -C /path/to/project rev-parse HEAD
sqlite3 ~/.local/share/seshat/repos/<project>.db \
  "SELECT branch_id, last_scanned_commit, last_scanned_at FROM branches;"
```

**Expected:** `last_scanned_commit` matches `git rev-parse HEAD` for
the active branch. `last_scanned_at` is within the last few seconds.

## US-010: `seshat serve` detects same-branch HEAD movement

**What to verify:** a `git pull` (or any HEAD-mover that keeps the
branch label) triggers a background sync on next serve startup.

```bash
# Capture the OLD head
OLD=$(git -C /path/to/project rev-parse HEAD)
# Move HEAD without changing the branch label (e.g. an empty commit)
git -C /path/to/project commit --allow-empty -m "smoke: head move"
NEW=$(git -C /path/to/project rev-parse HEAD)

# Restart serve and watch stderr
seshat serve /path/to/project 2>&1 | grep -E "old_head|new_head"
```

**Expected:** stderr includes `old_head=<7-char of $OLD>` and
`new_head=<7-char of $NEW>`. After ~5–30 s,
`SELECT last_scanned_commit FROM branches WHERE branch_id='<branch>'`
matches the new HEAD.

## US-011: `seshat review` blocks on stale DB before opening TUI

**What to verify:** with a stale `last_scanned_commit`, review prints a
sync header + progress line and does not open the TUI until sync is
done.

```bash
# Force the sentinel to a bogus old commit
sqlite3 ~/.local/share/seshat/repos/<project>.db \
  "UPDATE branches SET last_scanned_commit = 'deadbeefdeadbeef' WHERE branch_id = 'main';"

seshat review /path/to/project
```

**Expected:** stderr/stdout shows
`Syncing project state to <head[..7]>...` followed by
`Files: X / Y` updates. The TUI opens only after the sync completes.

## US-012: non-git directory still works

**What to verify:** running every Seshat command in a directory without
`.git` succeeds, decisions persist, and stdout/stderr stay quiet.

```bash
mkdir -p /tmp/seshat-nogit && cd /tmp/seshat-nogit
echo "hello" > README.md      # NO git init

seshat scan
seshat serve /tmp/seshat-nogit &  # in background or another shell
mcp call record_decision \
  --params '{"description":"non-git smoke","nature":"decision","weight":"rule"}' \
  -- seshat serve

sqlite3 ~/.local/share/seshat/repos/seshat-nogit.db \
  "SELECT branch_id, last_scanned_commit FROM branches;"
sqlite3 ~/.local/share/seshat/repos/seshat-nogit.db \
  "SELECT description, decided_on_branch FROM decisions;"
```

**Expected:** `branches.last_scanned_commit IS NULL`,
`branches.branch_id = 'main'`, the decision row exists with
`decided_on_branch = 'main'`. No warnings or errors on stderr.

## US-013: `seshat decisions list`

**What to verify:** the table format and the JSON format render
correctly, the filters narrow as documented.

```bash
seshat decisions list
seshat decisions list --format json
seshat decisions list --state approved
seshat decisions list --branch main --format json | jq length
```

**Expected:** the table has columns
`state | hash | description | decided_on_branch | decided_at`. The
JSON output is a valid array — `jq length` returns a number. State and
branch filters narrow the set.

## US-014: `seshat decisions forget`

**What to verify:** the prompt-and-confirm path works, the `--yes`
escape hatch works, prefix lookup with ≥ 4 chars works, < 4 chars is
rejected.

```bash
HASH=$(seshat decisions list --format json | jq -r '.[0].description_hash')
PREFIX=${HASH:0:6}

# Interactive: type "y" + ENTER
seshat decisions forget "$PREFIX"

# Or non-interactive
seshat decisions forget "$HASH" --yes

# Now rescan and verify the convention re-emits
seshat scan
seshat review        # the convention you forgot is back in the queue
```

**Expected:** the matched decision is printed before the prompt; "y"
deletes, anything else (including empty) preserves. After deletion, a
fresh scan re-emits the auto-detected node and the TUI surfaces it.

## US-015: `seshat decisions export` and `import`

**What to verify:** export → import round-trip is lossless. `--strict`
fails (no writes) on conflict.

```bash
# Export
seshat decisions export /tmp/decisions.json
jq length /tmp/decisions.json

# Wipe and reimport
sqlite3 ~/.local/share/seshat/repos/<project>.db "DELETE FROM decisions;"
seshat decisions import /tmp/decisions.json
seshat decisions list --format json | jq length
diff <(jq -S . /tmp/decisions.json) <(seshat decisions list --format json | jq -S .)

# Strict-mode conflict
seshat decisions import /tmp/decisions.json --strict   # should fail with hash list
```

**Expected:** the round-trip diff is empty. Strict-mode reimport into
an already-populated DB fails before any write, listing the conflicting
hashes.

## US-016: cross-branch decisions persist after merge

**What to verify:** approving on `feature` and merging into `main`
does not re-emit the convention on `main`.

```bash
cd /tmp/seshat-merge && git init -q
# ... seed some files that produce a convention ...
git checkout -b feature
seshat scan
seshat review            # approve the convention
git checkout main
git merge feature        # fast-forward
seshat scan
seshat review            # the approved convention is NOT in the queue
```

**Expected:** the convention does NOT appear in the review queue on
`main` after the merge. Verifies G1 + G5.

---

## Quick "everything still works" sanity check

```bash
cargo test --workspace --release
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

All three must be green. If they are, the smoke tests above are mostly
exercising user-visible polish (output, prompts, log messages) and
serve as a regression check before tagging a release.

## When a smoke test fails

1. Capture stderr (`seshat <cmd> 2> /tmp/stderr.log`) and the relevant
   `branches` / `decisions` rows.
2. Compare against the AC in `.ralph/tasks/prd-merge-aware-decisions.md`.
3. Check the "Failure-mode checklist" at the bottom of the PRD — the
   common foot-guns (sentinel write order, chunked-IN parameter limits,
   git-unavailable silence) are listed there.
4. File against epic 14.1 with the failing AC and the captured output.
