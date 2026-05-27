# validate_approach Relevance Gate — Why We Did Not Ship IDF

**Date:** 2026-05-27
**Author:** Seshat team (post-mortem session)
**Purpose:** Persist the empirical evidence behind rejecting an IDF-weighted relevance gate in `validate_approach`, so we do not waste effort re-considering it the next time phantom rules surface. Anchor for any future replacement of the current hardcoded-stop-words approach.

---

## Executive Summary

Long technical descriptions kept triggering unrelated `rules_violated` verdicts because the fixed-count token-overlap gate (`MIN_RULE_RELEVANCE_TOKENS = 2`) cannot tell incidental common-prose overlap from genuine same-domain overlap. We considered an IDF-weighted score gate as a more principled fix, **implemented it end-to-end**, and rejected it after empirical verification: on a project-internal corpus of rule + decision + convention descriptions, IDF degenerates and offers no separating threshold. We shipped a hardcoded "code-prose stop-words" extension instead. It is provably effective on the production reproducer, costs nothing at runtime, and is trivially extensible.

---

## The Phantom

Reproducer captured 2026-05-27 from `~/.seshat/call-log.jsonl`:

> Prose (~600 chars): _"In crates/seshat-graph/src/validate_approach.rs:120-160, the find_duplicates function passes the full description string to query_code_pattern without significant token filtering. ... Fix: route the description through extract_identifier_candidates first ..."_
>
> Rule: _"DB schema migrations and dropped read sites MUST be marked breaking in the commit message itself, not just in CHANGELOG.md. ..."_

After current-as-of-v0.5.0 stop-words filtering, the two share `{description, fix, without}` — 3 significant tokens. With `MIN_RULE_RELEVANCE_TOKENS = 2`, the rule fires and flips the verdict to `rules_violated` / `ready: false`. The user sees a red light on a completely unrelated change.

Any sufficiently long technical description has this failure mode: code-prose words like `description`, `fix`, `function`, `without`, `value`, `return` will incidentally collide with **some** rule, somewhere, somehow.

---

## Approaches Considered

| # | Approach | Status |
|---|---|---|
| A | IDF-weighted score gate, corpus = current project's rules + decisions + conventions, computed on-the-fly | Implemented, rejected (see below) |
| B | IDF-weighted gate, corpus = all docstrings/comments across the codebase | Not implemented; corpus-build cost not obviously worth it given Variant A failure mode |
| C | IDF using a fixed pre-trained corpus (Wikipedia / GitHub English) | Not implemented; loses project-specificity, ships a big static asset |
| D | Bigram requirement — overlap must include ≥1 shared 2-gram of significant tokens | Not implemented; short rules like "Use rusqlite for DB" don't have bigrams, would over-block |
| E | Embedding-cosine similarity behind the `builtin-embedding` feature flag | Not implemented; latency cost on every call, requires opt-in build, overkill for a gate |
| F | Hardcoded "code-prose stop-words" extension to `STOP_WORDS` | **Shipped** in PR #47 (`fix(validate): extend STOP_WORDS with code-prose filler ...`) |

---

## Why IDF Variant A Failed Empirically

IDF assigns weight `idf(t) = ln(N / df(t))` where `N` is the corpus size and `df(t)` is the number of corpus docs containing token `t`. The intuition behind IDF: words that appear in many documents are uninformative (and get downweighted); words that appear in few documents are informative.

IDF was designed for retrieval from long English documents where common connectives appear in most documents. **Our corpus has the opposite shape.**

### Measured df on the actual seshat corpus

`N = 547` (16 decisions + ~531 auto-detected + user-recorded convention descriptions on the `main` branch).

| Token | df | df / N | idf (nats) |
|---|---:|---:|---:|
| `description` | 1 | 0.2% | **6.30** |
| `fix` | 1 | 0.2% | **6.30** |
| `without` | 4 | 0.7% | **4.92** |
| `breaking` | 1 | 0.2% | 6.30 |
| `rusqlite` | 1 | 0.2% | 6.30 |
| `idempotent` | 0 | 0% | 6.31 (max) |
| `schema` | 2 | 0.4% | 5.61 |
| `migration` | 0 | 0% | 6.31 |
| `file` | 37 | 6.8% | 2.69 |
| `seshat` | 99 | 18.1% | 1.71 |

Note `description`/`fix` have the **same** IDF weight (~6.3) as `rusqlite`/`idempotent`. IDF cannot distinguish noise tokens from domain signal on this corpus because **every token is rare** in the IDF sense — descriptions are short and on-point, so vocabulary is naturally sparse.

The maximum-df tokens (`seshat`, `file`) are themselves not noise — they ARE informative on this project (`seshat` is the project name; `file` is a structural concept). Downweighting them would lose signal, not noise.

