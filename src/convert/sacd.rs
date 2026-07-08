//! Conversion-domain SACD ISO probe used by queue admission.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SECTOR_SIZE: u64 = 2048;
const MASTER_TOC_LSNS: [u64; 3] = [510, 520, 530];
const MASTER_TOC_MAGIC: &[u8] = b"SACDMTOC";

/// Return true when `path` is an SACD ISO by ScarletBook master-TOC magic.
///
/// The extension check is intentionally cheap and mirrors the existing TUI
/// lazy-classification contract: generic ISOs stay archive candidates unless
/// a disc-specific probe promotes them.
pub fn is_sacd_iso(path: &Path) -> bool {
    let is_iso = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("iso"))
        .unwrap_or(false);
    if !is_iso {
        return false;
    }

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };

    let mut magic = vec![0u8; MASTER_TOC_MAGIC.len()];
    for lsn in MASTER_TOC_LSNS {
        if file.seek(SeekFrom::Start(lsn * SECTOR_SIZE)).is_err() {
            continue;
        }
        if file.read_exact(&mut magic).is_ok() && magic == MASTER_TOC_MAGIC {
            return true;
        }
    }

    false
}
