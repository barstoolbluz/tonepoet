# assets — sanity assessment

50 of 50 read · 23 surprising

Each entry below is one **reading**, of a function or of a whole file. An
agent was given its name, signature, neighboring names and comments — never
its body — and wrote down what it expected to find. Then it opened the file.
The gap between the two is the finding. A file's own entry is titled `the file
itself` and asks whether the header at the top describes what is actually in
there.

`read at` is a hash of the body as it was when the reading was made. When it
stops matching the code, the reading is marked STALE and goes back in the
queue.

What this is and how to add to it: [README.md](README.md)

## assets/reference/hexload_labels_reference.rs

### the file itself
- spec 3 · read at `b99b949e383b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:53:19Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A standalone reference implementation (kept under assets/reference/, outside src/, likely for documentation/regression purposes rather than being compiled into the main build) for detecting vinyl-pressing info from record label text. detect_pressing_info, normalize_vinyl_case, and parse_inch_format recognize audiophile reissue labels (MFSL, Analogue Productions, DCC, Classic Records, Speakers Corner, Music On Vinyl, Blue Note, etc.), country/year/reissue markers, and colored-vinyl variants, backed by dozens of test_* functions pinning exact expected outputs and fallback behavior for specific labels, countries, and years. No file-level doc comment exists despite the file's size and specificity.
- found: A vinyl pressing-info detector: detect_pressing_info(folder_name, year) -> LabelInfo parses parenthetical/bracket text in folder names via a cascade of a hardcoded LABEL_MAPPINGS HashMap (lazy_static), West-German year logic, country-prefix lists, regex-based reissue/color/minimax pattern matching, and audiophile-label prefix fallbacks; normalize_vinyl_case title-cases strings; parse_inch_format detects 7/12-inch format text. 46 unit tests cover label precedence, fallback behavior, and false-positive guards. No file-level doc comment.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no

### `detect_pressing_info` — QUIRKY — TANGLED — TRAP
- spec 3 · read at `9973927f2168` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:44:34Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A large pattern-matching function that scans a folder name (plus optional year) for known vinyl-pressing markers — country codes (UK, German pre/post-1990), audiophile labels (MFSL, MoFi), colored/red vinyl, RPM speed (45), reissue indicators — and builds up a LabelInfo struct field by field via a long cascade of string/substring checks.
- found: Parses folder_name for a parenthetical group and runs it through an ordered cascade: exact LABEL_MAPPINGS lookup (with German/West-German year adjustment and audiophile-label/country detection), then parse_inch_format, then an audiophile prefix fallback (MFSL/DCC-style) with word-boundary checks, then several dynamic regexes for '<year> Reissue', '<country> <year> Reissue', '<country> Minimax', and colored-vinyl patterns, then a big country-prefix string table, finally falling back to a [bracket] LABEL_MAPPINGS lookup if nothing in parens matched.
- predicted: some · documented: none · derivable: yes · legible: some · trap: yes
- note: This is a long ordered fallback chain where each stage returns immediately on match — reordering stages or adding a new LABEL_MAPPINGS key that collides with a dynamic regex pattern would silently change behavior since earlier stages win.

