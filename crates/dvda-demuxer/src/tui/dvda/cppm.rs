#![forbid(unsafe_code)]

use std::io::Read;

use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::model::{CopyProtectionSource, DvdaDiagnostic, DvdaDisc, DVD_BLOCK_SIZE};
use crate::tui::dvda::volume::DvdaVolume;

const DVD_SECTOR_SIZE: usize = DVD_BLOCK_SIZE as usize;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AobMpegPsProbe {
    file_name: String,
    first_four: [u8; 4],
    pack_header_valid: bool,
    pes_packets: usize,
    private_stream_1_packets: usize,
    dvd_audio_substreams: usize,
    reason: String,
}

impl AobMpegPsProbe {
    fn audio_data_looks_readable(&self) -> bool {
        self.pack_header_valid
            && (self.dvd_audio_substreams > 0
                || (self.private_stream_1_packets == 0 && self.pes_packets > 0))
    }

    fn hex_prefix(&self) -> String {
        self.first_four
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join("")
    }
}

pub fn refine_copy_protection_from_aob_probe<V: DvdaVolume + ?Sized>(
    volume: &V,
    disc: &mut DvdaDisc,
    assume_decrypted: bool,
) -> Result<()> {
    if assume_decrypted {
        if disc.copy_protection.mkb_present || disc.copy_protection.cppm_detected {
            disc.copy_protection.cppm_detected = false;
            disc.copy_protection.source = CopyProtectionSource::AssumeDecryptedOverride;
            disc.diagnostics.push(DvdaDiagnostic::warn(
                "dvda_cppm_assume_decrypted",
                "DVD-Audio caller override declared AOB sectors already decrypted; CPPM blocking disabled for this source",
            ));
        }
        return Ok(());
    }

    if !disc.copy_protection.mkb_present {
        return Ok(());
    }

    match probe_first_backed_aob_for_mpeg_ps(volume, disc)? {
        Some(probe) if probe.audio_data_looks_readable() => {
            disc.copy_protection.cppm_detected = false;
            disc.copy_protection.source = CopyProtectionSource::MkbPresentAobProbeReadable;
            disc.diagnostics.push(DvdaDiagnostic::info(
                "dvda_mkb_present_aob_probe_readable",
                format!(
                    "DVDAUDIO.MKB is present, but first backed AOB sector {} begins with {} and parses as MPEG-PS ({} PES packet(s), {} PS1 packet(s), {} DVD-A substream(s)); treating AOB data as already readable",
                    probe.file_name,
                    probe.hex_prefix(),
                    probe.pes_packets,
                    probe.private_stream_1_packets,
                    probe.dvd_audio_substreams
                ),
            ));
        }
        Some(probe) => {
            disc.copy_protection.cppm_detected = true;
            disc.copy_protection.source = CopyProtectionSource::AobProbeNoMpegPs;
            disc.diagnostics.push(DvdaDiagnostic::warn(
                "dvda_mkb_present_aob_probe_not_mpeg_ps",
                format!(
                    "DVDAUDIO.MKB is present and first backed AOB sector {} begins with {}, but did not parse as readable MPEG-PS audio data: {}",
                    probe.file_name,
                    probe.hex_prefix(),
                    probe.reason
                ),
            ));
        }
        None => {
            disc.copy_protection.cppm_detected = false;
            disc.copy_protection.source = CopyProtectionSource::MkbPresence;
            disc.diagnostics.push(DvdaDiagnostic::warn(
                "dvda_mkb_present_no_aob_probe",
                "DVDAUDIO.MKB is present, but no backed AOB file was available for readability probing; MKB metadata alone will not block extraction",
            ));
        }
    }

    Ok(())
}

fn probe_first_backed_aob_for_mpeg_ps<V: DvdaVolume + ?Sized>(
    volume: &V,
    disc: &DvdaDisc,
) -> Result<Option<AobMpegPsProbe>> {
    let Some(aob) = disc
        .title_sets
        .iter()
        .flat_map(|title_set| title_set.aobs.iter())
        .filter(|aob| aob.exists && aob.byte_len >= DVD_BLOCK_SIZE)
        .min_by_key(|aob| (aob.title_set_nr, aob.part_nr))
    else {
        return Ok(None);
    };

    let mut file = volume.open_audio_ts_file(&aob.file_name)?;
    let mut sector = [0u8; DVD_SECTOR_SIZE];
    file.read_exact(&mut sector)
        .map_err(|source| DvdaError::io(&aob.file_name, source))?;

    Ok(Some(probe_mpeg_ps_sector(&aob.file_name, &sector)))
}

