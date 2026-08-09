# tonepoet — Omnibus: gnudb HELLO + context-menu keyboard shortcut + vinyl-TRACKNUMBER publish collision — 2026-08-08

Three independent, long-languishing fixes in one delivery. Outcomes + guardrails; diagnosis is
evidence, not prescription — you are the arbiter of HOW. What binds you is the outcomes and the
guardrails/invariants.

## Ground rules
- Base = `main`/`hardening` @ `c4b0d4f` (the bundle is that tree). Version **0.4.6 — do not bump.**
  No merge, no unrelated refactors.
- Gate: `cargo test --workspace --no-fail-fast` green **×2** (the applier runs it; you have no
  cargo — say so honestly).
- **Byobu/tmux input rules (HARD):** no F-keys; nothing reachable *only* via a chord; and — see
  Item 2 — **do NOT add plain-letter bindings to the Browse pane** (plain letters are reserved for
  type-ahead; the user has repeatedly rejected vi-style `j`/`k`/`l`/`g` there). Actions use Alt+/
  Ctrl+ chords, arrows, Enter/Space, or right-click.

The three items are unrelated; do them all.

---

# Item 1 — gnudb "500 Unknown developer email for tonepoet 0.1"

**Problem.** MusicBrainz/gnudb TOC lookups fail: gnudb rejects the CDDB HELLO with
`500 Unknown developer email for tonepoet 0.1`.

**Diagnosis (evidence).** `src/tui/gnudb.rs:128`:
`const HELLO: &str = "tonepoet+localhost+tonepoet+0.1";` used at :140 and :164 as
`hello={HELLO}&proto=6`. The CDDB HELLO field is `username+hostname+clientname+clientversion`;
`tonepoet+localhost` doesn't reconstruct a valid developer email, and the version is a stale
hardcoded `0.1`. The response handler (`parse_query_response`/`parse_read_response`) surfaces the
first line of the 500 body verbatim but does not parse it.

**Outcome (O1).** The HELLO carries a valid-format contact email and the real app version, so a
well-formed lookup no longer 500s on the email/version. Use **`foo@foobar.com`** as the contact
(user's choice) and the crate version. Concretely, the intended HELLO is
`foo+foobar.com+tonepoet+0.4.6` — e.g.:
`const HELLO: &str = concat!("foo+foobar.com+tonepoet+", env!("CARGO_PKG_VERSION"));`
(so the version tracks Cargo.toml automatically).

**Guardrails / honest caveat.**
- gnudb may require a *registered* client; if so, a placeholder email can still 500. The code
  can't currently distinguish "unknown email" from "unregistered client" (it doesn't parse the
  500 body) — **do not** over-engineer 500-body parsing here; just fix the HELLO. If you can
  cheaply surface the server's message more clearly, fine, but it's optional.
- Don't change `GNUDB_BASE`, the proto, or the request flow.
**Test.** No existing tests exercise the HELLO string; add a tiny one asserting the HELLO
reconstructs `foo@foobar.com` and ends with the crate version (string-level), so it can't silently
regress to `localhost`/`0.1`.

---

# Item 2 — add a keyboard shortcut to open the Browse context menu

**Problem.** The Browse context menu opens only via **right-click**; there's no keyboard way to
open it.

**Diagnosis (evidence).** `open_context_menu_with_tx(app, x, y, tx)` (keybindings.rs:~10225,
public `open_context_menu` at ~10221) opens the menu at screen coords. The Browse key dispatch is
`handle_browse_key` (keybindings.rs:6524–7021). Row geometry is available: `last_render_area`
(the Browse list pane `Rect`), `selected_index`, `scroll_offset`, and the helper
`browse_entry_y_start(area, search_active) -> u16` (draw_browse.rs:~1612) which gives the first
entry row's Y. **Note:** `browse_entry_y_start` is currently **private** to `draw_browse.rs` — to
reuse it from `keybindings.rs` you must expose it (`pub(super)`/`pub`) or inline its one-line body
(`area.y + 2 + browse_search_rows(search_active)`); either is fine.

