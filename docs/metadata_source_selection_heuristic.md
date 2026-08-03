# Metadata-Source Selection Heuristic for Music Directories

You are correcting the metadata-source selection heuristic for music directories that may contain:

- individually tagged audio files;
- one or more sidecar CUE files;
- embedded CUE data;
- single-file album images;
- multiple image files representing sides or discs;
- pre-split one-track-per-file albums;
- mixed or partially covered directory contents.

Do not implement a blanket rule such as:

> If a sidecar CUE exists, use it.

Do not hard-code sidecar CUE as always preferable to embedded CUE or individual files.

The user has an explicit configurable source-priority order among:

1. Individual files
2. Sidecar CUE
3. Embedded CUE

Examples include:

```text
Embedded CUE -> Sidecar CUE -> Individual files
```

```text
Individual files -> Embedded CUE -> Sidecar CUE
```

```text
Individual files -> Sidecar CUE -> Embedded CUE
```

That configured order must be respected.

The missing behavior is not a new fixed precedence rule. It is a semantic viability heuristic that determines whether each representation is actually capable of describing the content before the configured priority order is applied.

## Governing rule

Apply the user's configured source-priority order among representations that are:

- present;
- valid;
- applicable to the audio content being resolved;
- semantically usable for track-level metadata.

A configured preference must not force the application to select a representation that cannot describe the logical tracks present in the audio.

The governing distinction is:

> Configuration determines preference. Semantic viability determines which candidates may participate in that preference ordering.

## Resolve a content scope, not necessarily an entire directory

The heuristic must operate on the coherent content scope being resolved, not automatically on every audio file found in the directory.

A content scope may be:

- one album image;
- one side image;
- a side-A/side-B pair;
- one disc;
- a multi-disc set;
- a set of images collectively described by one or more CUE files;
- a selected subset of a mixed directory.

A CUE that validly describes only one subset of a directory must not globally disqualify the individual-file representation for unrelated or uncovered audio files.

Therefore:

> Exclude Individual files only for the content scope that an applicable CUE representation coherently covers. Do not use partial CUE coverage to invalidate individual-file metadata for unrelated or uncovered files.

## Individual files are not always a viable track representation

For a normal pre-split album, each physical audio file represents one logical track. In that case, the individual files and their tags are a viable metadata representation.

For an image-based album, one or more physical audio files contain multiple logical tracks. Examples include:

- one whole-album audio image;
- one image for side A and one image for side B;
- multiple disc or side images;
- several images described by one or more CUE files.

In those cases, the image files' own tags describe physical containers, not the logical tracks within them.

A file such as:

```text
wish-you-were-here-side-a.wv
```

may have an artist, album, and title tag, but those tags cannot represent the several individual songs contained within that side image.

Therefore, when a valid and applicable CUE representation demonstrates that one or more physical audio files in the current content scope contain multiple logical tracks, the individual-file representation is not viable for track-level source selection for that scope and must be skipped.

This does not alter the user's priority order. It removes an unusable candidate before applying that order.

## Required image-detection heuristic

Determine whether an applicable sidecar or embedded CUE maps multiple valid audio `TRACK` entries to the same physical carrier.

The association mechanism differs by CUE type:

- For a sidecar CUE, associate tracks with physical audio files through its resolved `FILE` references.
- For an embedded CUE, associate tracks with the audio file containing the embedded cue, unless the existing parser or format explicitly provides a different binding.

Conceptually:

```text
for each valid, applicable CUE representation:
    associate each audio TRACK with its physical carrier

    for sidecar CUE:
        resolve the TRACK through the applicable FILE reference

    for embedded CUE:
        bind the TRACK to the containing audio file
        unless the existing parser establishes another binding

    group valid audio TRACK entries by physical carrier

    if any physical carrier has more than one logical audio track:
        that CUE describes image-based content
```

The decisive fact is:

> At least one physical audio carrier is subdivided into multiple logical tracks.

Do not rely solely on an aggregate test such as:

```text
cue track count > audio file count
```

That may work in common cases, but it is only a proxy. The implementation must inspect the actual mapping between logical tracks and physical carriers.

Count only files or carriers covered by the applicable CUE representation. Unrelated audio files in the directory must not distort the decision.

## Candidate selection algorithm

The intended behavior is conceptually:

```text
content_scope = determine the coherent audio content set being resolved

candidates = detect all representations that are:
    present,
    valid,
    applicable to content_scope,
    and sufficiently complete for content_scope

image_evidence = determine whether any valid applicable CUE
                 proves that one or more physical carriers
                 in content_scope contain multiple logical tracks

if image_evidence is present:
    mark Individual files nonviable for that content_scope

return the highest-priority viable candidate
       in the user's configured source order
```

