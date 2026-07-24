# A contract replacement: authoritative membership, not co-location

Date: 2026-07-24
Baseline: `multifile_cue_v5_robust_corrected_2026-07-24.tar.gz`

## Product-contract decision

The previous shipped A regression was materially underspecified. It required the entire folder to fail closed when one readable CUE and one malformed sibling CUE were co-located, but the fixture supplied no authoritative evidence that the malformed CUE belonged to the readable CUE's album. The filesystem state alone cannot distinguish a corrupt album member from an unrelated malformed sidecar.

The corrected contract is:

> A multi-CUE album fails closed only when the system has authoritative evidence that an unreadable or otherwise rejected CUE belongs to that album. Without authoritative grouping evidence, a malformed sibling CUE is suppressed independently, ordinary audio and unrelated valid CUE content are preserved, and the ambiguity is surfaced visibly.

This is a product-contract correction, not a convenience change made to accommodate the implementation. No filename, stem, directory-co-location, or display-message heuristic was restored.

## Exact previous A test body

```rust
    #[test]
    fn folder_expansion_fails_closed_when_merged_cue_group_cannot_be_parsed() {
        let td = tempfile::tempdir().expect("tempdir");
        let a = td.path().join("side_a.flac");
        let b = td.path().join("side_b.flac");
        let cue_a = td.path().join("side_a.cue");
        let cue_b = td.path().join("side_b.cue");
        std::fs::write(&a, b"not real flac").unwrap();
        std::fs::write(&b, b"not real flac").unwrap();
        std::fs::write(
            &cue_a,
            r#"TITLE "Album Side A"
FILE "side_a.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 01:00:00
"#,
        )
        .unwrap();
        std::fs::write(&cue_b, [0xff, 0xfe, 0x00]).unwrap();

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert!(expanded.paths.is_empty(), "parse failure must not fall back to side CUEs or raw images: {:?}", expanded.paths);
        assert!(
            expanded.expansion_errors.iter().any(|err| {
                err.contains("failed to parse")
                    || err.contains("failed to analyze")
                    || err.contains("failed to decode")
                    || err.contains("member CUE invalid")
            }),
            "expected a parse/analyze/decode error, got {:?}",
            expanded.expansion_errors
        );
    }
```

## Replacement A1 body: proven merged group fails closed

```rust

    #[test]
    fn folder_expansion_a1_proven_merged_group_fails_closed_after_member_becomes_unparseable() {
        let td = tempfile::tempdir().expect("tempdir");
        let side_a = td.path().join("side_a.flac");
        let side_b = td.path().join("side_b.flac");
        let cue_a = td.path().join("side_a.cue");
        let cue_b = td.path().join("disc2.cue");
        let unrelated_audio = td.path().join("bonus.flac");
        let unrelated_cue = td.path().join("bonus.cue");
        let plain_audio = td.path().join("interview.flac");
        for audio in [&side_a, &side_b, &unrelated_audio, &plain_audio] {
            std::fs::write(audio, b"not real flac").expect("audio fixture");
        }
        std::fs::write(
            &cue_a,
            "TITLE \"Album Side A\"\nFILE \"side_a.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
        )
        .expect("side A cue");
        std::fs::write(
            &cue_b,
            "TITLE \"Album Side B\"\nFILE \"side_b.flac\" WAVE\n  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n  TRACK 04 AUDIO\n    INDEX 01 03:00:00\n",
        )
        .expect("side B cue");
        std::fs::write(
            &unrelated_cue,
            "TITLE \"Bonus Disc\"\nFILE \"bonus.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 02:00:00\n",
        )
        .expect("unrelated cue");

        let cue_paths = vec![cue_a.clone(), cue_b.clone()];
        let decision = crate::convert::split_cue_album::merge_decision(
            &cue_paths,
            SplitCueAlbumGroupingReason::TitleSharedPrefix,
        )
        .with_current_member_provenance();
        let mut decisions = QueueSplitCueAlbumGroupingDecisions::new();
        decisions.insert(split_cue_album_grouping_key_for_queue(&cue_paths), decision);

        let mut corrupt = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&cue_b)
            .expect("open proven member in place");
        std::io::Write::write_all(&mut corrupt, &[0xff, 0xfe, 0x00])
            .expect("make proven member unparseable without replacing its file object");
        drop(corrupt);

        let selection = select_split_cue_folder_members(
            &[cue_a.clone(), cue_b.clone(), unrelated_cue.clone()],
            None,
        );
        let SplitCueFolderSelection::Selected { rejected, .. } = selection else {
            panic!("fixture must retain valid candidates while rejecting the corrupt member");
        };
        assert!(rejected.iter().any(|rejection| {
            queue_path_key(&rejection.cue_path) == queue_path_key(&cue_b)
                && matches!(
                    rejection.reason,
                    crate::convert::split_cue_album::SplitCueMemberRejectionReason::ParseFailure { .. }
                )
        }));

        let expanded = expand_paths_to_audio_with_metadata_using_grouping_decisions(
            &[td.path().to_path_buf()],
            &decisions,
        );
        for proven_member in [&cue_a, &cue_b, &side_a, &side_b] {
            assert!(
                !path_list_contains(&expanded.paths, proven_member),
                "proven failed group member leaked into queue: {}",
                proven_member.display(),
            );
        }
        assert!(path_list_contains(&expanded.paths, &unrelated_cue));
        assert!(!path_list_contains(&expanded.paths, &unrelated_audio));
        assert!(path_list_contains(&expanded.paths, &plain_audio));
        assert!(!expanded.paths.is_empty(), "unrelated folder content must survive");
        assert!(expanded.expansion_errors.iter().any(|error| {
            error.contains("Cannot queue merged CUE album")
                && error.contains("member CUE invalid")
        }));
    }
```

