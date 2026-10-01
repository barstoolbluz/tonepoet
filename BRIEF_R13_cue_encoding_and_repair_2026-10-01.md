# Brief R13 — a CUE whose filename encoding has no candidate, and what to do when no candidate can help

Date: 2026-10-01
Base: the supplied `tonepoet-src.tar.gz` is authoritative. It is our local
`main` at gate 7212/0.

Issue #56 is a single-image album whose MusicBrainz tagging refuses with
"parsed 14 tracks but the editor expects 1". The hypothesis recorded in that
issue — that APE being decode-only is the discriminator — was wrong, and has
been corrected in `docs/OUTSTANDING_ISSUES.md`. The cause is a CUE text
encoding with no candidate in the decoder.

There are two outcomes here. The first removes this album from the problem
set entirely. The second is the repair surface for what the first cannot fix.

---

## What was measured

`~/torrents/Pret-A-Porter_OST`: one Monkey's Audio image, a sidecar CUE with
14 tracks. Running the production decoder against the real file:

```
decoded FILE line: FILE "Various - Pret-À-Porter.ape" WAVE
decoded name     : "Various - Pret-À-Porter.ape"
exists on disk   : false
actual filename  : "Various - Pret-А-Porter.ape"
names equal      : false
```

The whole CUE contains exactly two non-ASCII bytes, both `0xC0`: one in the
album `TITLE`, one in the `FILE` reference. Windows-1252 decodes `0xC0` as `À`.
The filename on disk holds Cyrillic `А`, U+0410, stored as `\xd0\x90`. Among
Cyrillic codepages only Windows-1251 maps `0xC0` to that character — KOI8-R
gives `ю`, ISO-8859-5 gives `р`, CP866 gives `└`.

So this is a homoglyph, not Russian text: someone typed Cyrillic `А` as a
stand-in for the `à` of *Prêt-à-Porter*, and the bytes on disk and in the CUE
agree with each other under Windows-1251 and under nothing else we carry.

The decoder's legacy candidates are CP932/Shift-JIS, EUC-JP, GBK, Big5 and
Windows-1252 (`src/convert/cue_parser.rs:526`). No Cyrillic encoding is among
them, so Windows-1252 wins on the priority tiebreak and produces a filename
that does not exist.

The consequence chain: the FILE reference does not resolve, single-image
detection returns nothing, no CUE album surface is built, the MusicBrainz flow
is handed the one `.ape` file instead of 14 track rows, and the count guard
refuses.

This is not about APE. A FLAC image with the same filename would fail the same
way.

## What is already right, and need not be touched

- **One decoder, path-aware everywhere.** `decode_cue_bytes_for_path` is the
  only variant any production call site uses — `keybindings.rs`,
  `materializer_archive.rs`, `materializer_cue.rs`, `tag_interchange.rs`,
  `event_loop.rs`, and `parse_cue_file` itself. The path-blind
  `decode_cue_bytes` is reached only by two unit tests. There is one place to
  fix, not several.

- **Path resolution is already the strong signal.** `cue_decode_score` awards
  `Exact` +5,000 against `Missing` 0, which dwarfs every other term. A
  Windows-1251 candidate would resolve this CUE exactly and win decisively; it
  would not be a close call settled by priority.

- **The existing fallbacks cannot rescue this on their own.** The name and
  stem fallbacks compare with `eq_ignore_ascii_case`, so they cannot bridge a
  difference in a non-ASCII character. Both decodings land on `Missing`.

- **The downstream path builders are correct.**
  `paths_for_cue_metadata_surfaces` and `paths_for_single_image_cue_infos`
  (`src/tui/command.rs`) each emit one path per CUE track already.

Note that with a single differentiating byte there is no syntax signal and no
CJK signal to work with. Path resolution is not merely the strongest
discriminator available here — it is the only one.

---

## Outcome 1 — a CUE in an encoding we can identify simply works

A sidecar CUE whose `FILE` reference is recoverable by any reasonable
candidate encoding resolves without the user being told anything, and this
album stops being a problem.

How wide the candidate set should be, and how to keep widening it from making
the scoring ambiguous for files that decode correctly today, is yours to
judge.

---

## Outcome 2 — a repair surface for what encoding cannot fix

Some CUEs will be genuinely malformed: the `FILE` reference matches nothing on
disk under any candidate encoding, because the file was renamed, the reference
was hand-edited, or the sheet is simply wrong.

For those, the user wants an automated CUE repair, offered in the
metadata-editing overlay or in the context menu under "Utilities", and offered
only when either of:

1. the sidecar CUE is the configured authoritative metadata source, **or**
2. the sidecar CUE becomes authoritative because the alternatives — tagged
   files, an embedded CUE — are absent;

and, in addition, all of:

3. the user explicitly acts on the sidecar CUE — clicks or right-clicks it;
4. the CUE actually needs repair, because it has this defect; and
5. the defect is one we can repair.

Where (5) does not hold, offer nothing and warn plainly that the CUE is
malformed. A repair action that cannot repair is worse than a clear warning.

The action should be legible before it runs — what it will change, and in
which file — and the original should remain recoverable.

---

## What we did not determine

Which encodings beyond Windows-1251 are worth carrying, and what classes of
malformation beyond an unresolvable FILE reference are worth repairing.

We also did not establish how a repair should choose a replacement when the
directory holds more than one plausible audio file, nor whether a CUE that is
rewritten on disk should keep its original encoding or be normalized.

## State

Gate 7212 passed / 0 failed, zero warnings. No code has changed since that
run; the tree differs only in `docs/OUTSTANDING_ISSUES.md` and this brief.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The CUE decoder lives in
the root `tonepoet` crate, which peaks at 6.24 GB in one `rustc` and will OOM
under a 4 GB ceiling; write those changes uncompiled and say so.
