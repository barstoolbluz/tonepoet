# Transfer Round 8 — corrected engineering report

## Executive result

This corrected overlay implements the Round 8 picker redesign, role-aware multi-FILE CUE carriers, gesture-level CUE policy override, transfer-matrix guards, tolerant APEv2 recovery, and unified OSC 52 publication while keeping version **0.4.4**.

The corrective round closes all reported blockers:

- native WavPack empty strings now obey the public delete contract;
- the native APEv2 serializer is recovery-only rather than the universal `.wv` writer;
- picker mark membership and range insertion are hash-backed rather than quadratic;
- tmux/byobu setup is available inside the application through `:help clipboard` (`:help tmux` and `:help byobu` are aliases);
- the overlay applier now has inode-owned rollback authority, atomic exchange-and-verify commits, conservative concurrent-edit preservation, target and lock hardlink refusal, preservation of the enumerated Unix metadata, confined recovery, fixed-order v2/v3/current dual-lock exclusion, a non-destructive self-identifying lock namespace, and an explicit Linux platform contract.

## WavPack/APEv2 correction

### Serializer authority

`.wv` remains an explicit persistence-policy route, but the route selects the serializer from a typed read:

| Condition | Writer |
|---|---|
| Lofty reads the healthy WavPack/APEv2 carrier | Existing Lofty transaction and serializer |
| Lofty returns the specifically eligible typed APE decoding failure | Native bounded APEv2 recovery writer |
| Lofty fails for any other reason | Honest refusal; native fallback does not activate |
| `.ape` or `.mpc` | Tolerant read fallback only; writes refuse this round |

This preserves the established behavior and blast radius for healthy WavPack files. `MetadataPersistenceBackend::NativeWavPackApe` remains the capability model for the recovery serializer; it does not imply that every `.wv` mutation uses that serializer.

### Delete semantics

The native boundary normalizes both `None` and `Some("")` to one deletion representation before ordinary or numbering fields are serialized.

For combined APE `Track` / `Disc` items:

- deleting the number deletes the inseparable physical item and its total;
- deleting only the total preserves the existing number without a fraction;
- writing a total without a number refuses;
- a malformed value such as `/12` cannot be emitted.

The transfer-level regression fixture writes two malformed-key WavPack targets with an empty source slot and verifies that the corresponding target field is removed while the nonempty target receives its value.

### Native recovery safety

The native path remains bounded and fail-closed: footer geometry, tag size, item count, item boundaries, header consistency, WavPack signature, symlink/hardlink policy, stable file identity, attributes, file sync, atomic replacement, and parent sync are checked. Invalid-key items remain byte-identical. Matching read-only items remain exact no-ops; conflicting edits refuse.

Named correction pins include:

- `native_wavpack_empty_string_deletes_ordinary_ape_item`
- `native_wavpack_empty_numbering_deletes_or_reduces_combined_item`
- `mixed_wavpack_transfer_empty_slot_deletes_native_fallback_target_field`
- `healthy_wavpack_retains_lofty_writer_and_native_is_recovery_only`
- `inline_wavpack_dispatch_bypasses_legacy_database_and_selects_serializer`
- `native_wavpack_write_preserves_invalid_ape_item_byte_exactly`
- `native_wavpack_write_is_byte_idempotent_for_matching_read_only_item`
- `non_ape_lofty_failure_does_not_activate_native_ape_fallback`

## Picker complexity correction

The picker retains a compact `Vec<PathBuf>` compatibility/order cache but adds a synchronized `HashSet<PathBuf>` as the membership authority.

- mark lookup: O(1) amortized;
- additive range insertion: O(range length) amortized;
- effective count with no active visual range: O(1);
- active visual count and visible-order emission: O(visible/range length), never O(N²);
- deterministic `SelectedMany` output remains derived from visible entry order.

All mark mutations pass through helpers that keep the vector and set synchronized. Render-time confirmation labels count marked visible files without allocating or cloning selected paths; path cloning is confined to explicit confirmation. The deterministic 20,000-entry pin `large_range_selection_uses_one_membership_probe_per_visible_entry` asserts exactly one hash-membership probe per visible row and contains no wall-clock threshold.

