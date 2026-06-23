//! Ignored Blu-ray backend smoke test.
//!
//! Run with:
//!   BLURAY_ISO=/path/to/disc.iso BLURAY_DIR=/path/to/disc/root \
//!     cargo test --test bluray_backend_smoke -- --ignored --nocapture

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use tonepoet::disc::bluray_backend::{
    BluRayAudioCoding, BluRayAudioStreamKind, BlurayBackend, BlurayBackendCapability,
    BlurayLpcmBitDepth,
};
use tonepoet::disc::bluray_backend_libbluray::BlurayBackendLibbluray;

#[test]
#[ignore = "requires local unencrypted Blu-ray ISO and BDMV directory fixtures"]
fn libbluray_opens_iso_and_bdmv_directory() {
    let iso = std::env::var("BLURAY_ISO").expect("BLURAY_ISO must point to a Blu-ray ISO");
    let dir = std::env::var("BLURAY_DIR").expect("BLURAY_DIR must point to a Blu-ray disc root");

    smoke_one(Path::new(&iso));
    smoke_one(Path::new(&dir));
}

fn smoke_one(path: &Path) {
    let disc = BlurayBackendLibbluray::open(path)
        .unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));
    let titles = BlurayBackendLibbluray::titles(&disc).expect("enumerate Blu-ray titles");
    assert!(!titles.is_empty(), "{} has no Blu-ray titles", path.display());

    println!("{}: {} titles", path.display(), titles.len());
    assert_title_source_cursor_survives_metadata_queries(&disc, &titles);

    for title in titles.iter().take(8) {
        println!(
            "playlist {:05} title_index={} duration={:.2}s angles={} chapters={} clips={}",
            title.playlist_number,
            title.key.title_index(),
            title.duration_secs(),
            title.angle_count,
            title.chapter_count,
            title.clip_count
        );

        let chapters = BlurayBackendLibbluray::chapters(&disc, title.key, 0)
            .expect("enumerate Blu-ray chapters");
        assert_eq!(chapters.len() as u32, title.chapter_count);

        let streams = BlurayBackendLibbluray::streams(&disc, title.key)
            .expect("enumerate Blu-ray audio streams");
        for stream in streams {
            println!(
                concat!(
                    "  {} stream={} pid=0x{:04x} codec={} lang={} rate={:?} ",
                    "depth={:?} channels={:?} layout={:?}"
                ),
                stream.kind.label(),
                stream.stream_index + 1,
                stream.pid,
                stream.coding.label(),
                stream.language.as_deref().unwrap_or("und"),
                stream.sample_rate,
                stream.bit_depth,
                stream.channels,
                stream.channel_layout,
            );

            if stream.kind == BluRayAudioStreamKind::Primary
                && stream.coding == BluRayAudioCoding::Lpcm
            {
                assert!(
                    matches!(
                        &stream.bit_depth,
                        BlurayLpcmBitDepth::Probed { .. }
                            | BlurayLpcmBitDepth::ProbeFailed { .. }
                            | BlurayLpcmBitDepth::NotProbed { .. }
                    ),
                    "primary LPCM stream pid 0x{:04x} did not report structured bit-depth status",
                    stream.pid
                );
            }
            if stream.kind == BluRayAudioStreamKind::Secondary
                && stream.coding == BluRayAudioCoding::Lpcm
            {
                assert!(
                    matches!(&stream.bit_depth, BlurayLpcmBitDepth::NotProbed { .. }),
                    "secondary LPCM stream pid 0x{:04x} should report NotProbed",
                    stream.pid
                );
            }
        }
    }
}