This means the heuristic filters candidate viability. It does not rewrite or override the configured ordering among the remaining candidates.

## Required precedence behavior

### Configuration: Individual files -> Embedded CUE -> Sidecar CUE

For a pre-split album:

```text
01 - Track One.flac
02 - Track Two.flac
03 - Track Three.flac
```

where each file represents one logical track, individual files win.

For a single album image with multiple logical tracks:

```text
album.wv
album.cue
```

and a valid embedded CUE, individual files are skipped because the physical image cannot describe the logical tracks.

The embedded CUE wins because it is the user's highest-priority viable representation.

If the embedded CUE is absent, invalid, inapplicable, or insufficiently complete for the content scope, the valid sidecar CUE wins.

### Configuration: Individual files -> Sidecar CUE -> Embedded CUE

For a pre-split album, individual files win.

For a side-A/side-B image rip, individual files are skipped.

The sidecar CUE wins if valid, applicable, and sufficiently complete for the content scope. Otherwise the embedded CUE is tried.

### Configuration: Embedded CUE -> Sidecar CUE -> Individual files

A valid and applicable embedded CUE wins because the user explicitly prefers it.

The existence of an applicable sidecar does not override that configuration.

### Configuration: Sidecar CUE -> Embedded CUE -> Individual files

A valid and applicable sidecar CUE wins because the user explicitly prefers it.

The application must not silently promote embedded CUE merely because it also proves that the files are images.

## Sidecar and embedded CUE are peers in configured selection

Sidecar CUE and embedded CUE are both possible structural metadata representations.

Neither should be hard-coded as globally authoritative over the other.

The user's configured priority determines which valid CUE representation wins when both are available.

The image heuristic may establish that individual files are unusable, but it must not decide between sidecar and embedded CUE except through the configured ordering.

## Structural authority versus field-level metadata authority

When a CUE subdivides a physical image into logical tracks, the selected CUE representation is authoritative for the structural facts it defines, including:

- logical track boundaries;
- track order;
- track-to-carrier mapping;
- indexes;
- pregaps;
- cue-provided titles and performers where present.

Do not infer that the selected CUE necessarily supersedes every metadata field from every other source.

A CUE may omit or incompletely describe:

- release date;
- genre;
- catalog number;
- label;
- album artist;
- edition information;
- MusicBrainz identifiers;
- other album-level or provenance-rich metadata.

Therefore:

> In an image-based layout, the selected CUE representation is authoritative for logical track structure and for the metadata fields it explicitly supplies. Preserve the project's existing field-merging, enrichment, and provenance rules. Do not invent a blanket replacement policy that discards useful metadata from tags, MusicBrainz, or other established sources.

This task concerns source viability and structural authority. It must not silently redefine unrelated field-merging semantics.

## Applicability, validity, and coverage

Do not let a random, stale, malformed, unrelated, or partially applicable CUE disqualify otherwise usable individual files.

Before using a CUE as evidence that the individual-file representation is not viable, establish that the CUE is sufficiently valid and applicable to the content scope.

At minimum, inspect whether:

- its referenced audio files resolve to the intended files;
- its audio tracks are structurally valid;
- its track-to-carrier mappings are coherent;
- it describes the same content scope being considered;
- its track boundaries and ordering are plausible under existing validation rules;
- its coverage is sufficient for the representation to stand as a candidate for that content scope.

Use the project's existing applicability, validation, and coverage mechanisms where available. Do not invent a weaker parallel parser or validation path.

A valid CUE that covers only one side, disc, or subset may establish image-based structure for that subset, but it must not automatically invalidate individual-file handling for unrelated or uncovered content.

## MusicBrainz TOC corroboration

The MusicBrainz TOC lookup is not merely a downstream mechanism for filling missing metadata.

It also provides independent corroboration that a CUE representation correctly describes the audio content.

The reasoning is:

1. A sidecar or embedded CUE maps multiple logical tracks onto one or more physical audio images.
2. That mapping indicates that the physical files are containers rather than individually usable track representations.
3. A MusicBrainz lookup using the cue-derived TOC, or a combined TOC derived from multiple applicable CUE files, returns a coherent release match.
4. That match corroborates the inference that the CUE's track boundaries, ordering, and metadata correspond to the actual content.

Therefore:

> A valid CUE that maps multiple logical tracks onto a physical audio carrier provides structural evidence that the individual-file representation is not usable for track-level metadata. A coherent MusicBrainz match from its derived TOC independently strengthens and corroborates that inference.

Do not describe MusicBrainz solely as metadata enrichment or a missing-tag fallback.

It serves both:

