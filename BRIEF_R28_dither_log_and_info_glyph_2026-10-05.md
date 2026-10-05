# Brief R28 — the dither log line, the dither stage, and the info glyph

Date: 2026-10-05
Base: the supplied `tonepoet-src.tar.gz` is `main` with R27 applied. Gate 7339
passed / 0 failed; Reference qualification current.

## 1. The log says dither was not applied. It was.

Converting a pre-emphasised 16-bit/44.1 kHz FLAC album with De-emphasis On and
the default TPDF, the log reads:

```
Dither: requested (TPDF) — not applied (executed command did not emit a dither stage)
```

The terminal command it recorded:

```
sox -S -t raw -e floating-point -b 64 -L -r 44100 -c 2 <carrier>.f64le -b 16 -C 8 <out>.flac
```

Run twice on identical input, that command shape produces different output;
with `-D` added it produces identical output. Dither is applied.

## 2. No explicit dither stage is emitted

The command carries neither `-D` nor a `dither` effect. The dithering above is
SoX's own behaviour on depth reduction, not a stage TonePoet asked for.

R24 §4.1 requires plain TPDF to be realised as the `dither` effect.

## 3. The `ⓘ` renders in two colours

It appears half cyan, half purple, split down the middle.

`ⓘ` is U+24D8, East Asian width Ambiguous — one cell in some terminals, two in
others. The layout treats it as one and styles one cell; the terminal draws it
across two, and the second cell carries the following span's colour.

The hitbox for the same glyph is registered two columns wide
(`convert_screen.rs:309`).

Nothing in the workspace handles ambiguous-width characters. `⚠` and the
bullet and diamond glyphs are in the same class.

## 4. Drop one log line

Remove `De-emphasis effect: …` from the conversion log. The remaining three
de-emphasis lines stay as specified.

## The outcome we want

The log states what happened. Dither that was applied is reported as applied,
and names what applied it.

TonePoet chooses its own terminal dither rather than inheriting a tool's
default.

The `ⓘ` renders in one colour in any terminal, whichever width it is given.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so. Some files are Reference-source-locked
(`reference_source_lock.rs` lists them); if your change touches any, say so and
we requalify here.
