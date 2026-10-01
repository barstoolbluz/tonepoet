# Brief R14 — R13 works in the field but leaves two of its own tests failing

Date: 2026-10-01
Base: the supplied `tonepoet-src.tar.gz` is R13 corrective R2 applied to our
`main`, unmodified.

R13 is accepted in substance. Outcome 1 is confirmed against the real album,
and the repair surface compiles clean with no warnings. Two of R13's own new
tests fail, and they are pointing at something real that is not in R13.

---

## What now works

Measured against `~/torrents/Pret-A-Porter_OST` itself, not a fixture:

```
R13 tracks parsed : 14
R13 decoded FILE  : FILE "Various - Pret-А-Porter.ape" WAVE
R13 resolves      : true
R13 assessment    : NotNeeded
```

Issue #56 is fixed. The album reaches the normal 14-track CUE surface and is
correctly judged to need no repair.

## What fails

```
cue_file_reference_repair_preserves_identified_legacy_encoding_when_representable
cue_file_reference_repair_refuses_implicit_whole_sheet_encoding_normalization
```

Both fixtures write a Windows-1252 CUE containing one non-ASCII byte, `0xF6`,
in `TITLE "Börk"`, and both reference a `FILE` that does not resolve. Both
assume the decoder will identify that sheet as Windows-1252. It identifies it
as GBK.

The first fails because GBK cannot encode `Börk.flac`, so a repair the test
expects to succeed is refused. The second fails because GBK *can* encode
`Аудио.flac`, so a repair the test expects to be refused is offered. One cause,
two opposite symptoms.

The mechanism is in the scorer, not in anything R13 added. `0xF6 0x72` is a
valid GBK double-byte sequence decoding to a CJK character, so the GBK
candidate collects the `+10` CJK bonus in `cue_decode_score`. That bonus is
awarded whenever path context exists, and it outranks the priority tiebreak
that is supposed to keep Windows-1252 the Western default. When no `FILE`
resolves there is no `+5,000` path signal to drown it out, so `+10` decides the
encoding of the whole sheet.

Windows-1251 is not involved: it earns no CJK bonus and sits at priority 5, so
it never wins these cases. This is pre-existing behavior that R13's new tests
are the first to exercise, because "no FILE resolves" is exactly the repair
domain R13 introduced.

Verified independently of our code: `b"B\xF6rk"` decodes under GBK to `鰎`,
`Аудио.flac` is GBK-encodable, `Börk.flac` is not.

## Required

A Western CUE whose only non-ASCII bytes happen to form valid multi-byte
sequences in an East Asian codepage is not identified as East Asian, and a
repair writes back in the encoding the sheet is actually in. R13's two failing
tests express the right expectations; the decoder should meet them.

Keep what already works. The path-resolution signal must stay decisive when it
is available — that is what fixes the real album, and it must not be weakened
to fix a case where no path resolves. The CJK bonus exists because it helps
when there is corroborating evidence; the problem is its weight relative to a
tiebreak, not its existence.

Also consider whether a repair should be offered at all while the source
encoding is identified only weakly. Writing bytes back in a guessed codepage is
a worse failure than declining.

## Third failure, not yours

```
tui::probe::tests::native_artwork_write_failure_keeps_rollback_journal_when_restore_fails
```

Passes in isolation. Lease-staging contention in `tui::probe` — the known
wandering flake described in `CLAUDE.md`. Not R13.

## State

`cargo check --workspace --all-targets`: clean, zero warnings.
Gate: 5,822 passed / 3 failed in `-p tonepoet --lib`; every other target green.
Two of the three are the failures above; the third is the flake.

`cargo fmt --all -- --check` is not a usable gate here and was not run as one:
it reports diffs in roughly 260 files including `build.rs` and all of
`tui-file-picker`, which R13 never touched. The tree has never been
rustfmt-clean.

## Scope

Fix the identification defect and the two tests. Do not widen this into more
codepages, fuzzy matching, multi-candidate ranking, or encoding normalization
policy — R13 deliberately left those open and they should stay open.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The CUE decoder lives in
the root `tonepoet` crate, which peaks at 6.24 GB in one `rustc` and will OOM
under a 4 GB ceiling; write those changes uncompiled and say so. R13 did
exactly that and compiled clean here, so that remains the right approach.