### `normalize_vinyl_case`
- spec 3 · read at `a2d2002285b8` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:16:25Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Splits the string on whitespace, and for each word capitalizes the first character and lowercases the rest, then rejoins with single spaces — a simple title-case normalizer for phrases like "blue vinyl" or "WHITE VINYL".
- found: Splits on whitespace, uppercases first char and lowercases rest of each word, joins with spaces.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `parse_inch_format` — TRAP
- spec 3 · read at `6fcb7af45fa0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:53:15Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Scans the content string for a "7-inch" or "12-inch" marker (via regex or substring match); if found, also scans for a known country/region token (e.g. "Japan", "UK", "AUS", "US", "Germany"), special qualifiers like "RSD" or "Mono", and a resolution pattern like "24-96" or "24-192". It then assembles and returns these pieces into a formatted string in the order Country, qualifiers, size-inch, Mono, resolution — joined with double spaces per the example — returning None if no inch-size marker is present.
- found: Detects "7 inch"/"12 inch" substring, then a long if/else chain matches country names to codes (defaulting to "US" if none match), detects RSD as a pre-qualifier and Mono/Stereo/EP/WLP/Promo as post-qualifiers, joins them with the format string, and always appends a hardcoded " 24-96" resolution suffix regardless of actual content.
- predicted: most · documented: none · derivable: yes · legible: full · trap: yes
- note: The "24-96" resolution suffix is hardcoded/always appended, not detected from content — anyone extending this for other resolutions (e.g. 24-192) needs to know that's currently faked.

### `test_audiophile_label_detection` — QUIRKY
- spec 3 · read at `530e4299fe5d` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:36:42Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A small test that feeds a handful of known audiophile reissue label strings (e.g. MFSL, DCC, Analogue Productions) into detect_pressing_info or a similar function and asserts each is recognized/flagged as an audiophile label, paralleling the sibling tests for specific labels like MFSL/MoFi 45rpm.
- found: Tests a single album title "Album (MFSL)" through detect_pressing_info, asserting is_audiophile is true and pressing_info is the specific string "MFSL LP 24-96" (with a double space) rather than just a boolean flag.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: I expected multiple labels tested here; it's just one MFSL case, and it checks an exact formatted pressing_info string (with a double space) not just a flag.

### `test_west_german_pre_1990` — QUIRKY
- spec 3 · read at `72f5bcb9dffc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:34:20Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Tests that parsing a vinyl record label string typical of West German pressings before 1990 (e.g. containing "Germany" text without post-unification markers) via a function like detect_pressing_info yields a country field of "West Germany" and a year/era before 1990, distinguishing it from the sibling test_german_post_1990 case.
- found: Calls detect_pressing_info("Album (German)", Some("1985")) and asserts the resulting pressing_info string equals "West German First-Press LP 24-96" — a formatted description string, not a struct with separate country/year fields.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The function returns a single formatted description string (pressing_info) rather than structured country/year data, and includes a specific catalog-like suffix ("24-96") whose meaning isn't obvious from this test alone.