The remaining picker contract is unchanged: Space marks and advances in file-capable modes, Directories mode retains Space-as-confirm, Alt+Enter explicitly confirms marks, Alt+Click anchors additive ranges, lowercase `v` controls visual selection, and the confirm control remains right-reserved.

## In-app clipboard help

The command parser now accepts a help topic. `:help clipboard`, `:help tmux`, and `:help byobu` open a read-only in-app help surface containing:

```tmux
set -g set-clipboard on
set -g allow-passthrough on
```

The same surface explains terminal OSC 52 permission, the 64 KiB publication cap, and the absence of system-clipboard reads. It reuses the mature read-only preview overlay rather than adding a second scrolling text UI.

Clipboard publication itself remains centralized: every shared text write updates the app clipboard and attempts OSC 52; tmux sessions receive both bare and passthrough-wrapped sequences.

## Transactional overlay application

`tools/apply-overlay.py` is explicitly Linux-only. The corrected authority model includes:

1. fixed-order cross-version lock acquisition: the v2-compatible flat lock first, then the v3+ namespaced lock, both retained through recovery and application;
2. safe creation or validation of the legacy lock as a current-owner, mode-`0600`, single-link regular inode containing either no bytes or exactly the v2 `pid=<decimal>\n` diagnostic; the current applier never truncates or writes it;
3. a self-identifying `.tonepoet-round8-installer/` namespace published with `RENAME_NOREPLACE`, with exact owner/mode/type validation, fixed recognized marker bytes, and a retained zero-length single-link lock inode;
4. nonblocking checkout-wide `flock` without truncating or writing either lock object; a held v2 lock blocks the current applier, a held v3 namespace lock blocks it, and the current applier blocks both predecessor protocols;
5. conservative cleanup of old, structurally recognized `.tonepoet-round8-installer.init-<uuid>` directories only under both locks, after a freshness floor and private-lock check;
6. strict relative-path, manifest, and non-symlink validation;
7. target hardlink refusal (`st_nlink == 1` is mandatory);
8. a durable versioned journal before any stage/backup creation;
9. exclusive same-directory artifacts and SHA-256 verified descriptor copies;
10. preservation and verification of owner, group, mode, atime, mtime, user xattrs, POSIX ACLs, and capabilities, with `fchown` before final `fchmod`/xattr restoration;
11. journaled staged/backup/target inode identities plus a metadata fingerprint;
12. atomic `renameat2(RENAME_EXCHANGE)` commit rather than pathname `hash -> os.replace`;
13. post-exchange validation of both the installed overlay inode and the displaced preimage inode;
14. post-exchange ownership verification and conservative exchange-back attempts; if the pathname changes again between verification and exchange, the ambiguous files are retained and automatic recovery stops rather than asserting an unavailable conditional-rename primitive;
15. recovery classification into intact preimage, exact transaction-owned overlay, or unexpected content/metadata;
16. rollback only for a demonstrably transaction-owned overlay; unexpected targets are preserved and force manual intervention with the journal retained;
17. conservative legacy-journal handling and orphan cleanup that never recreates or replaces a missing/unexpected target without journal authority.

The suite now has twenty-five focused Python tests. The cross-version pins prove both directions of the v2 migration protocol, exclusion by a v3-style namespace holder, fixed-order release on second-lock failure, unchanged legacy-lock bytes/inode/metadata, and simultaneous ownership of both locks throughout recovery and application. It also proves that unrelated or hardlinked legacy objects are refused without mutation and that old, exactly recognized namespace-initialization orphans are cleaned conservatively while a locked live candidate is preserved. The transaction tests additionally prove that edits before commit or after partial commit survive, exact transaction-owned overlays can be rolled back, ambiguous recovery never replaces unexpected content, hardlinked targets refuse, and the enumerated mode and timestamps, a user xattr, and a real `system.posix_acl_access` ACL are preserved by the exercised fixture.

The concurrency claim is deliberately bounded: atomic exchange closes the installer's final pathname race, but unrelated processes remain free to edit after final verification. Later edits are outside installer authority and are preserved; the tool does not claim checkout quiescence.

## Carrier and write-fan-out contract

