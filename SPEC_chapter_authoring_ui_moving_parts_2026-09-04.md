# Chapter authoring — what the interface has to surface

**Purpose:** input for a UI/wizard mockup. This describes the problem domain and enumerates
every moving part the interface must expose. Layout, flow and interaction design are yours.

---

## 1. What this feature is for

Tonepoet converts audio. Some sources are **one long file that is really many pieces**:

- an **audiobook** — a single `.m4b` running 19h 43m, internally divided into 45 titled chapters
  ("Opening Credits", "Prologue", "One", "Two"…). This is a real file we test against.
- a **vinyl rip** — one continuous file per side, containing 5–8 songs, with the side breaks
  and track starts recorded separately.
- a **DJ mix or live concert recording** — one file, many tracks, often with no divisions
  recorded anywhere.
- a **CD image** — one big `.flac` or `.wav` plus a `.cue` sheet listing where each track starts.

The divisions can live in two places. They may be **stored inside the audio file** — MP4-family
containers (`.m4b`, `.m4a`, `.mp4`) hold real chapter entries, and FLAC can carry a whole CUE
sheet as an embedded tag. Or they may live in a **sidecar CUE sheet**: a small text file beside
the audio listing track titles and start positions. Same information, different places; §8 has
the per-format detail.

Tonepoet can already **read** both, **cut** audio precisely at those positions, and **write**
them back out. What it cannot do is let a user **create or change a division point**. That is
the gap this interface fills.

## 2. What users are trying to do

Five situations, which differ mainly in what the interface starts with:

1. **There is no structure at all.** A 3-hour DJ mix, or an audiobook whose chapters were never
   written. The user wants to create divisions from scratch — by hand, or by asking for "every
   5 minutes" or "12 equal parts".
2. **Structure exists and is valid, but wrong.** The chapters mark the publisher's sections, not
   what the user wants. They want to start from what's there and change it.
3. **Structure exists but is broken.** The file's own chapter map violates the rules in §6 —
   overlapping entries, a gap, a chapter of zero length. Today the conversion simply fails.
   The user needs to see why and fix it.
4. **Tonepoet tried to make sense of it and could not**, and said so. Same as above, arriving
   from an error message that should be able to lead here.
5. **The user wants to throw it away and start over.**

The end goal is nearly always one of: **split this into one file per chapter**, or **keep it as
one file but record the divisions properly**, or both.

## 3. The one constraint that shapes everything

**There is no audio playback, and no waveform display.** Tonepoet has no audio-output capability
at all — the user cannot listen to the file, and there is nothing to look at that represents the
sound.

This rules out the interaction most chapter editors are built around: play the audio, listen for
the gap, drop a marker. It cannot work here.

So division points get set in one of three ways only:

- **typed** — the user knows or has been told the time ("chapter two starts at 16:44");
- **generated** — by a rule ("every 5 minutes", "12 equal parts");
- **imported** — read from structure that already exists in the file or a sidecar.

A mockup premised on scrubbing a waveform is not buildable. A mockup that makes typing times and
applying rules feel good is exactly right.

## 4. The thing being edited

A **program**: one continuous audio file, with a known total duration and sample rate.

A **list of division points** over it, in order. Each entry has:

| field | editable? | notes |
|---|---|---|
| number | derived | 1…N, renumbers itself when entries are added, removed or reordered |
| start position | **yes** | where this chapter begins |
| pregap | **yes, optional** | marks a region *before* this entry's start — the silence between tracks on a CD rip, or applause running in from the previous song on a live album. Marking it lets the converter decide whether it belongs to the previous entry, this one, or neither |
| title | **yes** | free text |
| duration | derived | next entry's start minus this one's; the final entry runs to the end |
| is-final | derived | the last entry is open-ended and absorbs whatever remains |

The interface also needs the program-level facts: total duration, and **where the current
structure came from** (embedded chapters, a sidecar CUE, or nothing) — because that affects what
saving means.

### Time appears in three units

- **samples** — the internal truth used to cut audio precisely;
- **CUE frames** — 75 per second, the unit a CUE sheet can store;
- **human time** — what someone types and reads.