**Outcome (O2).** Pressing **Alt+M** while in the Browse pane opens the **same** context menu as
right-click, anchored at the currently-highlighted row (not at 0,0), and Esc / click-away / making
a selection closes it. Right-click behavior is unchanged.
- Anchor: `y = browse_entry_y_start(last_render_area, search.active) + (selected_index −
  scroll_offset)`, `x ≈ last_render_area.x + small inset`. If `last_render_area` is `None` (not yet
  rendered), fall back gracefully (a sane default position or no-op — do not panic).

**Guardrails.**
- **Alt+M only** — a modifier chord, verified unbound **in the Browse pane** (`handle_browse_key`,
  6524–7021, has no Alt+M). Alt+M *is* used elsewhere — the metadata-editor overlay binds it to
  "toggle maximize" (keybindings.rs:~15252) — but that's a **separate modal context**: overlays
  dispatch first (handle_key ~76–79) and only when the editor overlay is open, so there is **no
  collision** when Browse has focus. Confirm the Browse dispatch actually reaches `handle_browse_key`
  for Alt+M (no overlay open) and wire it there. Do **NOT** use a plain letter (`m` etc.) — that
  would shadow type-ahead, which is exactly what the user rejected. Not Ctrl+M (that's Enter), not
  Ctrl+Space/Alt+Space (multiplexer/IME conflicts), no F-keys.
- Scope: only add the keyboard opener + anchoring. Don't change the menu's contents, the
  right-click path, or other keys.
**Test.** Alt+M in Browse opens the context menu (state reflects an open menu); a plain letter does
NOT (still type-ahead); Esc closes it.

---

# Item 3 — vinyl-style TRACKNUMBER breaks single-file album batches (publish collision)

**Problem (real user defect).** Converting an independent single-file album whose tracks carry
**vinyl-style `TRACKNUMBER` (A1, A2, … B5** — PBThal convention), **1 of N tracks converts and the
rest hard-fail** with `Publish: destination already exists: <album dir>`. Field case: a Mazzy Star
FLAC rip with `A1..B5` tags. **User workaround today:** renumber to `1..N` first — we want it to
just work.

**Diagnosis (evidence — consensus of a prior full trace + a re-anchor at this HEAD).**
The independent single-file album batch scheduler can't prove numeric ordering for `A1..B5`, so it
**strips the album-batch membership**, turning each track into its own album that derives the
**same** album directory from the ALBUM tag; publish then collides.
- `prepare_album_batches_for_queued_independent_single_file_jobs` (src/convert/processor.rs:~301).
  Track-number resolution uses `strict_track_number_from_dispatch_path` (~1300–1320) which parses
  **leading digits only** → returns `None` for `A1`. The ambiguous-identity branch (~477–490) then
  calls `prepare_completion_order_album_batch` and `continue`s.
- The ambiguous branch first tries a **completion-order dispatch**
  (`prepare_independent_single_file_album_batch_for_completion_order_dispatch`, ~1247). If that
  **succeeds**, the batch stays together with `ordering = CompletionOrder` and incremental publish
  works — vinyl albums on the happy path already survive. **The collision is specifically the
  dispatch-*failure* path:** on `Err` (~1274), the code falls back to
  `mark_queued_album_batch_as_ordering_unavailable` (processor.rs:~1284–1297), which sets
  `request.album_batch = None`, `album_batch_track = None`, and
  `suppress_incremental_conversion_log_append = true` — nullifying membership and turning each track
  into its own colliding album. The "cannot prove canonical track ordering" warn is at ~1253–1256.
  So the real defect is: **an ordering-unprovable batch loses album membership when the
  completion-order dispatch validation errors**, not merely because ordering is unknown.
- Publish (src/convert/pipeline/stages.rs): `FailIfExists` at ~21927–21954 hard-fails with
  `DestinationExists` unless `is_incremental_single_audio_publish` (~22083–22126) returns true.
  That gate returns `false` early when `suppress_incremental_conversion_log_append` is set, and
  otherwise needs a log **fragment** (requires album_batch — absent) or a standalone
  conversion.log entry (absent). Note: the `album_batch_completion_order` escape at ~22108 *would*
  allow incremental append — but that field is never set because membership was nullified upstream.
- Album-batch membership is normally granted at stages.rs:~800
  (`req.album_batch = Some(album_batch.clone())`), never reached for these tracks.
- No vinyl/side-letter parsing exists; DSF/ID3 even reject non-numeric TRACKNUMBER
  (`PLAIN_UNSIGNED_ONLY`).

This is scheduler / publish-fragment-contract territory (processor.rs batch scheduling +
stages.rs publish/log-fragment), i.e. reasoning-model-authored T7-scheduler work.

**Outcomes.**
- **O3.1 (the contract).** Converting a single-file album with vinyl-style `A1..B5` TRACKNUMBER
  publishes **all N tracks into one shared album directory with no `destination already exists`
  collisions.** (Reproduce with N single-audio FLACs tagged `A1..B5`, one ALBUM, distinct titles.)
- **O3.2 (ordering, ideally).** Tracks are ordered sensibly — parse vinyl side-numbering
  (`A1` → side A, position 1 → stable ordinal; `B2` → side B, position 2 …) so the album is
  ordered A-side then B-side. If you don't do full side-number parsing, O3.1 must still hold via a
  structural completion-order batch (below).
- **O3.3 (the safety net — do this regardless).** An ordering-*unprovable* single-file album batch
  must still receive **album-batch membership** (completion-order) so siblings share the publish
  root and the incremental-append publish path applies — instead of nullifying membership +
  suppressing the incremental log. **Crucially, membership must survive even when the
  completion-order dispatch validation *fails*** (the `Err`/`mark_queued_album_batch_as_ordering_unavailable`
  path above): a validation error on a genuine same-album batch must not degrade to per-track
  singleton publishes that collide. Whether you make that dispatch not error for this case, retain
  `album_batch` defensively before/despite the error, or provide a structured fallback that keeps
  membership without full dispatch — the invariant is that same-album siblings never lose their
  shared publish root. This also heals the settings-mismatch path that uses
  `mark_queued_album_batch_as_ordering_unavailable`.

**Guardrails / invariants.**
- **INV-3a.** Do NOT regress numeric-`TRACKNUMBER` albums (they already work via fragments) or
  ordinary multi-file albums. Full workspace suite green ×2.
- **INV-3b.** Don't loosen publish safety generally — the shared-batch publish must still be the
  *same album*'s tracks (don't let unrelated albums collide/merge). Keep `FailIfExists` protective
  for genuinely-distinct destinations. (For reference: the batch grouping key at
  processor.rs:~334–360 already includes the normalized `album_output_dir` plus format/naming/
  lifecycle keys, so distinct albums land in distinct batches — retaining completion-order
  membership per O3.3 does **not** merge unrelated albums. Preserve that partitioning; don't widen
  the grouping key.)