### Score arithmetic on the reproducer

| Case | Overlap tokens | IDF sum (nats) | Verdict at threshold = 3.0 |
|---|---|---:|---|
| **Phantom**: long find_duplicates prose vs breaking-changes rule | `{description, fix, without}` | 6.30 + 6.30 + 4.92 = **17.52** | fires (BAD) |
| **Genuine**: rusqlite Connection / prepared statements / idempotent description vs a domain rule | `{rusqlite, idempotent}` (after stop-words) | 6.30 + 6.31 = **12.61** | fires (good) |

Phantom scores **higher** than genuine because the phantom has three rare tokens and the genuine has only two. No threshold between 12.61 and 17.52 would help: any value high enough to block the phantom would also block the genuine.

### Why the unit tests passed anyway

The IDF prototype's reproducer test seeded a **synthetic** 4-doc corpus where the noise words appeared in every doc:

```rust
let (df, n) = make_corpus_df(&[
    "description fix without consequence",
    "description fix without warning",
    "description fix without notes",
    "description fix without doc",
]);
// description: df=4, fix=4, without=4  →  idf = ln(4/4) = 0
```

That gives `idf = 0` per token, score = 0, gate blocks. The test passed for the right reason — but only because the corpus was engineered to make IDF work. The real corpus doesn't have that shape.

### What it would have cost in production

`load_corpus_df` runs two SQL SELECTs against `decisions` and `nodes` per `validate_approach` call (no caching across calls). On the seshat corpus this is ~5-30 ms, plus ~500 small allocations for tokenisation. On the seshat baseline of ~49 ms per call that's +12–60% latency for a gate that **provably does not separate phantom from signal** on this corpus.

---

## Why Hardcoded Stop-Words Won

The actual root cause is that words like `description`, `fix`, `function`, `value` are generic noise in **any** technical writing about code — independent of corpus distribution. The right way to remove them is to acknowledge that fact and drop them up-front, before any gate runs.

PR #47 extends `STOP_WORDS` with ~80 such words organized into a clearly labelled "code-prose filler" section. After the extension:

| Case | Overlap tokens | Verdict |
|---|---|---|
| Phantom: long find_duplicates prose vs breaking-changes rule | `∅` (size 0) | gate stays closed ✅ |
| Genuine: rusqlite / Connection / prepared / statements / idempotent vs domain rule | `{rusqlite, connection, prepared, statements, idempotent}` (size 5) | gate fires ✅ |

The empirical verification was done against the actual `seshat.db` 547-doc corpus, not a synthetic test corpus. The whole change is data — no new compute path, no migration, no runtime cost.

The same `STOP_WORDS` const is also consumed by `conventions::search_decisions_by_topic`, so the extension narrows keyword-based decision search too — fewer false-positive matches across both code paths.

---

## When to Revisit

The hardcoded approach has a known weakness: if a rule deliberately uses a word that we've added to the stop-list (e.g. a rule about "function naming" matching against an approach that mentions "function"), the rule will under-match. Practical mitigation: such rules almost always also carry stronger markers (e.g. `snake_case`, `camelCase`, `naming`) which survive filtering.

Re-open the conversation if:

1. **Field reports of false negatives** — real rules that should fire but don't, where the missed overlap is on a word currently in the code-prose stop-list. Promote those specific tokens back out of the list (per-word, not wholesale).
2. **Corpus shape changes dramatically** — e.g. if decisions become much longer (paragraphs instead of single sentences) and the df distribution shifts, IDF Variant A could become viable. Re-measure df distribution before re-considering.
3. **An order-of-magnitude better signal becomes cheap** — e.g. cheap on-device embeddings with sub-millisecond cosine similarity. At that point, the right replacement is Variant E, not IDF.

Do **not** revisit IDF Variant A on a corpus of similar shape (≤ a few hundred short technical descriptions). The math will not work for the same reason it did not work this time.

The rejected implementation lives at `origin/feat/idf-relevance-gate` (unmerged) for anyone who wants to inspect the code, the IDF helper functions, or the synthetic-corpus tests.

---

## Pointers

- Production reproducer & verification numbers: this document (above)
- Hardcoded stop-words extension: PR #47, commit `e4ce047`
- Rejected IDF implementation: branch `feat/idf-relevance-gate`, head `fc3b338`
- Earlier 2-token gate (still in place as a floor): `MIN_RULE_RELEVANCE_TOKENS` in `crates/seshat-graph/src/validate_approach.rs`
- Shared filter in decision-side keyword search: `crate::validate_approach::STOP_WORDS` referenced from `crates/seshat-graph/src/conventions.rs::search_decisions_by_topic`