A1 first creates both readable CUE members and captures the shared grouping decision's validated file-object and parsed-membership provenance. It then corrupts one member in place so object identity remains stable while parsing fails. The assertions prove that only the established group is failed closed, unrelated valid CUE content and ordinary audio survive, and the typed rejection is `ParseFailure` before it is rendered for the queue warning.

## Replacement A2 body: unknown relationship preserves the folder

```rust

    #[test]
    fn folder_expansion_a2_unknown_malformed_siblings_preserve_valid_album_and_ordinary_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let album_audio = td.path().join("album.flac");
        let album_cue = td.path().join("album.cue");
        let same_stem_audio = td.path().join("bonus.flac");
        let same_stem_bad_cue = td.path().join("bonus.cue");
        let differently_named_audio = td.path().join("side_b.flac");
        let differently_named_bad_cue = td.path().join("disc2.cue");
        for audio in [&album_audio, &same_stem_audio, &differently_named_audio] {
            std::fs::write(audio, b"not real flac").expect("audio fixture");
        }
        std::fs::write(
            &album_cue,
            "TITLE \"Main Album\"\nFILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
        )
        .expect("valid cue");
        std::fs::write(&same_stem_bad_cue, [0xff, 0xfe, 0x00]).expect("same-stem bad cue");
        std::fs::write(&differently_named_bad_cue, [0xff, 0xfe, 0x00])
            .expect("differently-named bad cue");

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded.paths, &album_cue));
        assert!(!path_list_contains(&expanded.paths, &album_audio));
        assert!(path_list_contains(&expanded.paths, &same_stem_audio));
        assert!(path_list_contains(&expanded.paths, &differently_named_audio));
        assert!(!path_list_contains(&expanded.paths, &same_stem_bad_cue));
        assert!(!path_list_contains(&expanded.paths, &differently_named_bad_cue));
        assert!(!expanded.paths.is_empty());
        assert!(!expanded
            .expansion_errors
            .iter()
            .any(|error| error.contains("Cannot queue merged CUE album")));
        for malformed_cue in [&same_stem_bad_cue, &differently_named_bad_cue] {
            let name = malformed_cue
                .file_name()
                .and_then(|value| value.to_str())
                .expect("fixture CUE filename");
            assert!(expanded.expansion_errors.iter().any(|error| {
                error.contains("Suppressed unusable CUE")
                    && error.contains(name)
                    && error.contains("No current authoritative album-group membership")
            }));
        }
    }
```

A2 supplies no grouping provenance. It covers both a malformed CUE with same-stem audio and a differently named malformed CUE beside ordinary audio. The assertions prove that stems do not establish membership, malformed CUEs are suppressed independently, the valid CUE remains queueable, ordinary audio remains queueable, and visible no-authority warnings are emitted.

## Test replacement accounting

Removed names:

- `convert::queue_expansion::tests::folder_expansion_fails_closed_when_merged_cue_group_cannot_be_parsed`
- `convert::queue_expansion::tests::unrelated_malformed_cue_does_not_fail_a_valid_album_closed`

Replacement names:

- `convert::queue_expansion::tests::folder_expansion_a1_proven_merged_group_fails_closed_after_member_becomes_unparseable`
- `convert::queue_expansion::tests::folder_expansion_a2_unknown_malformed_siblings_preserve_valid_album_and_ordinary_audio`

The second removed test's unknown-membership behavior is subsumed and strengthened by A2's simultaneous same-stem and nonmatching-stem cases.
