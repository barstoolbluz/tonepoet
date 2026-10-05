# Brief R30 — a negative pre-emphasis tag, and a contention flake we have not diagnosed

Date: 2026-10-05
Base: the supplied `tonepoet-src.tar.gz` is `main` with R29 applied and
Reference requalified.

Two separate things. The first is a declared change. The second is context for
a problem we have not solved.

---

# 1. Write a negative pre-emphasis tag

## The situation

A pre-emphasised album can be converted to a lossy format, or to a resolution
other than 16-bit/44.1 kHz, and its CUE sheet and catalogue number travel with
the converted files. Opening those outputs later, TonePoet reports them as
possibly pre-emphasised on CUE or catalogue evidence, although the
pre-emphasis has already been removed.

## Required

Where TonePoet applies de-emphasis, the output carries `PRE_EMPHASIS=0`
rather than having the tag stripped.

`PRE_EMPHASIS` of `0` or `NO`, in any letter case, is categorical: TonePoet
does not report or warn about pre-emphasis for that source, whatever a CUE
`FLAGS PRE` or a catalogue match says.

## Not established

Whether a negative tag should also be written when no de-emphasis was applied,
and how the tag is carried in formats whose tagging model differs from Vorbis
comments.

---

# 2. A contention flake, described

We have not diagnosed this and are not proposing a cause. What follows is
only what was observed.

## Shape

On the current tree, three consecutive full-gate runs each returned
7347 passed / 1 failed, a different test each time, every one passing 3/3 when
run alone:

```
tui::probe::tests::id3_numbering_alias_conflicts_fail_closed_and_equal_aliases_coalesce
tui::keybindings::permanent_delete_tests::permanent_delete_is_blocked_by_recovery_reserved_claim
concurrency::tests::same_process_recovery_coholds_exact_descriptor_without_weakening_strict_acquire
```

## Tests observed failing this way

Each of these failed in a suite run during this session and passed when run
alone:

```
concurrency::tests::same_process_journal_coholder_shares_deliberate_export_authority
concurrency::tests::raw_inherited_fd_export_keeps_descriptor_visible_until_duplicate_closes
concurrency::tests::journal_operation_final_logical_owner_unlocks_accidental_fork_copy_immediately
concurrency::tests::same_process_recovery_coholds_exact_descriptor_without_weakening_strict_acquire
tui::probe::tests::dsf_numeric_numbering_round_trips_and_lexical_write_is_atomic
tui::probe::tests::non_flac_cancellation_before_fallback_does_not_create_backup
tui::probe::tests::parsed_artwork_rollback_recovers_while_retiring_stale_common_write_lock
tui::probe::tests::id3_numbering_alias_conflicts_fail_closed_and_equal_aliases_coalesce
tui::keybindings::permanent_delete_tests::permanent_delete_is_blocked_by_recovery_reserved_claim
convert::pipeline::materializer_single::tests::single_file_writer_round_trips_ordered_performer_and_arranger_lists
convert::pipeline::memory_budget::tests::execution_staging_live_and_recovery_reserved_block_stale_cleanup_until_retired
```

Two more appeared in suite runs but were never isolated individually:

```
concurrency::tests::journal_operation_deliberate_lifetime_export_remains_live_until_export_closes
concurrency::tests::truncated_durable_descriptor_routes_by_path_to_lifecycle_cleanup_only
```

## Messages observed

```
assertion `left == right` failed   left: Live   right: RecoveryReserved
scanner probe must acquire the now-ownerless durable descriptor inode:
  Os { code: 11, kind: WouldBlock }
retire test recovery reservation: "persistent lease is live-owned: …lease"
lazy reclamation must remove the retired exported descriptor
setup-orphan descriptor still has a live holder: …lease
create persistent lease staging file …lease.tmp-…: No such file or directory (os error 2)
metadata write busy: create persistent lease staging file …
native FLAC metadata-region tag write refused … cannot start metadata write
```

## Rates observed

Three consecutive `cargo test -p tonepoet --lib` runs on a clean tree earlier
in the session returned 3, 3 and 2 failures. Full-gate runs on the same commit
sometimes returned 0. On the current tree, three consecutive full-gate runs
each returned exactly 1.

Every lease path cited in these failures is under the test sandbox
(`/tmp/nix-shell.*`), not the user's `~/.config/tonepoet/concurrency-v1`.

## The outcome we want

The gate is deterministic: a run either fails for a reason in the code under
test, or passes.

---

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so. Some files are Reference-source-locked
(`reference_source_lock.rs` lists them); if your change touches any, say so and
we requalify here.