fn probe_mpeg_ps_sector(file_name: &str, sector: &[u8]) -> AobMpegPsProbe {
    let mut first_four = [0u8; 4];
    let prefix_len = sector.len().min(first_four.len());
    first_four[..prefix_len].copy_from_slice(&sector[..prefix_len]);

    let invalid = |reason: String| AobMpegPsProbe {
        file_name: file_name.to_string(),
        first_four,
        pack_header_valid: false,
        pes_packets: 0,
        private_stream_1_packets: 0,
        dvd_audio_substreams: 0,
        reason,
    };

    if sector.len() < DVD_SECTOR_SIZE {
        return invalid(format!(
            "sector shorter than DVD logical block size: {} byte(s)",
            sector.len()
        ));
    }
    if sector.get(0..4) != Some(&[0x00, 0x00, 0x01, 0xBA][..]) {
        return invalid("missing MPEG-PS pack start code 000001ba".to_string());
    }

    let mut probe = AobMpegPsProbe {
        file_name: file_name.to_string(),
        first_four,
        pack_header_valid: true,
        pes_packets: 0,
        private_stream_1_packets: 0,
        dvd_audio_substreams: 0,
        reason: "MPEG-PS pack header present".to_string(),
    };

    let mut cursor = 14 + usize::from(sector[13] & 0x07);
    if cursor > sector.len() {
        probe.pack_header_valid = false;
        probe.reason = "pack stuffing extends past sector boundary".to_string();
        return probe;
    }

    while cursor + 6 <= sector.len() {
        if sector.get(cursor..cursor + 3) != Some(&[0x00, 0x00, 0x01][..]) {
            if probe.pes_packets == 0 {
                probe.reason = format!(
                    "no PES/system packet start code after pack header at byte {cursor}"
                );
            }
            break;
        }

        let stream_id = sector[cursor + 3];
        if stream_id == 0xB9 {
            break;
        }

        let packet_len = ((usize::from(sector[cursor + 4])) << 8) | usize::from(sector[cursor + 5]);
        let packet_end = match cursor.checked_add(6).and_then(|base| base.checked_add(packet_len)) {
            Some(end) => end,
            None => {
                probe.reason = format!("packet at byte {cursor} overflows usize");
                break;
            }
        };
        if packet_end > sector.len() {
            probe.reason = format!(
                "packet at byte {cursor} extends past sector boundary: end={packet_end}, sector={}",
                sector.len()
            );
            break;
        }

        probe.pes_packets += 1;
        if stream_id == 0xBD {
            probe.private_stream_1_packets += 1;
            if dvd_audio_substream_header_is_visible(&sector[cursor..packet_end]) {
                probe.dvd_audio_substreams += 1;
            }
        }

        cursor = packet_end;
    }

    if probe.audio_data_looks_readable() {
        probe.reason = format!(
            "readable MPEG-PS audio data found: {} PES packet(s), {} PS1 packet(s), {} DVD-A substream(s)",
            probe.pes_packets, probe.private_stream_1_packets, probe.dvd_audio_substreams
        );
    } else if probe.pes_packets > 0 {
        probe.reason = format!(
            "MPEG-PS pack present, but no readable DVD-A private stream was found: {} PES packet(s), {} PS1 packet(s), {} DVD-A substream(s)",
            probe.pes_packets, probe.private_stream_1_packets, probe.dvd_audio_substreams
        );
    }

    probe
}

