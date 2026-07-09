//! Target-specific metadata-editor actions shared by reducers and input layers.

use tokio::sync::mpsc;

use super::app::{AppState, MetadataEditorState};
use super::message::AppMessage;

#[cfg(test)]
pub(crate) mod test_probe {
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct ArtworkDispatch {
        pub image_path: PathBuf,
        pub picture_type: lofty::picture::PictureType,
        pub target_paths: Vec<PathBuf>,
    }

    static ARTWORK_DISPATCHES: OnceLock<Mutex<Vec<ArtworkDispatch>>> = OnceLock::new();

    fn dispatches() -> &'static Mutex<Vec<ArtworkDispatch>> {
        ARTWORK_DISPATCHES.get_or_init(|| Mutex::new(Vec::new()))
    }

    pub(crate) fn clear_artwork_dispatches() {
        dispatches().lock().expect("dispatch probe lock").clear();
    }

    pub(crate) fn record_artwork_dispatch(
        image_path: PathBuf,
        picture_type: lofty::picture::PictureType,
        target_paths: Vec<PathBuf>,
    ) {
        dispatches()
            .lock()
            .expect("dispatch probe lock")
            .push(ArtworkDispatch { image_path, picture_type, target_paths });
    }

    pub(crate) fn last_artwork_dispatch() -> Option<ArtworkDispatch> {
        dispatches().lock().expect("dispatch probe lock").last().cloned()
    }
}

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
    #[cfg(test)]
    {
        test_probe::record_artwork_dispatch(image_path, picture_type, paths);
        state.file_picker = None;
        app.set_status("metadata editor: writing artwork...");
        let _ = tx;
        return;
    }

    #[cfg(not(test))]
    {
        let (session_id, generation, cancel) = state.begin_cancellable_artwork_write(
            crate::tui::app::MetadataArtworkWriteMode::Write,
            paths.len(),
        );
        state.file_picker = None;
        let tx = tx.clone();
        tokio::spawn(async move {
            let result_paths = paths.clone();
            let result = tokio::task::spawn_blocking(move || {
                super::probe::write_artwork_to_files_with_cancel(&paths, &image_path, picture_type, Some(&cancel))
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
}
