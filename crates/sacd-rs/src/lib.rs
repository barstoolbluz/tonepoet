//! Pure-Rust SACD ISO audio extraction.
//!
//! Extracts DSD audio streams from ScarletBook-format SACD ISO files
//! and writes them as Sony DSF, Philips DSDIFF/DSD, or Philips
//! DSDIFF/DST files. Supports both uncompressed DSD and DST-encoded
//! source frames.
//!
//! ## Scope
//!
//! This crate handles **audio extraction**: reading raw DSD frames
//! from per-track sector ranges, optionally DST-decoding them, and
//! serializing into one of the standard DSD file formats. It does
//! not parse ScarletBook metadata (master TOC, area TOCs, per-track
//! text, ISRCs, etc.) — that lives in tonepoet's `tui::sacd` module.
//! Callers pass pre-parsed metadata in.
//!
//! ## License
//!
//! GPL-2.0-or-later. This crate's DST decoder is a Rust port of the
//! DST decoder in [Sound-Linux-More/sacd-extract][upstream], which is
//! GPL-2.0. Derivative-work licensing requires GPL-2.0-or-later on
//! the port. Compatible with tonepoet's GPL-3.0-or-later top-level
//! license.
//!
//! [upstream]: https://github.com/Sound-Linux-More/sacd-extract
//!
//! ## Status
//!
//! This crate implements local SACD ISO audio extraction for both
//! uncompressed DSD and DST-encoded frames, with DSF, DSDIFF/DSD, and
//! DSDIFF/DST output, plus rate-aware DSF/DSDIFF/DST stream readers,
//! structured frame parsing, strict integrity validation, and explicit
//! damaged-ISO salvage reporting. Scarlet Book metadata parsing lives
//! in tonepoet's `tui::sacd` module, which passes parsed area state into
//! this crate's high-integrity extraction API.

pub mod asset_model;
pub mod consts;
pub mod container;
pub mod corpus;
pub mod dff_dst_writer;
pub mod dff_footer;
pub mod dff_writer;
pub mod dsd_file;
pub mod dsf_writer;
pub mod dst;
pub mod extract;
pub mod frame;
pub mod id3;
pub mod iso_reader;
pub mod output_transaction;
pub mod source_model;
pub mod stream_ops;
pub mod stream_reader;

#[cfg(test)]
pub(crate) mod test_allocation_counter {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) struct CountingAllocator;

    static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);

    thread_local! {
        static COUNT_ALLOCATIONS_ON_THREAD: Cell<bool> = Cell::new(false);
    }

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            COUNT_ALLOCATIONS_ON_THREAD.with(|enabled| {
                if enabled.get() {
                    ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
                }
            });
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            COUNT_ALLOCATIONS_ON_THREAD.with(|enabled| {
                if enabled.get() {
                    ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
                }
            });
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            COUNT_ALLOCATIONS_ON_THREAD.with(|enabled| {
                if enabled.get() {
                    ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
                }
            });
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    struct AllocationCounterGuard;

    impl Drop for AllocationCounterGuard {
        fn drop(&mut self) {
            COUNT_ALLOCATIONS_ON_THREAD.with(|enabled| enabled.set(false));
        }
    }

    pub(crate) fn allocation_count_for<T>(f: impl FnOnce() -> T) -> (T, usize) {
        COUNT_ALLOCATIONS_ON_THREAD.with(|enabled| enabled.set(true));
        let _guard = AllocationCounterGuard;
        ALLOCATION_COUNT.store(0, Ordering::SeqCst);
        let result = f();
        let allocations = ALLOCATION_COUNT.load(Ordering::SeqCst);
        (result, allocations)
    }
}

#[cfg(test)]
#[global_allocator]
static TEST_GLOBAL_ALLOCATOR: test_allocation_counter::CountingAllocator = test_allocation_counter::CountingAllocator;

pub use dsd_file::{
    describe_container, drain_decoded_dsd_source, inspect_dsd_container, inspect_dsdiff,
    inspect_dsf, open_dsd_as_decoded_reader, open_dsd_asset, open_dsd_file,
    open_dsd_file_with_policies, open_dsd_source, report_has_decoded_dst_coverage,
    validate_dsd_corpus_paths, validate_dsd_stream, write_decoded_dsd_to_dff,
    write_decoded_dsd_to_dsf, CommonDsdSourceKind, DecodedDsdSource, DsdAsset,
    DsdAssetError, DsdAssetInfo, DsdAssetKind, DsdAssetMetadata, DsdAssetProvenance,
    DsdAudioStreamInfo, DsdByteOrder, DsdChannelFrame, DsdCompression,
    DsdContainerDiagnostic, DsdContainerDiagnosticSeverity, DsdContainerError,
    DsdContainerFormat, DsdContainerInfo, DsdCorpusAcceptanceFailure,
    DsdCorpusEntryReport, DsdCorpusValidationOptions, DsdCorpusValidationReport,
    DsdDecodedFileReader, DsdFileAsset, DsdFileMetadata, DsdFileReadPolicies,
    DsdFileReader, DsdFileSource, DsdFrame, DsdFrameReader, DsdFrameSeek,
    DsdReadError, DsdSource, DsdSourceDrainStats, DsdSourceError, DsdSourceFrame,
    DsdSourceInfo, DsdSourceSeek, DsdStreamCopyStats, DsdValidationFailure,
    DsdValidationFailureKind, DsdValidationMode, DsdValidationOptions,
    DsdValidationReport, DsdiffDsdReader, DsdiffDstReader, DsdiffIndexValidationPolicy,
    DsdiffInspector, DsfInspector, DsfReader, DsfStreamReader, DsdDsdiffStreamReader,
    DstCrcStatus, DstCrcValidationPolicy, DstDsdiffStreamReader, DstFrame,
    DstFrameReader, DstToDsdAdapter, IsoTrackRange, IsoTrackSource, IsoTrackSourceOptions,
    SacdIsoTrackAsset, SourceDsdFrame, SourceDstFrame, SourceToDsdAdapter,
    ValidationDsdSourceKind,
};

pub use extract::{
    extract_track, extract_track_to_path, extract_track_to_path_with_dst_options,
    extract_track_with_dst_options, extract_track_with_integrity_and_dst_options,
    extract_track_with_integrity_options, write_dsd_source, write_dsd_source_to_path,
    DsdSourceExtractOptions, DstExtractionOptions, ExtractError, ExtractIntegrityOptions,
    ExtractIntegrityReport, ExtractOptions, ExtractReport, ExtractStats, ExtractToPathError,
    OutputFormat, PlainDsdDstHandling, SourceDstHandling, TimeFilter,
};

pub use frame::FrameFormat;
pub use output_transaction::{OutputOverwritePolicy, OutputTransaction, OutputTransactionError};

#[cfg(test)]
mod test_util;
