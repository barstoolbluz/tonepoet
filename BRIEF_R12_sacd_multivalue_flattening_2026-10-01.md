# Brief R12 — SACD conversions flatten multi-value metadata

Date: 2026-10-01
Base: the supplied `tonepoet-src.tar.gz` is authoritative. It is our local
`main` at gate 7212/0.

## What the user sees

Converting an SACD ISO that has an XML sidecar produces FLAC files whose
metadata editor shows backslashes in multi-value fields:

```
La Petite Bande, Dir. Sigiswald Kuijken\; Sophie Karthäuser - Soprano\;
Petra Noskaiová - Alto\; Christoph Genz - Tenor\; Dominik Wörner - Baryton
```

The same album's editor, opened on the SACD ISO itself, shows no backslashes.
Conversions from DSF, PCM and CUE sources do not show them either. Only SACD.

## What is actually different

The backslashes are not the defect. They are a faithful rendering of what the
converted file contains.

Two conversions, same editor, read with `metaflac`:

| source | ARTIST in the FLAC |
|---|---|
| DSF — *Echoes of an Era* | six repeated `ARTIST=` keys, no semicolons |
| SACD — *Bachkantaten* | one `ARTIST=` key whose value contains semicolons |

Given six distinct values the editor joins them for display and has nothing to
escape. Given one value with literal semicolons inside, it escapes them,
because an unescaped `;` would be indistinguishable from the separator it uses
between values. That escaping round-trips correctly and nothing is corrupted
on save.

So the difference is in the files, not the editor. The SACD path produced a
single delimited string where the DSF path produced distinct values.

## The outcome we want

Multi-value metadata from an SACD sidecar survives conversion as multiple
values, the way it does from every other source. A listener's tagger, a
library scanner and tonepoet's own editor should all see the same structure
for the same album regardless of which source it came from.

Where a field genuinely holds one value that happens to contain a semicolon,
it stays one value and the editor is free to escape it. The point is that the
structure of the metadata should reflect the structure of the source, not an
artifact of how it travelled.

## Where we stopped

We did not determine where the flattening happens — whether the sidecar is
parsed into one value, whether multiple values are joined somewhere in the
SACD path, or whether the writer collapses them. The album is at
`~/temp/La Petite Bande, Dir. Sigiswald Kuijken; …  - Bachkantaten (BWV 55, 56, 98, 180) (2005) [FLAC]`
and its source ISO has an XML sidecar.

Worth knowing: the editor opened on the ISO shows these values unescaped,
which suggests the pre-conversion representation may differ from the
post-conversion one. We did not confirm that.

## State

Gate 7212 passed / 0 failed, zero warnings. `tonepoet-true-peak` 160/0.
Reference requalified; SSRC true-peak registry commissioned.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; the SACD
and metadata paths live there, so write those uncompiled and say so.
