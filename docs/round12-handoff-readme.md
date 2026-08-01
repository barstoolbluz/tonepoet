# Handoff — Round 12 (bundle readme)

## ⛔ READ FIRST — HARD SCOPE DISCIPLINE

A recent round was **rejected in full** for over-engineering (a few-line file-move fallback became
a protocol-v3 crash-recovery transaction system with ownership journals, adoption, fidelity proofs).
Do not repeat it. **tonepoet is a single-user desktop audio TUI** — no adversary, no concurrent
writers, no fleet, no hostile filesystem. For every item:

1. Implement exactly the scoped behaviour — nothing more. Fixes must be proportionate to the problem.
2. **No** new subsystems/protocols/journals/transaction managers/recovery layers unless the brief
   explicitly asks (it does not).
3. **No** defenses against threats this domain doesn't have (no ABA/inode-substitution, no
   adversarial-race hardening).
4. Do **not** harden arcane edges. Default path = fast + graceful degrade.
5. If you catch yourself building a "system," STOP. The smallest correct change, in the surrounding
   code's style, is the answer.
6. Stronger reasoner = SIMPLEST correct solution, not the most elaborate. If something seems
   under-hardened, flag it in one sentence for the user rather than building it.

**Governing document:** `docs/round12-brief.md`. Read it fully, including its "How to read this
brief" note (you are empowered to pick a better/simpler root cause; anchors are search aids).

**Baseline:** branch `hardening` == `main` @ `a6b8236`, version **0.4.5**. `cargo test --workspace`
inside `nix develop` = 56 targets, **5410 passed / 0 failed / 15 ignored**. No compiler on your side
— the applying side compile-fixes and gates. Return complete files (or a patch) + a short report.

## Scope — three items (details + anchors in the brief)

1. **COMMENT (and other image tags) dropped on the single-image + sidecar-CUE conversion path.**
   Empirically reproduced (source `.wv` has an APE COMMENT; output FLACs don't). Verified root cause:
   `read_image_album_metadata` (materializer_cue.rs:2784) uses two hard-coded allowlists
   (`cue_image_tag_field`, `cue_image_extra_key`) and drops anything else — COMMENT, COMPOSER,
   PUBLISHER, COPYRIGHT, ISRC, custom keys. The single-file materializer (materializer_single.rs:411)
   passes ALL text tags through — that's the inconsistency. Make the CUE path preserve image tags
   consistently; verify the value reaches the writer (`authoritative_metadata_tags`, stages.rs ~4665,
   which emits COMMENT from typed `meta.comment`).

2. **Autocomplete in the metadata overlay** for (a) custom field NAMES (DISCOGS_URL, LINEAGE) and
   (b) VALUES for artist/album-artist/genre/performer/country/composer. Extend the existing
   `CompletionMode` framework (text_input.rs:162) — do not build a new one. Tab is free in AddingKey
   (field names) but bound in InlineEdit (value completion needs a different affordance). Embedded
   sources exist for artist/album-artist (canonical_artists_reference.txt) and country
   (DictionaryLabelResolver); **genre/composer/performer have NO embedded list** — decide curated-list
   vs user-history.

3. **Configurable ordered priority for aggregate (directory/album) metadata targets**
   [individual-files, sidecar-cue, embedded-cue], resolved first-present-in-order on directory select.
   This GENERALIZES existing machinery — `TransferCarrier` (tag_interchange.rs:42) models the three
   targets, and `CueSidecarPolicy` + `classify_single_transfer_root` (keybindings.rs:15179) are the
   2-way policy to widen into a 3-way ordered one, refactored into a pure resolver reusable by a future
   Library. Config plumbing (config.rs pattern) DECOUPLED from UX. Single-image first-value collapse
   (materializer_archive.rs:1854). Explicit file-selection workflows MUST be preserved. Metadata-target
   priority and conversion-source selection are SEPARATE policies. **Do NOT build the Library.**

## Non-negotiable constraints

- NO F-keys ever; byobu-safe input (don't make a chord the only path); Ctrl+Q stays quit. No emoji /
  decorative unicode (▸/▾ pane indicators are the sanctioned exception).
- Do not regress rounds 5–11 (all on `main`, audited): metadata authority / DSP honesty / numbering /
  DSD Reference / fingerprint contracts / the round-11 items. New behaviour needs pins.
- Version stays **0.4.5**.

## Fences (do NOT fold in unasked)

- The Library itself (build only the reusable aggregate model).
- Config UX/presentation prettification (plumbing only this round).
- Custom tag builder + Paste tags (queued for later).
- Vinyl side-number parsing; pairing-guard relaxation (parked).

## Empirical fixtures (item 1 — do not modify)

Source: `~/livetorrents/Supertramp - Crime Of The Century (Japan AML-225) (1974)` (one `.wv` + sidecar
`.cue`; APE COMMENT present). Output: `~/temp/Supertramp - Crime of the Century (1974) [FLAC] {Japan
A&M AML-225 LP  32-192}` (COMMENT absent — the bug). Also note the separate deliberately-broken
acceptance fixture `~/livetorrents/Supertramp – Even In The Quietest Moments...` (invalid APE key
`&год`) — do not "repair" it.