| Carrier / gesture | Read authority | Write authority |
|---|---|---|
| Plain Files / folder | Member-file tags | Existing Files writer |
| `MetadataSidecar`, folder/member-image | CUE sheet for CUE-carrier reads | Full Files-dimension plan to members first; CUE-capped plan to sidecar only after all members succeed |
| `MetadataSidecar`, explicit `.cue` | Explicit sidecar text; no source resolution or embedded substitution | Sidecar only |
| `SyntheticAlbumPart` | CUE sheet | Sidecar only this round |
| Single-image embedded CUESHEET | Embedded sheet | Embedded FLAC compare-and-swap writer |
| Multi-FILE embedded CUESHEET | Embedded sheet | Read-only; target use refuses before confirmation/write |

The carrier records role, write method, distinct referenced images, and TRACK-number-sorted per-track ownership. It never pairs parse order with number-sorted values.

Files↔Tracks pairing compares the file-derived sequence with the CUE’s authored TRACK-number sequence. Tags win over filename prefixes. Exact disagreement refuses; missing corroboration is disclosed. Confirmed Files targets are re-expanded from retained roots before execution. Sidecar and embedded targets retain their final compare-and-swap/re-resolution checks.

## Other named regression pins

### Picker

- `space_marks_and_advances_while_alt_enter_confirms_many`
- `visual_range_commit_is_additive_and_does_not_toggle_or_advance`
- `alt_click_ranges_from_stable_anchor_and_clears_double_click_state`
- `refresh_discloses_each_marked_path_that_disappeared_exactly_once`
- `large_range_selection_uses_one_membership_probe_per_visible_entry`
- right-reserved confirm geometry at the 56-column embedded minimum

### Carrier and matrix

- `transfer_resolution_policy_parameter_changes_the_selected_carrier`
- `explicit_cue_classification_accepts_role_aware_multi_file_and_refuses_invalid_mixed_selections`
- `directory_cue_and_image_gestures_apply_the_four_arm_carrier_policy`
- `single_image_folder_with_embedded_only_cue_classifies_as_embedded_carrier`
- `multi_file_folder_cue_and_member_gestures_share_album_identity_and_sorted_ownership`
- `files_to_tracks_pairing_requires_the_exact_authored_number_sequence`
- `editor_snapshot_retains_authored_cue_track_numbers_for_files_pairing`
- `embedded_target_refusals_surface_before_confirmation`
- `metadata_sidecar_member_failure_leaves_sidecar_byte_identical`
- `confirmed_files_target_is_reexpanded_and_refuses_membership_changes`

### Clipboard/help

- `text_input_copy_and_cut_publish_through_the_shared_hook`
- exact bare/tmux OSC 52 and size-gate coverage
- `clipboard_help_is_an_in_app_read_only_tmux_byobu_surface`

## Preserved fences and limitations

- Lowercase `v` is unavailable for Files-pane type-ahead; uppercase `V` remains available.
- CUE-carrier reads use sheet metadata rather than member-file tags.
- `SyntheticAlbumPart` writes sidecar text only; embedded fan-out remains fenced.
- Multi-FILE embedded CUESHEET is source-only.
- A folder with several audio files where only one has an embedded CUE remains Files(n).
- A multi-FILE member resolves its album before image-gesture policy.
- `.ape` and `.mpc` writes remain refused.
- First-track collapse on disk paths, ISRC/SONGWRITER CUE writeback, Custom/Paste-tags execution, config cascade, libraries, and disc-image carriers remain fenced.
- No F-key-only capability, decorative Unicode, Ctrl+Q change, CUE byte-span-engine change, or version bump was introduced.

## Validation status

Executed successfully:

- original and corrected archive safety inspection;
- original manifest and embedded/uploaded document identity;
- signed Round 5→8 reconstruction and exact preimage checks for all 18 touched files;
- `git diff --check`;
- lexical validation of delimiters, nested comments, ordinary/byte/raw strings, and character literals over 193,038 Rust lines;
- Python compilation and all twenty-five apply-tool tests;
- end-to-end exact-preimage application, repeated no-op application, and SHA-256 verification;
- regenerated preimage, overlay, and bundle manifests.

Not executable here: `cargo fmt`, `cargo check`, and `cargo test`. The environment has no Rust toolchain, and none was present in the retained file library. The corrected delivery therefore does not claim compiler-backed success. The complete authoritative checkout must still pass the formatter, all-target compiler check, the full 5,295/0 baseline plus new tests, and the real WavPack/byobu field exercises before release handoff.
