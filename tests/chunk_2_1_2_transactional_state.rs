use std::fs;
use std::path::PathBuf;

use tonepoet::convert::pipeline::transactional_state::{
    begin_track_output, delete_stale_transactional_track_states, is_transactional_state_file,
    mark_track_validated, materialize_validated_final, transactional_track_paths, PARTIAL_SUFFIX,
    VALIDATED_SUFFIX,
};

#[test]
fn transactional_state_paths_use_tonepoet_owned_suffixes() {
    let paths = transactional_track_paths(PathBuf::from("stage/01.flac"));
    assert_eq!(paths.partial_path, PathBuf::from(format!("stage/01.flac{PARTIAL_SUFFIX}")));
    assert_eq!(paths.validated_path, PathBuf::from(format!("stage/01.flac{VALIDATED_SUFFIX}")));
    assert_eq!(paths.final_staged_path, PathBuf::from("stage/01.flac"));
}

#[test]
fn partial_validated_final_state_machine_hides_incomplete_output() {
    let temp = tempfile::tempdir().unwrap();
    let final_path = temp.path().join("stage").join("01.flac");
    let paths = begin_track_output(&final_path).unwrap();

    fs::write(&paths.partial_path, b"complete encoded audio").unwrap();
    assert!(paths.partial_path.exists());
    assert!(!paths.final_staged_path.exists());

    mark_track_validated(&paths).unwrap();
    assert!(!paths.partial_path.exists());
    assert!(paths.validated_path.exists());
    assert!(!paths.final_staged_path.exists());

    materialize_validated_final(&paths).unwrap();
    assert!(!paths.validated_path.exists());
    assert!(paths.final_staged_path.exists());
    assert_eq!(fs::read(&paths.final_staged_path).unwrap(), b"complete encoded audio");
}

#[test]
fn cleanup_deletes_only_known_tonepoet_state_paths() {
    let temp = tempfile::tempdir().unwrap();
    let final_path = temp.path().join("stage").join("01.flac");
    let paths = transactional_track_paths(&final_path);
    fs::create_dir_all(final_path.parent().unwrap()).unwrap();
    fs::write(&paths.partial_path, b"old partial").unwrap();
    fs::write(&paths.validated_path, b"old validated").unwrap();

    let user_partial = temp.path().join("stage").join("notes.partial");
    let user_validated = temp.path().join("stage").join("take.validated");
    fs::write(&user_partial, b"user file").unwrap();
    fs::write(&user_validated, b"user file").unwrap();

    let deleted = delete_stale_transactional_track_states([final_path]).unwrap();
    assert_eq!(deleted.len(), 2);
    assert!(!paths.partial_path.exists());
    assert!(!paths.validated_path.exists());
    assert!(user_partial.exists());
    assert!(user_validated.exists());
}

#[test]
fn state_file_detection_matches_only_tonepoet_owned_suffixes() {
    assert!(is_transactional_state_file(PathBuf::from(format!("01.flac{PARTIAL_SUFFIX}")).as_path()));
    assert!(is_transactional_state_file(PathBuf::from(format!("01.flac{VALIDATED_SUFFIX}")).as_path()));
    assert!(!is_transactional_state_file(PathBuf::from("notes.partial").as_path()));
    assert!(!is_transactional_state_file(PathBuf::from("take.validated").as_path()));
}
