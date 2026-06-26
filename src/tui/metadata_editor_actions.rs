//! Target-specific metadata-editor actions shared by reducers and input layers.

use tokio::sync::mpsc;

use super::app::{AppState, MetadataEditorState};
use super::message::AppMessage;

pub(crate) fn dispatch_artwork_write(
    app: &mut AppState,
    state: &mut Box<MetadataEditorState>,
    image_path: std::path::PathBuf,
    picture_type: lofty::picture::PictureType,
    tx: &mpsc::Sender<AppMessage>,
) {
    if state.artwork_write.is_some() {
        app.set_status("metadata editor: artwork write already running");
        return;
    }
    let paths = state.active_surface().paths.clone();
    if paths.is_empty() {
        app.set_status("metadata editor: no files available for artwork write");
        return;
    }
    let (session_id, generation) = state.begin_artwork_write(
        crate::tui::app::MetadataArtworkWriteMode::Write,
        paths.len(),
    );
    state.file_picker = None;
    let tx = tx.clone();
    tokio::spawn(async move {
        let result_paths = paths.clone();
        let result = tokio::task::spawn_blocking(move || {
            super::probe::write_artwork_to_files(&paths, &image_path, picture_type)
        })
        .await
        .unwrap_or_else(|err| Err(format!("artwork write task failed: {err}")));
        let _ = tx
            .send(AppMessage::MetadataEditorArtworkWriteComplete {
                session_id,
                generation,
                mode: crate::tui::app::MetadataArtworkWriteMode::Write,
                paths: result_paths,
                result,
            })
            .await;
    });
    app.set_status("metadata editor: writing artwork...");
}
