# Prompt: Build a precision-first CD pre-emphasis spectral scorer for tonepoet

## Context

You are working on **tonepoet**, a Rust CLI+TUI audio conversion toolkit. The project already has a pre-emphasis detection feature (`src/tui/preemphasis.rs`) that works in two tiers:

1. **Metadata evidence (working, authoritative):** Checks audio file tags (`PRE_EMPHASIS=1`), CUE files (`FLAGS PRE`), comment tags, and EAC log files. This is reliable and handles most cases where the ripper preserved the subcode flag.

2. **Spectral analysis (needs replacement):** The current implementation does a full-file streaming Goertzel analysis at 8 probe frequencies, detrends per-block, averages residuals, and compares against the theoretical pre-emphasis curve. **This does not work** — the RMS error ranges overlap completely between known pre-emphasized and non-pre-emphasized discs (both land in the 1.0–5.0 dB range). The spectral metric cannot discriminate.

Your task is to **replace the spectral analysis** with a precision-first suspicion scorer that minimizes false "possible" flags while accepting low recall. The metadata evidence tier remains unchanged.

## The problem

CD pre-emphasis (IEC 60908) is a first-order high-shelf filter:
- Time constants: τ₁ = 50 µs, τ₂ = 15 µs
- Transfer function: `H(f) = (1 + jf/f₁) / (1 + jf/f₂)` where f₁ ≈ 3183 Hz, f₂ ≈ 10610 Hz
- Gain: 0 dB at DC, +10 dB at 20 kHz

When a CD was mastered with pre-emphasis but ripped without applying de-emphasis, the audio has the pre-emphasis curve baked into the PCM. The subcode flag (read from the physical CD) is how all existing tools detect this — no known tool reliably detects pre-emphasis from PCM alone.

**Why naive spectral analysis fails:** Music's own spectral character (mastering EQ, genre characteristics, recording techniques) creates consistent non-linear spectral signatures that persist across all blocks and survive detrending. Well-mastered music (e.g., Steve Hoffman remasters of Steely Dan's Aja) can have spectral shapes that match the pre-emphasis curve as well as genuinely pre-emphasized discs.

## Architecture recommended by the reasoning model

### Core: M0/M1/M2 model comparison

Score the file by comparing three models on smoothed log-spectra:

- **M0:** `s(f) = Bc + a₀ + a₁·log(f)` — nuisance model (normal album spectral shape)
- **M1:** `s(f) = Bc + a₀ + a₁·log(f) + shelf_generic(f; θ)` — bright mastering
- **M2:** `s(f) = Bc + a₀ + a₁·log(f) + α·g_PE(f)` — exact Red Book pre-emphasis

Where:
- `s(f)` is a smoothed long-term log spectrum
- `Bc` is a learned low-rank basis of ordinary CD spectral envelopes
- `g_PE(f)` is the exact IEC 50/15 µs emphasis curve
- `α` is the fitted PE strength

**Only flag as candidate if M2 beats both M0 and M1 by a clear margin.** The key false-positive control is that many bright masters can beat "no PE" but cannot beat the exact 50/15 µs shape.

### M1 design: constrained hard-negative library, NOT a free shelf

M1 should be a small dictionary of specific alternatives, not an unconstrained parametric shelf:
- Pure tilt (linear in log-frequency)
- First-order shelf with corner frequency on a coarse grid: {2, 4, 8, 12 kHz}, gain free
- One two-knot smooth brightness spline

Compare M2 against the **best member** of this library on held-out frames. Orthogonalize M1's basis against the PE vector so M1 captures "bright but not Red Book-shaped" while M2 owns the actual Red Book direction.

### Frame selection: low-information frames only

Score on frames where the music is least informative:
- Fade-outs, intros/outros, gaps, reverb tails
- Very low-RMS regions
- Frames with high HF flatness and low tonalness