fn assert_title_source_cursor_survives_metadata_queries(
    disc: &<BlurayBackendLibbluray as BlurayBackend>::Disc,
    titles: &[tonepoet::disc::bluray_backend::BlurayTitleInfo],
) {
    let title = titles[0].key;
    let other_title = titles.get(1).map(|info| info.key).unwrap_or(title);
    let mut source = BlurayBackendLibbluray::open_title(disc, title, 0, None)
        .expect("open independent Blu-ray title source");

    let mut warmup = [0u8; 188];
    source.read_exact(&mut warmup).expect("read initial TS packet");

    let cursor = source
        .seek(SeekFrom::Current(0))
        .expect("query title source position");
    let mut expected = [0u8; 188];
    source
        .read_exact(&mut expected)
        .expect("read comparison TS packet");
    source
        .seek(SeekFrom::Start(cursor))
        .expect("rewind to comparison point");

    let _ = BlurayBackendLibbluray::streams(disc, other_title)
        .expect("metadata stream query must not perturb an open title source");

    let pts = BlurayBackendLibbluray::pts_continuity_segments(&source)
        .expect("query PTS continuity capability");
    assert!(
        matches!(pts, BlurayBackendCapability::Unsupported { .. }),
        "libbluray Phase 0 should report unsupported PTS continuity explicitly"
    );

    let mut actual = [0u8; 188];
    source
        .read_exact(&mut actual)
        .expect("read comparison TS packet after metadata query");
    assert_eq!(actual, expected);
}

#[test]
#[ignore = "requires a local fixture that makes libbluray queue a fatal/read/encrypted event"]
fn libbluray_surfaces_real_event_errors_from_fixture() {
    let fixture = std::env::var("BLURAY_EVENT_FIXTURE")
        .expect("BLURAY_EVENT_FIXTURE must point to an encrypted or damaged Blu-ray fixture");
    let expected = std::env::var("BLURAY_EVENT_EXPECT").unwrap_or_else(|_| "any".to_string());
    let read_limit = std::env::var("BLURAY_EVENT_READ_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(64 * 1024 * 1024);

    let error = first_event_error_from_fixture(Path::new(&fixture), read_limit).unwrap_or_else(|| {
        panic!(
            "{} did not trigger a libbluray fatal/read/encrypted event within {} bytes",
            fixture, read_limit
        )
    });
    assert_real_event_error_matches(&error, &expected);
}

fn first_event_error_from_fixture(path: &Path, read_limit: usize) -> Option<String> {
    let disc = match BlurayBackendLibbluray::open(path) {
        Ok(disc) => disc,
        Err(err) => return Some(err),
    };
    let titles = match BlurayBackendLibbluray::titles(&disc) {
        Ok(titles) => titles,
        Err(err) => return Some(err),
    };

    for title in titles.iter().take(8) {
        let mut source = match BlurayBackendLibbluray::open_title(&disc, title.key, 0, None) {
            Ok(source) => source,
            Err(err) => return Some(err),
        };
        if let Err(err) = source.seek(SeekFrom::Start(0)) {
            return Some(err.to_string());
        }

        let mut total = 0usize;
        let mut buffer = vec![0u8; 1024 * 1024];
        while total < read_limit {
            let request = buffer.len().min(read_limit - total);
            match source.read(&mut buffer[..request]) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(err) => return Some(err.to_string()),
            }
        }
    }

    None
}

fn assert_real_event_error_matches(error: &str, expected: &str) {
    let upper = error.to_ascii_uppercase();
    let any_event = upper.contains("LIBBLURAY")
        || upper.contains("READ_ERROR")
        || upper.contains("ENCRYPTED")
        || upper.contains("BLU-RAY STREAM IS ENCRYPTED");
    assert!(
        any_event,
        "expected a libbluray event-derived error, got: {error}"
    );

    match expected {
        "any" => {}
        "encrypted" => assert!(
            upper.contains("ENCRYPTED") || upper.contains("PERMISSION"),
            "expected encrypted event error, got: {error}"
        ),
        "read_error" => assert!(
            upper.contains("READ_ERROR") || upper.contains("UNEXPECTED EOF"),
            "expected read-error event, got: {error}"
        ),
        "fatal" => assert!(
            upper.contains("LIBBLURAY EVENT ERROR") || upper.contains("FATAL"),
            "expected fatal libbluray event, got: {error}"
        ),
        other => panic!(
            "unsupported BLURAY_EVENT_EXPECT={other}; use any, encrypted, read_error, or fatal"
        ),
    }
}