### `test_german_post_1990` — QUIRKY
- spec 3 · read at `63fa1cce986c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:23:07Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Calls detect_pressing_info with a label string indicating "Germany" (not "West Germany") pressing info, then asserts the returned pressing metadata identifies it as a post-1990/reunified Germany pressing (as opposed to the West Germany case tested in the sibling test_west_german_pre_1990).
- found: Calls detect_pressing_info(\"Album (German)\", Some(\"1995\")) and asserts pressing_info equals the literal formatted string \"German First-Press LP 24-96\", confirming that a post-1990 year keeps the \"German\" (not \"West German\") label text.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The comparison is against an exact formatted pressing_info string, not a structured field distinguishing West vs reunified Germany.

### `test_reissue_detection`
- spec 3 · read at `b4b958dede31` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:42:15Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A unit test that calls detect_pressing_info with a sample string containing a reissue marker (e.g. "Reissue" or a reissue year) and asserts the resulting struct correctly identifies it as a reissue, likely checking a boolean or enum field.
- found: Calls detect_pressing_info(\"Album (2022 Reissue)\", None) and asserts info.is_reissue is true and info.pressing_info equals a specific formatted string with the year, format, and bit depth/sample rate.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_uk_pressing` — QUIRKY
- spec 3 · read at `e4b45ca2d3b0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:20:38Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Calls detect_pressing_info with a sample vinyl label string containing a UK pressing indicator (e.g. "Made in England" or "UK") and asserts the resulting struct/enum marks the pressing country/origin as UK.
- found: Test that detect_pressing_info(\"Album (UK RL)\", None) returns country Some(\"UK\") and expands the pressing_info to the specific decoded string \"UK RL-Mastered LP 24-96\" — a code decoding I didn't anticipate.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `test_minimax_standalone` — QUIRKY
- spec 3 · read at `2164bfcd949b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:16:21Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A short unit test that calls the label-detection function (like detect_pressing_info or normalize_vinyl_case) on a string containing just "Minimax" with no country/year qualifiers, and asserts it returns a label of "Minimax" with country and year fields empty/None, distinguishing it from the peer test_minimax_with_country which includes a country.
- found: Calls detect_pressing_info(\"Album (Minimax)\", None) and asserts the resulting pressing_info string equals \"Minimax LP 24-96\" — a fixed formatted description, not a structured label/country/year breakdown.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: detect_pressing_info returns a formatted display string (with baked-in format/bit-depth like 'LP 24-96'), not a structured record with separate label/country/year fields as the peer naming might suggest.

### `test_minimax_with_country` — QUIRKY
- spec 3 · read at `775ec1302391` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:28:17Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Calls detect_pressing_info (or similar) with a sample label string containing "MiniMax" plus a country name (e.g. "Germany" or "UK"), then asserts that the returned struct correctly identifies the label as MiniMax and also populates/matches the country field, similar to the other "_with_country" variant tests near it (test_colored_vinyl_with_country, test_dynamic_country_year_reissue).
- found: Calls detect_pressing_info twice with "Album (UK Minimax)" and "Album (Japan Minimax)", asserting the returned pressing_info string is exactly "<Country> Minimax First-Press LP 24-96" — the second call is explicitly testing the dynamic country fallback for a country not in some static list.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `test_red_wax` — OBSCURE — TRAP
- spec 3 · read at `9b27d7c0f985` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:15:41Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Feeds a label string containing "red wax" into detect_pressing_info (or similar parser), then asserts the result identifies it as colored vinyl with color "red", similar to the neighboring test_colored_vinyl_* tests.
- found: Asserts detect_pressing_info(\"Album (Red Wax)\", None) produces the exact canned string \"Japan Red Wax First-Press LP 24-96\" (and a Mono variant), embedding details like country \"Japan\", \"First-Press\", and \"24-96\" that are not present anywhere in the input string.
- predicted: none · documented: none · derivable: yes · legible: full · trap: yes
- note: The asserted output bakes in facts (country Japan, First-Press, 24-96) that don't appear in the input text at all, so this test only makes sense if detect_pressing_info has hidden default/inference logic elsewhere in the file — that logic isn't visible from this test alone and a naive edit to it would silently break this assertion for reasons that look unrelated.

### `test_german_odeon` — OBSCURE
- spec 3 · read at `6009da18029e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:12:06Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Feeds a raw vinyl label string referencing "Odeon" (a German record label) into the parser and asserts the resulting normalized struct has label="Odeon" and country="Germany" (possibly with a year or catalog number field too), similar to the sibling country/label detection tests.
- found: Calls detect_pressing_info with an album title containing \"(German Odeon)\" and asserts it returns a specific hardcoded/looked-up pressing_info description string \"West German Odeon First-Press LP 24-96\", not a generic label/country struct.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: The function returns a single free-text pressing_info string baked with details (press generation, format, bit/sample rate) that aren't derivable from the input text alone — this looks like it's matching against some reference/lookup table not shown here.

### `test_horzu` — QUIRKY
- spec 3 · read at `0d1e93ffd416` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:22:52Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Feeds a sample label/catalog string containing "Horzu" (a German budget record label) into the label parser, and asserts the parsed result correctly identifies "Horzu" as the label field, likely alongside a catalog number or country/year extraction similar to sibling tests.
- found: Calls detect_pressing_info(\"Album (Horzu)\", None) and asserts the resulting pressing_info string equals \"West German Horzu First-Press LP 24-96\" — a fully composed descriptive string inferring country, press generation, format, and a numeric code from just the bracketed label hint.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Didn't anticipate the composed descriptive string format or that so much (country, press-generation, '24-96' code) gets inferred from a bare label name with no other input.

### `test_dynamic_year_reissue`
- spec 3 · read at `627e00b3c231` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:47:17Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Unit test feeding a label string containing a year plus the word "Reissue" (e.g. "1985 Reissue") into the label-detection/parsing function, asserting the result flags it as a reissue and correctly extracts that dynamic year value.
- found: Calls detect_pressing_info("Album (2016 Reissue)", None) and asserts is_reissue is true and pressing_info equals a formatted string "2016 Reissue LP 24-96".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_dynamic_country_year_reissue`
- spec 3 · read at `bcaa2a77b35d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:52:46Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Exercises a text pattern combining a country name, a year, and "reissue" (e.g. "Germany 1978 Reissue") and asserts the label/pressing parser extracts country, year, and reissue flag correctly.
- found: Calls detect_pressing_info on \"Album (UK 2021 Reissue)\" and asserts is_reissue is true and pressing_info equals \"UK 2021 Reissue LP 24-96\".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_label_reissues` — QUIRKY
- spec 3 · read at `e9a7265073cf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:10:06Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Tests several known reissue-label strings (audiophile reissue imprints like MFSL/Mobile Fidelity/Analogue Productions or similar) through the label parser and asserts each is correctly classified/flagged as a reissue rather than an original pressing.
- found: Calls detect_pressing_info on album title strings containing "(Label Reissue)" for three different labels (Craft, Studio One, Matador) and asserts the resulting pressing_info string is formatted as "Label Reissue LP 24-96".
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: My prediction guessed this tested audiophile-imprint classification (MFSL/AP-style), but it actually tests generic label-name + \"Reissue\" parenthetical parsing into a formatted pressing_info string.

### `test_colored_vinyl_with_country` — QUIRKY
- spec 3 · read at `a9c5a6b58785` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:26:49Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Constructs an input string representing a colored-vinyl record label that also includes a country designation, runs it through a label-parsing function, and asserts the parsed result has both the correct color field and correct country field populated.
- found: Calls detect_pressing_info with a title string containing "(UK White Vinyl)" and no extra arg, then asserts is_reissue is true and pressing_info equals a specific formatted string "UK White Vinyl Reissue LP 24-96" (with a double space and a hardcoded resolution suffix), rather than just checking color/country fields separately.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `test_colored_vinyl_with_year` — QUIRKY
- spec 3 · read at `c21f26c070e1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:31:56Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A test asserting that parsing a label string like "Colored Vinyl, 2020" (or similar with a specific color and year embedded) correctly extracts both the vinyl color attribute and the release year into the expected structured fields, exercising the label-parsing logic alongside its sibling tests for other pressing variants.
- found: Calls detect_pressing_info on an album title containing "(2024 Blue Vinyl)" and asserts it's flagged as a reissue with a fully formatted pressing_info string "US 2024 Blue Vinyl Reissue LP 24-96" — including a default US country code, reissue label, LP format marker, and a 24-96 resolution suffix that aren't derivable from the input text alone.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The pressing_info output bakes in a default country ("US") and a fixed \"24-96\" resolution suffix that don't appear in the input string — worth flagging for whoever edits detect_pressing_info next, since those defaults are invisible from this test's name.

### `test_colored_vinyl_bare` — QUIRKY
- spec 3 · read at `a670a704d32d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:37:11Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Tests that a bare "colored vinyl" label string (no country or year qualifier) is parsed/matched correctly by the label-detection function, asserting it identifies the colored-vinyl variant with country/year fields left empty/default — a baseline case alongside the with_country and with_year sibling tests.
- found: Calls detect_pressing_info with "Album (Gold Vinyl)" and no country, asserting is_reissue is true and that the resulting pressing_info string defaults to "US Gold Vinyl Reissue LP 24-96" — filling in US and a default bitrate/format when none is given in the title.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `test_mfsl_45_rpm_exact`
- spec 3 · read at `52e15ad6b12c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:54:52Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A unit test asserting that parsing an MFSL (Mobile Fidelity Sound Lab) label string specifying 45 RPM produces an exact match result identifying the label as MFSL with a 45 RPM variant/speed field, rather than falling back to a generic/unknown match.
- found: Calls detect_pressing_info on an album string containing "(MFSL 45 RPM)" and asserts the resulting pressing_info string is the exact reissue description "MFSL 45-RPM Reissue LP 24-96" and that is_audiophile is true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_mofi_45_exact`
- spec 3 · read at `8a20cca447b0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:42:15Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Test that passes a specific "MoFi" 45 RPM label string into the label-matching function and asserts an exact match result (not a fallback/fuzzy match), confirming the parser recognizes this precise Mobile Fidelity Sound Lab pressing variant.
- found: Calls detect_pressing_info(\"Album (MOFI 45)\", None) and asserts the pressing_info string is exactly \"MFSL 45-RPM Reissue LP 24-96\" and is_audiophile is true — confirms exact-match detection as predicted, though the specific expanded label text and audiophile flag field were not something I called out precisely.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `test_mofi_45_rpm_exact`
- spec 3 · read at `d27d0bbd122f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:29:13Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Parses a specific "MoFi ... 45 RPM" label string and asserts the parser returns an exact match to the MoFi 45 RPM label variant (not a generic fallback), mirroring the sibling test_mfsl_45_rpm_exact/test_ap_45_rpm_exact tests for other audiophile reissue labels.
- found: Calls detect_pressing_info on \"Album (MOFI 45 RPM)\" and asserts pressing_info equals \"MFSL 45-RPM Reissue LP 24-96\" and is_audiophile is true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_ap_45_rpm_exact`
- spec 3 · read at `fa002890cf03` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:47:17Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Feeds a known exact label/catalog string containing "AP" and "45 RPM" through the label parser and asserts the result matches the exact Analogue Productions 45rpm variant, not a fallback/generic AP match.
- found: Calls detect_pressing_info on "Album (AP 45 RPM)" and asserts the pressing_info string equals "Analogue Productions 45-RPM Reissue LP 24-96" and that is_audiophile is true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_ap_33_rpm_exact`
- spec 3 · read at `f9de2ac0d2fd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:52:21Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A unit test that feeds a specific catalog/label string (something like "AP 33⅓ RPM" or an "Analogue Productions" 33 RPM release) into the label-parsing function and asserts it returns the exact expected label/variant match (not a fallback), mirroring sibling tests like test_mfsl_45_rpm_exact and test_mm_33_rpm_exact for other audiophile labels.
- found: Calls detect_pressing_info with an album title containing "(AP 33 RPM)" and asserts the resulting pressing_info string is the specific expanded label "Analogue Productions Reissue LP 24-96" and is_audiophile is true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The exact expanded string includes a double space and a "24-96" resolution suffix that isn't derivable without seeing the body.

### `test_dcc_compact_classics_exact`
- spec 3 · read at `0e894c471a6d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:25:55Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A test asserting that a release string containing "DCC Compact Classics" (an audiophile reissue label) is matched exactly to its own label/variant rather than falling back to a generic audiophile fallback, similar to the MFSL/AP/MM exact-match tests nearby.
- found: Asserts detect_pressing_info on a DCC Compact Classics release string returns pressing_info "DCC LP 24-96" and is_audiophile true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_mm_33_rpm_exact`
- spec 3 · read at `652cb7d8613f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:57:27Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Calls the label classification function with an input string naming the "MM" (Music Matters) reissue label at 33 RPM, and asserts it returns an exact-match variant/result identifying Music Matters 33 RPM specifically, rather than falling back to a generic audiophile label match.
- found: Calls detect_pressing_info with an album title containing "(MM 33 RPM)" and asserts the resulting pressing_info string is "Music Matters 33-RPM Reissue LP 24-96" and that is_audiophile is true.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `test_mfsl_unknown_variant_fallback`
- spec 3 · read at `bbd7931d67b6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:02:57Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Constructs an MFSL release string with a variant that isn't one of the known exact patterns (e.g. not "45 RPM" or a specific catalog code), and asserts that the label-matching function still returns a generic MFSL fallback label/tag rather than None/no match, mirroring the DCC and AP fallback tests nearby.
- found: Calls detect_pressing_info on a title with an unrecognized MFSL variant string, asserting it falls back to a fixed generic label "MFSL LP 24-96" and flags is_audiophile and is_reissue as true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_dcc_unknown_variant_fallback`
- spec 3 · read at `fd7c3d8b30dd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:07:57Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Feeds a label string containing "DCC" with an unrecognized/unknown variant suffix into the label-detection function, and asserts it still falls back to matching the generic DCC label rather than returning no match.
- found: Calls detect_pressing_info with an album title containing an unrecognized DCC variant ("DCC Gold"), asserting it falls back to a fixed generic DCC pressing_info string ("DCC LP 24-96") and flags is_audiophile and is_reissue.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The exact fallback string and audiophile/reissue flags aren't guessable without seeing detect_pressing_info's DCC branch.

### `test_ap_unknown_variant_fallback`
- spec 3 · read at `e9f99f372011` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:33:22Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Feeds a label string that starts with an "AP" (Analogue Productions) prefix but has a variant/suffix the exact-match table doesn't recognize, and asserts the label-parsing function still falls back to a generic "Analogue Productions" classification rather than returning None/unmatched, mirroring test_mfsl_unknown_variant_fallback and test_dcc_unknown_variant_fallback.
- found: Calls detect_pressing_info with a title containing an unrecognized 'AP Special Edition' variant and asserts it falls back to a generic Analogue Productions reissue classification, producing the pressing_info string 'Analogue Productions Reissue LP 24-96' and setting both is_audiophile and is_reissue flags.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The exact fallback pressing_info string format (label + 'Reissue LP' + resolution) and which flags get set aren't derivable from the test name alone — needed the body.

### `test_analogue_productions_fallback_longest_first`
- spec 3 · read at `a0571264a3bf` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:46:12Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A unit test asserting that when a label string could match multiple "Analogue Productions" (AP) fallback patterns of different lengths, the matcher picks the longest/most specific matching pattern rather than a shorter generic one — likely calling the label-lookup function with a crafted string and asserting the returned label/variant equals the more specific expected value.
- found: Calls detect_pressing_info on a title containing \"Analogue Productions 45 RPM\" and asserts it resolves to the full \"Analogue Productions\" reissue label (not the shorter \"AP\" alias) with is_audiophile true, confirming longest-match-first precedence in the label matcher.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: The interesting behavior (longest-prefix-wins) is only explained by an inline comment in the test body, not by the function name/signature alone.

### `test_classic_records_fallback`
- spec 3 · read at `c66b6ca25e72` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:34:16Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Calls a label-detection function with an input string containing "Classic Records" in some unrecognized-variant form, and asserts the function still falls back to returning the canonical "Classic Records" label (rather than None), exercising the generic fallback-matching path shared with the other _fallback tests.
- found: Calls detect_pressing_info on an album string mentioning "Classic Records 200g" (not an exact catalog match) and asserts it falls back to a generic "Classic Records Reissue LP 24-96" pressing_info string with is_audiophile true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_speakers_corner_fallback`
- spec 3 · read at `3da3daa62cc1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:39:22Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Calls the label-matching function with a Speakers Corner label text that doesn't match a specific exact known pressing variant, and asserts it still resolves to a generic Speakers Corner label match/name via fallback logic rather than returning no match.
- found: Calls detect_pressing_info with a generic "Speakers Corner 180g" album string (no exact variant match) and asserts it falls back to a generic "Speakers Corner Reissue LP 24-96" pressing_info string and marks is_audiophile true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_no_false_positive_from_ap_prefix`
- spec 3 · read at `d5e911784c29` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:08:15Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Tests that a label string merely starting with the letters "AP" (like "Apple" or "Apex") but not actually "Analogue Productions" is not misidentified as an AP/Analogue Productions release by the label-matching logic, asserting the classifier returns no match or a different/fallback result instead of the AP label.
- found: Calls detect_pressing_info on "Album (Apple Records)" and asserts is_audiophile is false, confirming "Apple" doesn't false-match the "AP" (Analogue Productions) prefix due to lack of a word boundary.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `test_random_label_no_audiophile_fallback`
- spec 3 · read at `883f04ebbc9f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:49:49Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Asserts that a generic/random record label string (not one of the known audiophile reissue imprints like AP, MFSL, DCC, MoV, BN) is NOT matched by the label-fallback detection function, i.e., the function returns None for it. Likely constructs a label string and checks equality against None or an empty/absent match.
- found: Calls detect_pressing_info with a full album string containing a made-up label name, and asserts the resulting info.is_audiophile flag is false.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_mfsl_45_exact_wins_over_fallback`
- spec 3 · read at `77ec63410471` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:17:42Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Feeds a label string that matches an exact "MFSL 45rpm" catalog entry into the classifier and asserts it returns that specific exact match rather than falling back to a generic MFSL fallback classification.
- found: Calls detect_pressing_info with an album title containing "(MFSL 45)" and asserts it returns the exact LABEL_MAPPINGS entry ("MFSL 45-RPM Reissue LP 24-96") with is_audiophile true, rather than a generic MFSL fallback.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_classic_short_form` — QUIRKY
- spec 3 · read at `e0be33a6ff61` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:12:59Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Tests that a short-form Classic Records catalog string resolves via exact match to the specific Classic Records label variant rather than falling into the generic fallback path.
- found: Calls detect_pressing_info on an album title with a parenthetical "(Classic)" tag and asserts it resolves to the exact pressing_info string "Classic Records Reissue LP 24-96" and sets is_audiophile to true.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: I guessed the general shape (short form resolving to Classic Records) but not the actual API (detect_pressing_info on album title text) or the specific asserted output string/is_audiophile flag.

### `test_acoustic_sounds` — QUIRKY
- spec 3 · read at `1e0ac5fa231d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:00:00Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Tests that a known Acoustic Sounds identifier/catalog string is recognized and matched to the exact Acoustic Sounds label variant/enum value, distinct from the fallback-matching behavior covered by test_acoustic_sounds_fallback.
- found: Calls detect_pressing_info on an album title string containing \"(Acoustic Sounds)\" and asserts it produces a formatted pressing_info string \"Acoustic Sounds Reissue LP 24-96\" and sets is_audiophile true.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `test_acoustic_sounds_fallback`
- spec 3 · read at `230e0a929ab4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:44:33Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Tests that an unrecognized/unlisted variant string containing "Acoustic Sounds" (e.g. an edition or catalog suffix not explicitly enumerated) still falls back to matching the "Acoustic Sounds" label via prefix/fuzzy matching, rather than returning no match — the fallback counterpart to test_acoustic_sounds (the exact-match case).
- found: Calls detect_pressing_info with an album string containing "Acoustic Sounds Series" (not an exact catalog match) and asserts it falls back to recognizing it as an "Acoustic Sounds Reissue LP 24-96" pressing, with is_audiophile true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_mov` — QUIRKY
- spec 3 · read at `2957c76397ff` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:17:58Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A short test asserting that the label-matching function recognizes the abbreviated "MOV" prefix/catalog form and maps it to "Music On Vinyl", complementing the sibling test_music_on_vinyl_full which likely tests the full-name variant. It probably calls the lookup function with a short catalog string and asserts the returned label equals "Music On Vinyl".
- found: Test asserting detect_pressing_info() on an album title containing "(MOV)" returns pressing_info "Music On Vinyl Reissue LP 24-96" and sets is_audiophile true.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `test_music_on_vinyl_full` — QUIRKY
- spec 3 · read at `1b3104a8aa45` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:05:00Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Calls the label-matching function with the exact full string "Music on Vinyl" and asserts it returns the specific MusicOnVinyl label variant/identifier, confirming exact matches are recognized directly rather than via a fallback/prefix heuristic.
- found: Calls detect_pressing_info with an album title containing "(Music On Vinyl)" and asserts it returns a formatted pressing_info string "Music On Vinyl Reissue LP 24-96" and sets is_audiophile true.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `test_bn_classic` — QUIRKY
- spec 3 · read at `a586576cbb0f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:23:00Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Calls the label lookup function with a sample "BN" (Blue Note) catalog code representative of the classic era, and asserts the returned label match equals the expected "Blue Note" label result (exact match, not a fallback).
- found: Calls detect_pressing_info on an album title string containing "(BN Classic)" and asserts it returns the pressing_info string "Blue Note Classic Reissue LP 24-96" with is_audiophile true.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `test_bn_unknown_variant_fallback`
- spec 3 · read at `d3651dc92211` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:38:31Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This test checks that parsing/matching a Blue Note (BN) label string with a variant suffix the code doesn't explicitly recognize still falls back to a generic "Blue Note" match rather than returning no match or an error. It likely calls a labeling/detection function with a fabricated or unusual BN label string and asserts the returned label/enum variant equals the generic Blue Note fallback.
- found: Calls detect_pressing_info with an album title containing "BN Selects" and asserts it falls back to "Blue Note Classic Reissue LP 24-96" with is_audiophile true, confirming prefix-based fallback matching for unrecognized BN variants.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_csm_rl`
- spec 3 · read at `60fcc22d0c8e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:21:29Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Asserts that parsing a sample vinyl-label string containing a "CSM" prefix and an "RL" mastering-variant marker resolves to the expected recognized label/variant, via an assert_eq! comparing the label-parsing function's output against a specific expected label value — analogous to the sibling test_csm_wally test for a different variant of the same label family.
- found: Calls detect_pressing_info on "Album (CSM RL)" and asserts the resulting pressing_info string equals the specific formatted string "US CSM/RL-Mastered LP 24-96" — confirming both the label/variant detection and the exact human-readable formatting (country, format, sample rate) it produces.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: Got the assert-equality-against-expected-parse-result shape right, but the function under test is detect_pressing_info (not a bare label parser) and the assertion checks a fully formatted descriptive string (country, LP, 24-96 sample rate) rather than a simple label enum/variant value.

### `test_csm_wally`
- spec 3 · read at `a1e4dbbd13da` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:47:00Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A short test asserting that a sample matrix/runout string containing a "CSM" stamp with the "WALLY" mastering variant is correctly classified/mapped to its expected label string by the label lookup function, mirroring the sibling test_csm_rl test for a different CSM variant.
- found: Calls detect_pressing_info on an album title tagged "(CSM Wally)" and asserts it returns the pressing_info string "US CSM/Wally-Mastered LP 24-96", confirming the CSM/Wally mastering tag is parsed into a specific formatted description including a fixed catalog resolution suffix.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The exact output format (fixed "24-96" resolution suffix baked into the fallback string) isn't derivable from the test name alone — it reflects a hardcoded reference-data convention.

### `test_wlp_mono`
- spec 3 · read at `9ab2e4dc32b9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:17:10Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A short test asserting that a label-detection/lookup function correctly identifies a "WLP" (likely a record label or pressing abbreviation) mono variant from some input string, checking that the returned label/catalog metadata matches expected values (e.g., label name and "mono" attribute).
- found: Calls detect_pressing_info on a title string containing "WLP Mono" and asserts the resulting pressing_info string is "US White-Label Promo Mono LP 24-96".
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `test_sweden` — QUIRKY
- spec 3 · read at `443130096733` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:27:51Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A small unit test asserting that a matrix/runout code string identifying a Swedish pressing plant resolves via the label-lookup function to the expected label/country identifier, mirroring sibling tests like test_finland and test_teldec.
- found: Unit test: calls detect_pressing_info("Album (Sweden)", None) and asserts the returned pressing_info field equals "Sweden First-Press LP 24-96" — a lookup from an album title/edition string (not a matrix code) to a canned pressing description.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `test_finland` — QUIRKY
- spec 3 · read at `511776fa9901` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:33:07Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This is a unit test verifying that a vinyl record label/pressing string related to Finland is correctly parsed/classified by the hexload label-matching logic, likely asserting that a specific input string maps to an expected enum variant or fallback classification, similar to sibling tests like test_sweden and test_high_roller.
- found: Asserts that detect_pressing_info parses an album title containing "(Finland)" and produces a specific pressing_info string "Finland First-Press LP 24-96", presumably a hardcoded/derived label for that country tag rather than a general parenthetical passthrough.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `test_teldec` — QUIRKY
- spec 3 · read at `49ae6f5a1fc2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:47:25Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A short test that calls a label-lookup function with a sample Teldec catalog/matrix code string and asserts it resolves to the canonical label name "Teldec", analogous to the other test_<label> siblings verifying label recognition from hex/catalog identifiers.
- found: Calls detect_pressing_info with an album title containing "(Teldec)" and asserts the resulting pressing_info string is "West German Teldec First-Press LP 24-96".
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `test_high_roller` — QUIRKY
- spec 3 · read at `68b6ff765c4d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:13:40Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Tests that a specific hex/catalog code (or label string) resolves to the "High Roller" reissue label via the hexload label lookup function, asserting the returned label matches "High Roller" (possibly with a variant/fallback check similar to sibling tests).
- found: Calls detect_pressing_info with an album title containing "(High Roller)" and asserts the resulting pressing_info string is "High Roller Records Reissue LP 24-96" — testing label-name-to-pressing-description detection from album title text, not a hex code lookup.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