For selected frames, compute scores both before and after virtual de-emphasis (applying the exact IIR inverse curve). Look for:
- Drop in HF excess matching the PE curve
- Drop in spectral centroid
- Drop in roll-off frequency
- Improvement in high-band flatness on hiss-like frames
- Stability across many low-level frames

A bright cymbal-heavy master can trigger one or two of these; it is much less likely to trigger all of them in low-level broadband hiss-like frames.

### Virtual de-emphasis test

Score the file twice: original and after applying the exact IIR de-emphasis filter. Ask: does the de-emphasized version look more like a normal early-CD spectral envelope?

The reference for "normal" should be an **empirical non-PE manifold** (not pink noise slope):
- A baseline non-PE corpus, preferably era-matched
- Represented as a low-rank manifold or density model over smoothed log spectra
- Compute distance of original spectrum to non-PE manifold
- Compute distance after applying inverse 50/15 µs curve
- Score the change in distance

The user's library at `~/library/` contains thousands of non-PE CDs. Files with `PRE_EMPHASIS=1` tags or CUE files with `FLAGS PRE` are confirmed PE — everything else can seed the non-PE corpus. **Do NOT modify any library files.**

### Counterevidence gates (reject if any are true)

- The fitted improvement is explained almost as well by a generic shelf from M1
- The evidence comes mostly from high-energy music frames rather than low-level frames
- The HF band is highly tonal/peaky rather than broadband
- Virtual de-emphasis makes the 2–6 kHz region unnaturally recessed
- Only one track on an album fires while the rest do not (pre-emphasis is per-track metadata but typically applied disc-wide)

### Album-level pooling (optional second layer)

Keep the per-file scorer as the core. Add album-level aggregation when grouping is available:

Each track emits:
- `LLR_i = log p(data|M2) - log p(data|best M1/M0)`
- Fitted PE amplitude `α_i`
- Confidence / usable-frame count

Album layer pools with:
- Median or trimmed mean of LLR_i
- Penalty for wide spread in α_i
- Support count: how many tracks show positive evidence

Album label boosts to "possible" only when at least k tracks support PE and their α_i values cluster reasonably well.

### Feature set

Use a smoothed spectral envelope, not point probes:
- STFT → log-magnitude in ERB or 1/3-octave bands
- Cepstral smoothing or low-order polynomial smoothing
- Robust aggregation with median, not mean

Feature vector per file:
- Exact-curve fit residual
- Exact-curve gain over generic shelf (M2 vs best M1)
- Fitted PE amplitude α
- Score restricted to low-level/hiss-like frames
- Cross-track consistency score (when album context available)
- Penalty for tonal HF structure
- Penalty when only full-level frames support the hypothesis

### Output labels

The output should mean:
- **unlikely** — no spectral evidence
- **possible** — some evidence, but "possible" should be RARE (accept low recall)
- **strong candidate** — multiple cues line up
- **indeterminate** — insufficient data for a judgment

## Existing codebase

### Key files

- `src/tui/preemphasis.rs` — current implementation (rewrite the spectral analysis portion; keep the metadata evidence checking)
- `src/tui/analyze.rs` — DR/peak/RMS analysis (uses ffmpeg-next for in-process decode; reference for decode patterns)
- `src/tui/probe.rs` — `probe_audio()` for file info, `read_metadata()` for tags
- `src/tui/bit_compare.rs` — streaming decode via ffmpeg CLI pipe (reference for the pipe pattern)
- `src/tui/verify.rs` — async file processing pattern
- `Cargo.toml` — current dependencies (add `rustfft` if needed for proper STFT)