- corroboration of the inferred structural representation and content identity;
- metadata lookup, reconciliation, or enrichment.

However, MusicBrainz corroboration is optional evidence, not a prerequisite for recognizing a structurally valid image-based representation.

The following must not, by themselves, make an otherwise valid and applicable CUE nonviable:

- no MusicBrainz result;
- an ambiguous result;
- network unavailability;
- rate limiting;
- temporary service failure;
- absence of the release from MusicBrainz;
- unusual pregaps, data tracks, side-based layouts, or partial-disc structures that prevent an exact match.

Conversely, a MusicBrainz match must not rehabilitate a malformed, stale, unrelated, or otherwise inapplicable CUE.

A MusicBrainz result also must not override the user's configured priority between sidecar and embedded CUE. It strengthens confidence and content identity; it does not create a new hard-coded precedence rule.

## Multiple sidecar CUE files and combined TOCs

Preserve or implement support for cases where:

- one CUE describes multiple image files;
- multiple CUE files describe multiple sides or discs;
- a combined TOC is assembled from multiple applicable CUE files;
- MusicBrainz lookup is performed against the combined TOC.

Where multiple sidecar CUE files collectively and coherently describe image-based content, they may jointly establish that the individual-file representation is unusable for the covered content scope.

The configured order still decides whether the selected source is the sidecar CUE set or an applicable embedded CUE representation.

Do not assume that every CUE file in a directory belongs to the same logical set. Group only cues that existing project rules establish as jointly applicable.

## Embedded CUE applicability

An embedded CUE must be associated with the physical audio file that contains it unless the existing parser establishes another explicit relationship.

For multiple image files, determine whether:

- each file has an applicable embedded CUE;
- the embedded representations collectively describe the whole content scope;
- an existing combined or multi-source embedded-CUE path exists;
- partial embedded coverage is treated as insufficient, degraded, or mixed according to existing project policy.

Do not assume that the presence of one embedded CUE in one file fully describes every image file in the directory.

Preserve existing valid behavior and report any ambiguity you find rather than silently inventing unsupported mixed-source behavior.

## Cases that must work

### Single-file album image

```text
album.wv
album.cue
```

The sidecar defines twelve tracks against `album.wv`.

If a valid embedded CUE also exists:

- skip Individual files;
- select Sidecar CUE or Embedded CUE according to configured priority.

If only the sidecar exists:

- skip Individual files;
- select the sidecar.

If only a valid embedded CUE exists and it defines multiple logical tracks within the containing image:

- skip Individual files;
- select the embedded CUE.

The mere presence of an embedded CUE that does not demonstrate image-based structure must not automatically disqualify Individual files.

### Side-A/side-B images under one sidecar CUE

```text
album-side-a.wv
album-side-b.wv
album.cue
```

The CUE assigns several tracks to each image.

Skip Individual files for that covered content scope.

Choose between Sidecar CUE and Embedded CUE according to configured priority and actual validity, applicability, and coverage.

### Multiple images and multiple sidecar CUE files

```text
album-side-a.wv
album-side-a.cue
album-side-b.wv
album-side-b.cue
```

If the cues coherently define multiple tracks within the images, skip Individual files for the covered content scope.

Treat the applicable sidecar CUE set as one candidate representation where the existing architecture supports that model.

Choose it or the applicable embedded representation according to configured priority.

### Pre-split album with a sidecar CUE

```text
01 - Track One.flac
02 - Track Two.flac
03 - Track Three.flac
album.cue
```

If the CUE maps one logical track to each physical file, Individual files remain viable.

Apply the configured priority normally.

Therefore:

- Individual files win only when configured above the CUE sources.
- Sidecar CUE wins when configured above Individual files.
- Embedded CUE wins when configured above both and is valid and applicable.

Do not treat one-track-per-file CUE content as image evidence.

### Partial CUE coverage in a mixed directory

Example:

```text
album-side-a.wv
album-side-a.cue
01 - Bonus Track.flac
```

If `album-side-a.cue` subdivides only `album-side-a.wv`:

- it may make Individual files nonviable for the side-A image content scope;
- it must not globally disqualify the individually tagged bonus track;
- the resolver must not silently treat the entire directory as one CUE-covered representation unless existing grouping rules establish that scope.

### Misleading aggregate counts

Construct a case where aggregate track and file counts would not reliably reveal the image relationship, but one physical carrier is assigned multiple cue tracks.

The per-carrier mapping must identify the image correctly.

### Unrelated files

Unrelated audio files in the same directory must not affect whether the applicable CUE subdivides its covered images.

## Implementation guidance

Inspect the existing source-selection, cue-discovery, cue-validation, metadata-editor, and MusicBrainz code before changing anything.

