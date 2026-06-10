#![forbid(unsafe_code)]

//! DVD-Audio MLP/LPCM channel-assignment layout table.
//!
//! The table matches the 21 MLP/PCM channel-assignment entries used by
//! foo_input_dvda and by the Phase 1 IFO parser.  The source order is the
//! DVD-Audio elementary-stream order: group 1 channels first, followed by group
//! 2 channels.  LPCM realization can either preserve that source order for
//! archival output or reorder decoded samples into conventional
//! WAVEFORMATEXTENSIBLE/ffmpeg order for WAV interoperability.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DvdaChannelOrderPolicy {
    PreserveDvdAudio,
    WaveExtensible,
}

impl DvdaChannelOrderPolicy {
    pub(super) const DEFAULT: Self = Self::PreserveDvdAudio;

    #[must_use]
    pub(super) fn from_env_var(name: &str) -> Self {
        let Ok(value) = std::env::var(name) else {
            return Self::DEFAULT;
        };
        Self::from_value(&value).unwrap_or_else(|| {
            log::warn!(
                "Unknown DVD-Audio LPCM channel-order policy {value:?} in {name}; using {}",
                Self::DEFAULT.as_str()
            );
            Self::DEFAULT
        })
    }

    #[must_use]
    pub(super) fn from_value(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "source" | "preserve" | "preservedvda" | "preservedvdaudio" | "dvda" | "dvdasource" => {
                Some(Self::PreserveDvdAudio)
            }
            "wav" | "wave" | "wfx" | "waveextensible" | "waveformatextensible" | "interoperable" => {
                Some(Self::WaveExtensible)
            }
            _ => None,
        }
    }

    #[must_use]
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::PreserveDvdAudio => "preserve-dvd-audio-source-order",
            Self::WaveExtensible => "waveformatextensible-order",
        }
    }

    #[must_use]
    pub(super) const fn behavior(self) -> &'static str {
        match self {
            Self::PreserveDvdAudio => {
                "preserve DVD-Audio group order; label WAV channel_layout only when source order already matches a safe ffmpeg/WAV alias"
            }
            Self::WaveExtensible => {
                "reorder LPCM samples from DVD-Audio group order into conventional WAVEFORMATEXTENSIBLE channel order before WAV muxing"
            }
        }
    }
}

impl Default for DvdaChannelOrderPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DvdaChannelLayout {
    pub code: u8,
    pub group1: &'static [&'static str],
    pub group2: &'static [&'static str],
}

impl DvdaChannelLayout {
    #[must_use]
    pub(super) fn group1_channel_count(self) -> u32 {
        self.group1.len() as u32
    }

    #[must_use]
    pub(super) fn group2_channel_count(self) -> u32 {
        self.group2.len() as u32
    }

    #[must_use]
    pub(super) fn total_channel_count(self) -> u32 {
        self.group1_channel_count() + self.group2_channel_count()
    }

    #[must_use]
    pub(super) fn ordered_channel_names(self) -> Vec<&'static str> {
        self.group1
            .iter()
            .chain(self.group2.iter())
            .copied()
            .collect()
    }

    #[must_use]
    pub(super) fn order_label(self) -> String {
        self.ordered_channel_names().join(",")
    }

    #[must_use]
    pub(super) fn wave_ordered_channel_names(self) -> Vec<&'static str> {
        let source = self.ordered_channel_names();
        let mut ordered = Vec::with_capacity(source.len());
        for name in ["L", "R", "C", "LFE", "Ls", "Rs", "S"] {
            if source.iter().any(|candidate| *candidate == name) {
                ordered.push(name);
            }
        }
        for name in source {
            if !ordered.iter().any(|candidate| *candidate == name) {
                ordered.push(name);
            }
        }
        ordered
    }

    #[must_use]
    pub(super) fn wave_order_label(self) -> String {
        self.wave_ordered_channel_names().join(",")
    }

    #[must_use]
    pub(super) fn output_order_label(self, policy: DvdaChannelOrderPolicy) -> String {
        match policy {
            DvdaChannelOrderPolicy::PreserveDvdAudio => self.order_label(),
            DvdaChannelOrderPolicy::WaveExtensible => self.wave_order_label(),
        }
    }

    #[must_use]
    pub(super) fn source_to_output_indices(self, policy: DvdaChannelOrderPolicy) -> Vec<usize> {
        let source = self.ordered_channel_names();
        let target = match policy {
            DvdaChannelOrderPolicy::PreserveDvdAudio => source.clone(),
            DvdaChannelOrderPolicy::WaveExtensible => self.wave_ordered_channel_names(),
        };
        target
            .iter()
            .filter_map(|name| source.iter().position(|candidate| candidate == name))
            .collect()
    }

    #[must_use]
    pub(super) fn group_label(self) -> String {
        let group1 = if self.group1.is_empty() {
            "-".to_string()
        } else {
            self.group1.join(",")
        };
        let group2 = if self.group2.is_empty() {
            "-".to_string()
        } else {
            self.group2.join(",")
        };
        format!(
            "code {}: group1=[{}], group2=[{}], source_order=[{}], wave_order=[{}]",
            self.code,
            group1,
            group2,
            self.order_label(),
            self.wave_order_label()
        )
    }

    /// ffprobe channel_layout strings that are compatible with the DVD-A source order.
    ///
    /// Only layouts whose common WAV/ffmpeg channel order matches the DVD-Audio
    /// group interleave are listed. Some DVD-A assignment codes contain the same
    /// channel set as a common WAV layout but in a different order; those return
    /// no compatible layout so the mux/probe boundary never mislabels channels.
    #[must_use]
    pub(super) fn compatible_ffmpeg_layouts(self) -> &'static [&'static str] {
        match self.code {
            0 => &["mono"],
            1 => &["stereo"],
            4 => &["2.1"],
            7 => &["3.0"],
            8 | 13 => &["4.0"],
            10 | 15 => &["3.1"],
            11 | 16 => &["4.1"],
            12 | 17 => &["5.1", "5.1(side)"],
            14 => &["5.0", "5.0(side)"],
            _ => &[],
        }
    }

    #[must_use]
    pub(super) fn ffmpeg_source_input_layout(self) -> Option<&'static str> {
        self.compatible_ffmpeg_layouts().first().copied()
    }

    /// ffmpeg layout alias for samples that have already been reordered into
    /// WAVEFORMATEXTENSIBLE channel order.
    #[must_use]
    pub(super) fn ffmpeg_wave_input_layout(self) -> Option<&'static str> {
        match self.code {
            0 => Some("mono"),
            1 => Some("stereo"),
            3 => Some("quad"),
            4 => Some("2.1"),
            7 => Some("3.0"),
            8 | 13 => Some("4.0"),
            9 | 14 | 19 => Some("5.0"),
            10 | 15 => Some("3.1"),
            11 | 16 | 18 => Some("4.1"),
            12 | 17 | 20 => Some("5.1"),
            _ => None,
        }
    }

    #[must_use]
    pub(super) fn ffmpeg_input_layout_for_policy(self, policy: DvdaChannelOrderPolicy) -> Option<&'static str> {
        match policy {
            DvdaChannelOrderPolicy::PreserveDvdAudio => self.ffmpeg_source_input_layout(),
            DvdaChannelOrderPolicy::WaveExtensible => self.ffmpeg_wave_input_layout(),
        }
    }
}

