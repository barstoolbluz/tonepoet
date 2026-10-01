# Brief R13 — a CUE whose filename encoding has no candidate, and what to do
# when no candidate can help

Date: 2026-10-01
Base: the supplied `tonepoet-src.tar.gz` is authoritative. It is our local
`main` at gate 7212/0.

Issue #56 is a single-image album whose MusicBrainz tagging refuses with
"parsed 14 tracks but the editor expects 1". The hypothesis recorded in that
issue — that APE being decode-only is the discriminator — is wrong. The cause
is a CUE text encoding with no candidate in the decoder.

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

The byte in question is `0xC0`. Windows-1252 decodes it as `À`; the filename
on disk holds Cyrillic `А`, U+0410, which UTF-8 stores as `\xd0\x90`. The CUE
is Windows-1251, where `0xC0` is exactly that Cyrillic `А`.

The decoder's legacy candidates are CP932/Shift-JIS, EUC-JP, GBK, Big5 and
Windows-1252. No Cyrillic encoding is among them, so Windows-1252 wins by
default and produces a filename that does not exist.

The consequence chain: the FILE reference does not resolve, single-image
detection returns nothing, no CUE album surface is built, the MusicBrainz flow
is handed the one `.ape` file instead of 14 track rows, and the count guard
refuses. Both CUE-aware path builders already emit one path per track, so
nothing downstream needs changing.

This is not about APE. A FLAC image with the same filename would fail the same
way.

---

## Outcome 1 — a CUE in an encoding we can identify simply works

A sidecar CUE whose `FILE` reference is recoverable by any reasonable
candidate encoding resolves without the user being told anything, and this
album stops being a problem.

The selection mechanism for this already exists and is already path-aware; the
decoder's own comment says scoring lets a candidate win "when their decoded
FILE references actually exist". What is missing is a candidate that can
decode this file. How wide the candidate set should be, and how to keep
widening it from making the scoring ambiguous, is yours to judge.

---

## Outcome 2 — a repair surface for what encoding cannot fix

Some CUEs will be genuinely malformed: the `FILE` reference matches nothing on
disk under any candidate encoding, because the file was renamed, the reference
was hand-edited, or the sheet is simply wrong.

For those, the user wants an automated CUE repair, offered in the
metadata-editing overlay or in the context menu under "Utilities", and offered
only when all of these hold:

1. the sidecar CUE is the configured authoritative metadata source, **or**
2. the sidecar CUE becomes authoritative because the alternatives — tagged
   files, an embedded CUE — are absent;

and in addition, all of:

3. the user explicitly acts on the sidecar CUE — clicks or right-clicks it;
4. the CUE actually needs repair, because it has this defect; and
5. the defect is one we can repair.

Where (5) does not hold, offer nothing and warn plainly that the CUE is
malformed. A repair action that cannot repair is worse than a clear warning.

The action should be legible before it runs — what it will change, and in
which file — and the original should remain recoverable.

---

## What we did not determine

Which encodings beyond Windows-1251 are worth carrying, whether widening the
candidate set can make path-aware scoring ambiguous for files that currently
decode correctly, and what classes of malformation beyond an unresolvable
FILE reference are worth repairing.

We also did not check whether other CUE paths — conversion, CUE generation,
the AccurateRip flow — share this decoder or have their own candidate lists.

## State

Gate 7212 passed / 0 failed, zero warnings. Issue #56 is open and its recorded
hypothesis should be corrected by whatever lands here.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The CUE decoder lives in
the root `tonepoet` crate, which peaks at 6.24 GB in one `rustc` and will OOM
under a 4 GB ceiling; write those changes uncompiled and say so.