Determine whether the code already supports:

- configured ordering among Individual files, Sidecar CUE, and Embedded CUE;
- one image file with multiple CUE tracks;
- multiple image files under one CUE;
- multiple sidecar CUE files;
- combined multi-CUE TOCs;
- embedded CUE extraction and validation;
- sidecar CUE applicability checks;
- content-scope or grouping logic;
- partial CUE coverage;
- MusicBrainz lookup from a single cue-derived TOC;
- MusicBrainz lookup from a combined TOC;
- field-level merging or metadata provenance.

Preserve all working behavior.

The likely defect may be that an existing detector requires two or more audio paths, such as:

```text
audio_paths.len() >= 2 && tracks > files
```

That would detect some multi-image cases while missing a single-file album image.

If so, replace it with the semantic per-carrier track-mapping test. Do not replace it with a blanket `sidecar_available` condition.

Also inspect whether the current implementation immediately picks the first configured source before evaluating whether that source can represent the logical tracks. If so, separate:

1. content-scope determination;
2. source discovery;
3. source validation and applicability;
4. coverage determination;
5. semantic viability;
6. configured-priority selection;
7. existing field-level merge or enrichment behavior.

Do not broaden the task into a metadata-merging redesign unless the existing implementation is directly broken by the source-selection defect.

## Tests

Add or update tests that prove at least the following:

1. One audio image with multiple sidecar CUE tracks:
   - Individual files are not viable for the covered content scope.
   - Sidecar or embedded CUE wins according to configured priority.

2. One audio image with both sidecar and embedded CUE:
   - Sidecar wins when configured above embedded.
   - Embedded wins when configured above sidecar.
   - Individual files are skipped even when configured first.

3. Embedded CUE carrier binding:
   - An embedded CUE is associated with its containing audio file unless existing parser semantics establish another binding.
   - Multiple logical tracks in the embedded CUE make Individual files nonviable for that image.

4. Two side images with multiple tracks per image:
   - Individual files are skipped for the covered scope.
   - Remaining CUE candidates follow configured order.

5. Multiple images under one sidecar CUE:
   - Per-carrier mapping detects image-based content.

6. Multiple sidecar CUE files:
   - The applicable cue set can establish image-based content.
   - Combined TOC behavior is preserved.

7. Pre-split one-track-per-file album:
   - Individual files remain viable.
   - The configured priority is followed without heuristic reordering.

8. Partial CUE coverage:
   - A cue that covers only one subset does not globally disqualify Individual files for unrelated or uncovered audio.

9. Invalid or unrelated sidecar CUE:
   - It does not disqualify valid individual files.

10. Invalid or unrelated embedded CUE:
    - It does not disqualify valid individual files.

11. Unrelated audio files in the directory:
    - They do not affect the image determination for the covered scope.

12. Misleading aggregate counts:
    - A per-carrier mapping catches an image case that a simple total-tracks-versus-total-files test would mishandle.

13. MusicBrainz cue-derived TOC match:
    - A coherent match is retained or surfaced as corroborating evidence for CUE applicability and content identity.
    - It is not treated solely as metadata enrichment.

14. MusicBrainz unavailable or unmatched:
    - Lack of a match does not negate independently established structural applicability.
    - Network or service failure does not make an otherwise valid CUE nonviable.

15. Malformed CUE with MusicBrainz-like identity:
    - External metadata or a possible match does not rehabilitate a structurally invalid or inapplicable CUE.

16. Combined multi-CUE MusicBrainz TOC match:
    - Existing behavior is preserved and tested.

17. User preference:
    - Every supported permutation of the three configured sources is respected among viable candidates.

18. Structural versus field-level authority:
    - Selecting a CUE for track structure does not discard richer album-level metadata contrary to existing merge and provenance rules.

## Required report

Do not re-litigate the intended behavior.

Inspect the code and report:

- the existing configured-priority behavior;
- how content scopes or coherent audio sets are currently determined;
- how source validity, applicability, and coverage are currently determined;
- the exact heuristic gap;
- whether the implementation currently confuses configured priority with source viability;
- whether the single-image case is missed;
- whether partial CUE coverage can incorrectly affect unrelated files;
- whether per-file or per-carrier track mapping already exists anywhere;
- how embedded CUE tracks are bound to their containing carrier;
- whether single-CUE and combined-multi-CUE MusicBrainz corroboration paths already exist;
- whether MusicBrainz is currently being treated as enrichment, corroboration, or a required gate;
- how embedded CUE applicability works for multiple image files;
- how field-level metadata merge and provenance behave after structural source selection;
- the exact code changed;
- the tests added or updated;
- confirmation that the configured source order remains authoritative among viable representations.