#[must_use]
pub(super) fn layout_for_assignment_code(code: u8) -> Option<DvdaChannelLayout> {
    let (group1, group2): (&'static [&'static str], &'static [&'static str]) = match code {
        0 => (&["C"], &[]),
        1 => (&["L", "R"], &[]),
        2 => (&["L", "R"], &["S"]),
        3 => (&["L", "R"], &["Ls", "Rs"]),
        4 => (&["L", "R"], &["LFE"]),
        5 => (&["L", "R"], &["LFE", "S"]),
        6 => (&["L", "R"], &["LFE", "Ls", "Rs"]),
        7 => (&["L", "R"], &["C"]),
        8 => (&["L", "R"], &["C", "S"]),
        9 => (&["L", "R"], &["C", "Ls", "Rs"]),
        10 => (&["L", "R"], &["C", "LFE"]),
        11 => (&["L", "R"], &["C", "LFE", "S"]),
        12 => (&["L", "R"], &["C", "LFE", "Ls", "Rs"]),
        13 => (&["L", "R", "C"], &["S"]),
        14 => (&["L", "R", "C"], &["Ls", "Rs"]),
        15 => (&["L", "R", "C"], &["LFE"]),
        16 => (&["L", "R", "C"], &["LFE", "S"]),
        17 => (&["L", "R", "C"], &["LFE", "Ls", "Rs"]),
        18 => (&["L", "R", "Ls", "Rs"], &["LFE"]),
        19 => (&["L", "R", "Ls", "Rs"], &["C"]),
        20 => (&["L", "R", "Ls", "Rs"], &["C", "LFE"]),
        _ => return None,
    };
    Some(DvdaChannelLayout { code, group1, group2 })
}

#[must_use]
pub(super) fn source_group_order_label(group1: Option<&str>, group2: Option<&str>) -> Option<String> {
    let mut names = Vec::new();
    for group in [group1, group2].iter().filter_map(|value| *value) {
        names.extend(
            group
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned),
        );
    }
    (!names.is_empty()).then(|| names.join(","))
}

#[must_use]
pub(super) fn normalized_ffmpeg_channel_layout(layout: &str) -> String {
    layout
        .trim()
        .to_ascii_lowercase()
        .replace(' ', "")
        .replace("6.0(side)", "6.0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_code_12_is_standard_dvda_5_1_order() {
        let layout = layout_for_assignment_code(12).unwrap();
        assert_eq!(layout.group1, &["L", "R"]);
        assert_eq!(layout.group2, &["C", "LFE", "Ls", "Rs"]);
        assert_eq!(layout.order_label(), "L,R,C,LFE,Ls,Rs");
        assert_eq!(layout.wave_order_label(), "L,R,C,LFE,Ls,Rs");
        assert!(layout.compatible_ffmpeg_layouts().contains(&"5.1"));
    }

    #[test]
    fn assignment_code_20_is_safe_after_wave_order_reorder() {
        let layout = layout_for_assignment_code(20).unwrap();
        assert_eq!(layout.order_label(), "L,R,Ls,Rs,C,LFE");
        assert_eq!(layout.wave_order_label(), "L,R,C,LFE,Ls,Rs");
        assert!(layout.compatible_ffmpeg_layouts().is_empty());
        assert_eq!(layout.ffmpeg_input_layout_for_policy(DvdaChannelOrderPolicy::WaveExtensible), Some("5.1"));
        assert_eq!(layout.source_to_output_indices(DvdaChannelOrderPolicy::WaveExtensible), vec![0, 1, 4, 5, 2, 3]);
    }

    #[test]
    fn parses_channel_order_policy_values() {
        assert_eq!(
            DvdaChannelOrderPolicy::from_value("preserve"),
            Some(DvdaChannelOrderPolicy::PreserveDvdAudio)
        );
        assert_eq!(
            DvdaChannelOrderPolicy::from_value("waveformatextensible"),
            Some(DvdaChannelOrderPolicy::WaveExtensible)
        );
    }
}
