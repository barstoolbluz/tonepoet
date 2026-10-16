# TonePoet R20C — RF64 reader-integration corrective handoff

Date: 2026-10-15 (America/New_York). Corrects the two real integration omissions identified in the review of R20. This is a **complete source snapshot** layered on top of the supplied R20 full-repo corrective (`TonePoet_R20_RF64_HDCD_CORRECTIVE_FULL_REPO_2026-10-15.tar.gz`, SHA-256 `fdf84b328b3820b4197df3f22f327a0f8520748476969bdbb4dce7a85541655b`). The original upstream user source was `TonePoet_LOGGING_R20_hdcd_dither_rf64_2026-10-15.tar.gz`, SHA-256 `bdc4d0947119a7bcf572422b03b65795aafdfcb7a2f4c6ec45e882565cc7155e`.

## Correction implemented

**No audio engine, HDCD production logic, existing RF64 adapter behavior, metadata authority rules, or cache architecture was changed.** The bounded, read-only adapter from R20 is reused as-is. Normal Lofty reading remains the first operation; a failure retries only if the file has real `RF64` magic. The recovery-before-metadata gate in track ordering, its filename fallback, and Browse's successful-empty versus failed-read distinction remain in place.

- `src/tui/probe.rs`: Convert's `read_convert_preemphasis_tag_evidence` now retries genuine RF64 and feeds tags through the *unchanged* `convert_preemphasis_evidence_from_tags` authority logic. `sort_paths_by_track` retries RF64 after failed Lofty reads only inside the existing successful-recovery branch.
- `src/tui/preemphasis/metadata.rs`: Both `check_pre_flag_tag_disposition` and the COMMENT-aware `check_tag_evidence` can read genuine RF64 using the same retry rule. No change to CUE precedence, negative-veto semantics, eligibility, or spectral detection.
- `src/tui/browse.rs`: `read_tags_from_file_checked` and `extract_tag_sort_key` now retry genuine RF64. Success with no tags remains `Ok(empty)`; failed reads remain `Err` and do not populate the tag-search cache. Search and sorting do not acquire new reads on cursor movement.
- `src/tui/rf64_metadata.rs`: **Test-only** builder for matching 16-bit/44.1-kHz RIFF/RF64 WAVs with real ID3 metadata (including custom `PRE_EMPHASIS`, track numbers, and standard title/artist/album/year). No production adapter changes.
- `docs/OUTSTANDING_ISSUES.md`: Accurate pending-validation note for #62; no other issue status edited.

## Added regression cases (must run on the qualified host)

1. `rf64_convert_preemphasis_matches_riff_yes_no_and_tagless` — affirmative YES, categorical NO, tagless, and negative CUE veto; check identical RIFF/RF64 behavior.
2. `rf64_preemphasis_advisories_match_riff_for_yes_no_and_tagless` — explicit and broader metadata advisory entrypoints; positive/negative dispositions and no-tag control.
3. `rf64_browse_tag_only_search_and_tag_sort_use_real_metadata` — mixed RIFF/RF64 directory; a title not in the filename is found by tag-only Browse search; metadata title sorting reverses filename ordering; artist, album and year sort keys come from tags.
4. `rf64_browse_preserves_successful_empty_tags_vs_read_failure` — tagless valid RF64 is an empty success; malformed RF64 is an error and does not create a negative tag-search cache entry.
5. `sort_paths_by_track_honors_rf64_tracknumber_over_misleading_filename` — mixed RIFF/RF64; `A5 Song.wav` tagged TRACKNUMBER=5 follows tracks 1 and 2. A repeated sort remains stable.

All R20 RF64 adapter/artwork/boundedness and HDCD tests remain intact. The existing `scripts/verify_r20.sh` command includes the new `rf64_`-named tests as well as the previously required full workspace gate.

## Evidence collected in this environment

- The base R20 `SHA256SUMS` inventory was independently verified before editing (**PASS**).
- `git diff --check` and six source read-site order/gating checks (**PASS**).
- Independent on-disk 16-bit/44.1-kHz ID3 WAV smoke fixtures: FFprobe reads equivalent title and `PRE_EMPHASIS=YES` / `NO` from RIFF and RF64 (**PASS**). This uses independent Python/mutagen construction; it is **not evidence of Rust/Lofty test success**.
- Rust toolchain (`cargo`, `rustc`) and `nix` are absent; network DNS is unavailable here. **No Rust compilation, Rust tests, 7,501-test workspace baseline comparison, UI reproduction, or real RF64 album run was performed. Do not claim acceptance or close #62.**

## Qualified build-host instructions

In the pinned dev shell, at the root of the unpacked repository:

```bash
sha256sum -c SHA256SUMS
cargo test -p tonepoet rf64_ -- --nocapture
bash scripts/verify_r20.sh
```

The script repeats the RF64-focused tests and HDCD ownership regressions and then runs `cargo test --workspace --no-fail-fast`. Exercise the real issue-#62 RF64 file: tags/artwork visible; Browse tag-only search and tag sorts use metadata; conversion naming uses its title, without the spurious metadata-loss warning. Exercise a *valid 16/44.1 RF64 with PRE_EMPHASIS=YES and NO* in Convert to confirm the affirmative advisory and negative-veto behavior. Finally reproduce the original in-field HDCD ON -> 16-bit -> OFF sequence in the UI; the earlier failure was not demonstrated by R20 source tracing, and **R20C does not alter HDCD state management**.

**Important test diagnostic, not a new feature request:** The real ID3v2 `TXXX:PRE_EMPHASIS` fixtures intentionally test whether the pinned Lofty version exposes that custom key to the unchanged Convert authority logic. FFprobe recognition alone cannot prove it. If the *ordinary RIFF control also fails* its YES/NO assertion, first determine whether Lofty 0.21 retains the custom TXXX item in `TaggedFile.tags()`; distinguish an existing tag-exposure limitation from this RF64-fallback correction before proposing any additional code. Do not relax affirmative/negative precedence or broaden parsing on speculation.

## Resume/integrity

R20C's reviewable diff is `R20C_RF64_INTEGRATION_CORRECTIVE_2026-10-15.patch` (relative to the verified R20 input). `SHA256SUMS` covers every regular file other than the manifest. The source archive is intended to be unpacked standalone, with no `.git` required. Run the commands above; on any failure, fix only a reproduced fault, update this checkpoint and issue ledger truthfully, regenerate `SHA256SUMS`, and repack a full snapshot.
