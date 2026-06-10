#![forbid(unsafe_code)]

use std::env;

use dvda_phase1::tui::dvda::{parse_dvda_volume, DirectoryDvdaVolume};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = env::args().nth(1).ok_or("usage: dvda_list_groups <dvd-audio-root-or-AUDIO_TS>")?;
    let volume = DirectoryDvdaVolume::new(root);
    let disc = parse_dvda_volume(&volume)?;

    println!("audio title sets: {}", disc.title_sets.len());
    println!("titles: {}", disc.title_count());
    println!("tracks from ATSI: {}", disc.track_count_from_atsi());
    println!("CPPM detected: {}", disc.copy_protection.cppm_detected);

    for group in disc.groups {
        println!("group {} ({:?})", group.group_nr, group.correlation);
        for title_ref in group.title_refs {
            println!("  ATS_{:02} title {}", title_ref.title_set_nr, title_ref.title_nr);
        }
        for samg in group.samg_tracks {
            println!("  SAMG track {}.{} ordinal {}", samg.group_nr, samg.track_nr, samg.samg_ordinal);
        }
    }

    Ok(())
}