fn dvd_audio_substream_header_is_visible(packet: &[u8]) -> bool {
    if packet.len() < 10 || packet.get(0..3) != Some(&[0x00, 0x00, 0x01][..]) || packet[3] != 0xBD {
        return false;
    }
    let pes_len = ((usize::from(packet[4])) << 8) | usize::from(packet[5]);
    let pes_end = 6usize.saturating_add(pes_len).min(packet.len());
    if pes_end < 10 {
        return false;
    }
    let header_data_len = usize::from(packet[8]);
    let sub_header = match 9usize.checked_add(header_data_len) {
        Some(offset) if offset + 4 <= pes_end => offset,
        _ => return false,
    };
    matches!(packet[sub_header], 0xA0 | 0xA1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::{Cursor, Read, Seek, SeekFrom};

    use crate::tui::dvda::model::{
        AmgInfo, AmgPointers, AobFileEntry, AtsiHeader, CopyProtectionInfo, SamgInfo, TitleSet,
        TitleSetKind,
    };
    use crate::tui::dvda::volume::DvdaFile;

    struct MemoryDvdaVolume {
        files: BTreeMap<String, Vec<u8>>,
    }

    struct MemoryDvdaFile {
        cursor: Cursor<Vec<u8>>,
    }

    impl Read for MemoryDvdaFile {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.cursor.read(buf)
        }
    }

    impl Seek for MemoryDvdaFile {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.cursor.seek(pos)
        }
    }

    impl DvdaFile for MemoryDvdaFile {
        fn len(&self) -> u64 {
            self.cursor.get_ref().len() as u64
        }
    }

    impl DvdaVolume for MemoryDvdaVolume {
        fn open_audio_ts_file(&self, name: &str) -> Result<Box<dyn DvdaFile>> {
            let Some(bytes) = self.files.get(name) else {
                return Err(DvdaError::MissingFile {
                    candidates: vec![name.to_string()],
                });
            };
            Ok(Box::new(MemoryDvdaFile {
                cursor: Cursor::new(bytes.clone()),
            }))
        }
    }

    fn probe_test_mlp_sector() -> Vec<u8> {
        let mut sector = vec![0u8; DVD_SECTOR_SIZE];
        sector[0..4].copy_from_slice(&[0x00, 0x00, 0x01, 0xBA]);
        sector[13] = 0;

        let cursor = 14usize;
        let sub_header_extra_len = 6usize;
        let payload = [0xF8, 0x72, 0x6F, 0xBA];
        let pes_payload_len = 3 + 4 + sub_header_extra_len + payload.len();
        sector[cursor..cursor + 3].copy_from_slice(&[0x00, 0x00, 0x01]);
        sector[cursor + 3] = 0xBD;
        sector[cursor + 4] = ((pes_payload_len >> 8) & 0xFF) as u8;
        sector[cursor + 5] = (pes_payload_len & 0xFF) as u8;
        sector[cursor + 6] = 0x80;
        sector[cursor + 7] = 0x80;
        sector[cursor + 8] = 0;
        let sub = cursor + 9;
        sector[sub] = 0xA1;
        sector[sub + 1] = 0;
        sector[sub + 2] = 0;
        sector[sub + 3] = sub_header_extra_len as u8;
        sector[sub + 8] = 0;
        let body = sub + 4 + sub_header_extra_len;
        sector[body..body + payload.len()].copy_from_slice(&payload);
        sector
    }

    fn disc_with_mkb_and_aob_for_probe() -> DvdaDisc {
        DvdaDisc {
            amg: AmgInfo {
                source_file: "AUDIO_TS.IFO".to_string(),
                last_sector: 0,
                ifo_last_sector: 0,
                specification_version: 0,
                category: 0,
                nr_of_volumes: 1,
                this_volume_nr: 1,
                disc_side: 1,
                audio_title_sets: 1,
                video_title_sets: 0,
                provider_identifier: String::new(),
                position_code: 0,
                ifo_last_byte: 0,
                first_play_pgc: 0,
                pointers: AmgPointers::default(),
                audio_title_table: Vec::new(),
            },
            title_sets: vec![TitleSet {
                number: 1,
                source_file: "ATS_01_0.IFO".to_string(),
                kind: TitleSetKind::Audio,
                header: AtsiHeader {
                    ats_last_sector: 0,
                    atsi_last_sector: 0,
                    specification_version: 0,
                    category: 0,
                    atsm_vobs: 0,
                    atstt_vobs: 0,
                    ats_ptt_srpt: 0,
                    ats_pgcit: 0,
                    ats_c_adt: 0,
                    ats_vobu_admap: 0,
                },
                audio_pgcit_offset: 0,
                audio_formats: Vec::new(),
                downmix_matrices: Vec::new(),
                aobs: vec![AobFileEntry {
                    title_set_nr: 1,
                    part_nr: 1,
                    file_name: "ATS_01_1.AOB".to_string(),
                    exists: true,
                    byte_len: DVD_BLOCK_SIZE,
                    block_first: 0,
                    block_last: 0,
                }],
                aobs_last_sector: Some(0),
                titles: Vec::new(),
                diagnostics: Vec::new(),
            }],
            samg: Some(SamgInfo {
                source_file: "AUDIO_PP.IFO".to_string(),
                specification_version: 0,
                track_count_declared: 0,
                tracks: Vec::new(),
                raw_len: 0,
                expected_len: 0,
                copy_size: 0,
                copy_count: 0,
                repeated_copies_valid: false,
                copy_validations: Vec::new(),
                diagnostics: Vec::new(),
            }),
            groups: Vec::new(),
            copy_protection: CopyProtectionInfo {
                mkb_present: true,
                cppm_detected: true,
                source: CopyProtectionSource::MkbPresence,
            },
            supplemental_video_ifo_present: false,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn aob_probe_accepts_visible_mpeg_ps_dvda_substream() {
        let sector = probe_test_mlp_sector();
        let probe = probe_mpeg_ps_sector("ATS_01_1.AOB", &sector);

        assert!(probe.pack_header_valid);
        assert_eq!(probe.pes_packets, 1);
        assert_eq!(probe.private_stream_1_packets, 1);
        assert_eq!(probe.dvd_audio_substreams, 1);
        assert!(probe.audio_data_looks_readable());
    }

    #[test]
    fn aob_probe_rejects_missing_mpeg_ps_pack_header() {
        let mut sector = probe_test_mlp_sector();
        sector[0..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);

        let probe = probe_mpeg_ps_sector("ATS_01_1.AOB", &sector);

        assert!(!probe.pack_header_valid);
        assert!(!probe.audio_data_looks_readable());
        assert!(probe.reason.contains("missing MPEG-PS pack start code"));
    }

    #[test]
    fn mkb_present_with_readable_aob_does_not_set_cppm_detected() {
        let mut files = BTreeMap::new();
        files.insert("ATS_01_1.AOB".to_string(), probe_test_mlp_sector());
        let volume = MemoryDvdaVolume { files };
        let mut disc = disc_with_mkb_and_aob_for_probe();

        refine_copy_protection_from_aob_probe(&volume, &mut disc, false).unwrap();

        assert!(disc.copy_protection.mkb_present);
        assert!(!disc.copy_protection.cppm_detected);
        assert_eq!(
            disc.copy_protection.source,
            CopyProtectionSource::MkbPresentAobProbeReadable
        );
        assert!(disc
            .diagnostics
            .iter()
            .any(|diag| diag.code == "dvda_mkb_present_aob_probe_readable"));
    }

    #[test]
    fn mkb_present_with_unreadable_aob_sets_cppm_detected() {
        let mut bad_sector = probe_test_mlp_sector();
        bad_sector[0..4].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let mut files = BTreeMap::new();
        files.insert("ATS_01_1.AOB".to_string(), bad_sector);
        let volume = MemoryDvdaVolume { files };
        let mut disc = disc_with_mkb_and_aob_for_probe();

        refine_copy_protection_from_aob_probe(&volume, &mut disc, false).unwrap();

        assert!(disc.copy_protection.cppm_detected);
        assert_eq!(disc.copy_protection.source, CopyProtectionSource::AobProbeNoMpegPs);
    }

    #[test]
    fn assume_decrypted_override_disables_cppm_blocking_without_probe() {
        let volume = MemoryDvdaVolume {
            files: BTreeMap::new(),
        };
        let mut disc = disc_with_mkb_and_aob_for_probe();

        refine_copy_protection_from_aob_probe(&volume, &mut disc, true).unwrap();

        assert!(!disc.copy_protection.cppm_detected);
        assert_eq!(
            disc.copy_protection.source,
            CopyProtectionSource::AssumeDecryptedOverride
        );
    }
}