- **INV-3c.** If side-number parsing is added, keep it to the scheduler/ordering (it's adjacent to
  the parked cross-format side-numbering backlog); do not change on-disk tag *writing* semantics.
**Tests.** (a) vinyl `A1..B5` single-file album → all N publish into one album dir, no collision;
(b) ordering reflects side-numbering if implemented; (c) regression: numeric single-file album
still works; (d) the settings-mismatch batch path no longer collides.

---

## Deliverables
- Patch or changed files; a short WHY per item; the tests above; honest note that you can't run
  cargo (the applier gates ×2). If you reject any diagnosis, say so and do what's correct — the
  outcomes are the contract.

## Bundle manifest
- This brief. Complete compiling `main`@`c4b0d4f` tree:
  - Item 1: `src/tui/gnudb.rs`.
  - Item 2: `src/tui/keybindings.rs`, `src/tui/browse.rs`, `src/tui/draw_browse.rs`
    (+ the context-menu/geometry code).
  - Item 3: `src/convert/processor.rs`, `src/convert/pipeline/stages.rs` (+ whatever the batch/
    publish/log-fragment path references).
  - Full `src/` + `crates/` + `tonepoet-pipeline/` + root `Cargo.toml` + `flake.nix` + `CLAUDE.md`
    so it compiles. NOT `target/`. If anything's missing, say so rather than guessing.