These do not convert losslessly. A position measured in samples **floors to the preceding CUE
frame** when written to a CUE sheet, so a boundary can move by up to 1/75 of a second. Don't
hide this. A user placing a deliberate boundary should be able to tell which unit is
authoritative for what they're saving to.

## 5. Operations the interface must offer

None of the first three exist anywhere in the product today:

1. **Insert a division point** — at a typed time, or relative to the current one.
2. **Remove a division point** — neighbouring durations recompute.
3. **Move a division point** — retype the time, or nudge it by a small step.

And over the list as a whole:

4. **Edit one title.**
5. **Apply many titles at once** — paste a newline-separated list, lines mapping onto entries in
   order. Users will paste a chapter list from a book's contents page or a tracklist from a
   website. This pattern already exists elsewhere in the product.
6. **Generate titles from a pattern** — see §7.
7. **Clear everything and start again.**
8. **Save** — see §8.

## 6. Rules the interface has to enforce, and explain

The conversion pipeline validates structure strictly and **fails the entire job** if it doesn't
hold. The user must find out while editing, not hours later:

- division points must be **strictly increasing** — each after the one before;
- no entry may be **zero length**;
- entries must **join up** — a gap or an overlap larger than one sample is rejected;
- the first entry must start at the very **beginning of the program** — no audio may sit outside
  the structure;
- every entry must still have a real duration after rounding.

A list can be temporarily invalid mid-edit — e.g. while moving one point past another. The design
needs a way to show which entries are offending and why, and to stop a save that would produce a
file the converter will reject.

## 7. Titles and numbering

The user gives a **base title** and a **numbering format**. Four formats, with these meanings:

- `N` → 1, 2, 3 … 10
- `NN` → 01, 02, 03 … 10 (width follows the total, with a **minimum of two digits** — so a
  three-chapter program still renders 01/02/03, and only a 100+ entry program widens to three)
- `N/NN` → 1/10, 2/10
- `NN/NN` → 01/10, 02/10

Note the last two were described as "n of nn" in the original request, but the existing
formatter renders them with a slash, as shown. Worth confirming which is wanted before a mockup
commits to the word "of".

"Part" + `NN` over 12 entries gives `Part 01` … `Part 12`. Because the padding width depends on
the total, crossing 99 entries re-pads every existing title — a preview before applying is worth
having.

Pattern numbering and pasted titles are alternatives; a user may apply a pattern and then correct
individual entries by hand.

## 8. Where the result goes

Three destinations, and a user may want more than one. Availability depends on the format, and
in a way that is not obvious:

- a **sidecar CUE file** written beside the audio — available for anything;
- **structure stored inside the audio file** — but the mechanism differs by container:
  - **MP4-family** (`.m4b`, `.m4a`, `.mp4`) store real chapter entries, with titles and times;
  - **FLAC** cannot store chapter entries, but *can* carry an entire CUE sheet as an embedded
    tag, which Tonepoet already reads and writes. Same information, different shape;
  - **WAV** has no embedded-structure support here;
- **split output** — one file per chapter, produced when the conversion runs.

So "embed the structure" means something slightly different for an audiobook than for a FLAC
image, and is unavailable for WAV. The interface shouldn't offer an option that would silently
do nothing, and shouldn't imply FLAC users have no in-file option.

## 9. Scale

Design for more than an album's worth of rows:

- the audiobook above: **45 entries** over 19h 43m;
- a vinyl rip: 2 sides, ~20 entries;
- "every 2 minutes over a 3-hour mix": **90 entries**.

The list must stay navigable and editable at that size — and a user re-titling 45 chapters by
hand needs that to not feel punishing.

## 10. Deliberately open

Yours to decide:

- whether this is a wizard with steps, a single editing surface, or a mode within the existing
  metadata editor;
- whether edits apply live or are staged behind an explicit confirm;
- how a time is entered;
- whether **start position** or **duration** is the primary editable column — the data stores
  start positions and derives durations, but users think in both, and "make this chapter 4
  minutes long" is a natural request;
- how generation is presented — replace the list outright, or propose and let the user accept;
- how validation problems are surfaced.