### Build requirements

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo build
cargo test --lib --workspace
```

Must build inside `nix develop` — system Rust (1.82) cannot resolve some transitive deps.

### IIR de-emphasis filter coefficients

For bilinear transform at sample rate `fs`:
```
a = 2 * 50e-6 * fs
b = 2 * 15e-6 * fs
b0 = (1 + b) / (1 + a)
b1 = (1 - b) / (1 + a)
a1 = (1 - a) / (1 + a)
y[n] = b0*x[n] + b1*x[n-1] - a1*y[n-1]
```

At 44.1 kHz: b0 ≈ 0.4294, b1 ≈ -0.0597, -a1 ≈ 0.6303

### Calibration data

The user's library (`~/library/`) contains:

**CUE-confirmed pre-emphasized discs** (grep for `FLAGS PRE` in .cue files):
- Asia, Boston, Billy Joel, Journey, Pink Floyd, Toto, Santana, Miles Davis, Springsteen, Boz Scaggs, Heart, REO Speedwagon, and more (~68 directories)

**Tag-confirmed pre-emphasized files** (grep for `PRE_EMPHASIS=1` or `PRE-EMPHASIS=1`):
- ~30 of the above directories also have per-file tags

**Known non-pre-emphasized (hard negatives — these should NOT trigger):**
- Steely Dan - Aja (Steve Hoffman mastering) — bright, smooth HF, false-positive-prone
- Steely Dan - The Royal Scam — another bright master
- Any modern remaster, any disc from late 1980s onward

**Same-album different-pressing pairs** (for matched-master comparison):
- Asia: Japan 35DP-25 (PE) vs Audio Fidelity (no PE) vs Japan SHM (no PE)
- Toto IV: Japan 35DP-12 (PE) vs MFSL (no PE) vs many others
- Journey Escape: Japan 35DP-6 (PE) vs Columbia reissue (no PE) vs MFSL
- Santana Abraxas: Japan 1st-Press (PE) vs MFSL (no PE) vs HDTracks

**IMPORTANT: Do NOT modify, move, rename, or write to any files in ~/library/. Read-only access only.**

### Previous diagnostic data

Previous testing showed these spectral RMS errors (detrended Goertzel residual vs theoretical PE curve):
- Pre-emphasized discs: 1.0 – 5.0 dB (median ~2.5)
- Non-pre-emphasized Steely Dan: 1.4 – 5.0 dB (median ~2.8)
- Ranges overlap completely — this is why the naive approach fails

## What to build

1. Replace the spectral analysis in `src/tui/preemphasis.rs` with the M0/M1/M2 comparison framework
2. Implement STFT-based smoothed log-spectrum computation (add `rustfft` dependency if needed)
3. Implement frame selection (low-RMS, high HF flatness, low tonalness)
4. Implement virtual de-emphasis scoring
5. Implement counterevidence gates
6. Optionally implement album-level pooling
7. Build/train the non-PE corpus model from the user's library (store as a compact representation)
8. Calibrate thresholds on the known PE/non-PE discs
9. Write diagnostic output to `/tmp/tonepoet-preemph-diag.txt` for threshold tuning

The metadata evidence checking (tags, CUE files, log files) should remain unchanged. The spectral scorer runs alongside it as a supplementary signal.

### Integration point

The public API is:
```rust
pub async fn detect_preemphasis(path: PathBuf) -> PreemphasisResult
```

This is called per-file from the command dispatch. It should:
1. Check metadata evidence (existing code, keep as-is)
2. Run the new spectral scorer
3. Combine: metadata evidence is authoritative; spectral is supplementary

### Training workflow

The non-PE corpus model needs to be trained once from the user's library. Consider:
- A one-time `:preemph-train` command that scans non-PE files and saves the model
- Or an automatic model that builds incrementally from analyzed files
- Store the model in the tonepoet database or a separate file in `~/.cache/tonepoet/`

## Constraints

- Must compile with `nix develop` + `cargo build`
- No modifications to files in `~/library/`
- The tool processes files via ffmpeg CLI pipe (`ffmpeg -f s32le -acodec pcm_s32le pipe:1`) for streaming decode with constant memory
- Keep the diagnostic dump to `/tmp/tonepoet-preemph-diag.txt` for threshold calibration
- Optimize for **precision** (low false positives), not recall. Accept that many real PE discs will be missed. The metadata tier handles the well-documented cases; the spectral tier is for the undocumented ones.
