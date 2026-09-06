use super::types::{
    AacProfile, AdditionalOptionsHelp, AudioFormat, DestinationMode, EditingField, FlacSection,
    FormatSpecificHelp, NyquistTransition, OpusContentType, PopupFocus, PopupState, PopupType,
    ReplayGainMode, SimpleWizard,
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::theme::WizardTheme;

#[derive(Debug, Clone, Copy)]
pub(crate) struct WizardRenderCtx {
    theme: WizardTheme,
}

pub struct MouseAreas {
    pub areas: Vec<(Rect, ButtonId)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ButtonId {
    Back,
    Next,
    Cancel,
    LoadPreset,
    FormatOption(usize),
    QualityOption(usize), // Add this for MP3/AAC/Opus quality selection
    BitDepthOption(usize),
    SampleRateOption(usize),
    CompressionLevelOption(usize),
    ResampleQualityOption(usize),
    DitherOption(usize),
    ProcessingOption(usize),
    NyquistTransitionOption(usize),
    SsrcInsaneCheckbox,
    AdditionalOption(usize),
    AdditionalOptionCheckbox(usize),
    InfoIcon(FlacSection),
    AdditionalInfoIcon(AdditionalOptionsHelp),
    FormatInfoIcon(FormatSpecificHelp),
    LosslessInfoIcon,
    LossyInfoIcon,
    PopupOk,
    PopupCancel,
    PresetItem(usize),
    PopupBackground,
    BrowseButton,
    FileItem(usize),
    NewFolder,
    FileBrowserSelect,
    FileBrowserCancel,
}

impl MouseAreas {
    pub fn new() -> Self {
        Self { areas: Vec::new() }
    }

    pub fn add(&mut self, rect: Rect, id: ButtonId) {


        self.areas.push((rect, id));
    }

    pub fn get_button_at(&self, x: u16, y: u16) -> Option<ButtonId> {


        // Still check normally (in reverse order - last added first)
        for (rect, id) in self.areas.iter().rev() {
            if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
                return Some(*id);
            }
        }
        None
    }
}

pub fn draw_wizard(f: &mut Frame, wizard: &SimpleWizard) -> MouseAreas {
    draw_wizard_with_theme(f, wizard, WizardTheme::default())
}

pub fn draw_wizard_with_theme(
    f: &mut Frame,
    wizard: &SimpleWizard,
    theme: WizardTheme,
) -> MouseAreas {
    let ctx = WizardRenderCtx { theme };
    draw_wizard_inner(f, wizard, &ctx)
}

fn draw_wizard_inner(f: &mut Frame, wizard: &SimpleWizard, ctx: &WizardRenderCtx) -> MouseAreas {

    let mut mouse_areas = MouseAreas::new();

    // Get terminal size from frame
    let term_size = f.size();

    // Calculate wizard dimensions from the terminal size
    let width = (term_size.width as f32 * 0.8).min(100.0).max(70.0) as u16;
    let height = (term_size.height as f32 * 0.85).min(50.0).max(30.0) as u16;

    let x = (term_size.width.saturating_sub(width)) / 2;
    let y = (term_size.height.saturating_sub(height)) / 2;

    let wizard_area = Rect::new(x, y, width, height);

    // Check if terminal is too small
    if term_size.height < 35 || term_size.width < 90 {
        let msg_text;
        let msg = if term_size.height < 35 {
            msg_text = format!("Height: {} rows", term_size.height);
            vec![
                "Terminal too small!",
                "",
                msg_text.as_str(),
                "Need: 35 rows minimum",
                "",
                "Please resize your terminal",
            ]
        } else {
            msg_text = format!("Width: {} columns", term_size.width);
            vec![
                "Terminal too small!",
                "",
                msg_text.as_str(),
                "Need: 90 columns minimum",
                "",
                "Please resize your terminal",
            ]
        };

        let lines: Vec<Line> = msg.iter().map(|s| Line::from(*s)).collect();
        let paragraph = Paragraph::new(lines).alignment(Alignment::Center).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(Style::default().fg(ctx.theme.error).add_modifier(Modifier::BOLD))
                .title(" Error "),
        );

        // Center the error message
        let msg_width = 40;
        let msg_height = 10;
        let msg_x = (term_size.width.saturating_sub(msg_width)) / 2;
        let msg_y = (term_size.height.saturating_sub(msg_height)) / 2;
        let msg_area = Rect::new(msg_x, msg_y, msg_width, msg_height);

        f.render_widget(paragraph, msg_area);
        return mouse_areas;
    }

    // Clear and fill the wizard panel background.
    f.render_widget(Clear, wizard_area);

    // Fill the wizard panel with the configured background role.
    let bg_block = Block::default().style(Style::default().bg(ctx.theme.background));
    f.render_widget(bg_block, wizard_area);

    // Main panel chrome uses explicit border and title roles.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border))
        .title(Span::styled(
            " Audio Conversion Wizard ",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(ctx.theme.background));

    f.render_widget(block, wizard_area);

    // Inner layout
    let inner = Rect::new(
        wizard_area.x + 1,
        wizard_area.y + 1,
        wizard_area.width - 2,
        wizard_area.height - 2,
    );

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(20),   // Content
            Constraint::Length(3), // Navigation
        ])
        .split(inner);

    // Draw header with step indicator
    draw_header(f, ctx, chunks[0], wizard.current_step);

    // Draw content based on current step
    match wizard.current_step {
        0 => draw_format_selection(f, ctx, chunks[1], wizard, &mut mouse_areas, wizard_area),
        1 => draw_quality_options(f, ctx, chunks[1], wizard, &mut mouse_areas, wizard_area),
        2 => draw_additional_options(f, ctx, chunks[1], wizard, &mut mouse_areas, wizard_area),
        3 => draw_confirmation(f, ctx, chunks[1], wizard, &mut mouse_areas),
        _ => {}
    }

    // Draw navigation
    draw_navigation(f, ctx, chunks[2], wizard, &mut mouse_areas);

    // Draw popup if active
    if let Some(popup_state) = &wizard.popup_state {
        draw_popup(
            f,
                ctx,
            wizard_area,
            popup_state,
            &mut mouse_areas,
            wizard.hovered_button,
        );
    }


    mouse_areas
}

fn draw_header(f: &mut Frame, ctx: &WizardRenderCtx, area: Rect, current_step: usize) {
    let steps = vec![
        "Format & Settings",
        "Quality",
        "Additional Options",
        "Confirm",
    ];
    let mut spans = vec![];

    for (i, step) in steps.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" → "));
        }

        let style = if i == current_step {
            Style::default()
                .fg(ctx.theme.accent)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC)
        } else if i < current_step {
            Style::default().fg(ctx.theme.text)
        } else {
            Style::default().fg(ctx.theme.text_dim)
        };

        spans.push(Span::styled(format!("{}. {}", i + 1, step), style));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(vec![line])
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::BOTTOM));

    f.render_widget(paragraph, area);
}

fn draw_format_selection(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
    wizard_area: Rect,
) {
    // Split area into two sections: format list and format-specific options
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    // Left side: Format selection
    draw_format_list(f, ctx, chunks[0], wizard, mouse_areas);

    // Right side: Format-specific options (only if a format is selected)
    if wizard.selected_format.is_some() {
        draw_format_options(f, ctx, chunks[1], wizard, mouse_areas, wizard_area);
    }

    // Show format-specific help popups
    if let Some(help_section) = wizard.show_format_help_for {
        match help_section {
            FormatSpecificHelp::WavPackCompression => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "WavPack Compression",
                    "WavPack compression modes and their encoder flags:\n\n\
                     • Fast (Low CPU, larger files) → -f flag\n\
                       - Fastest encoding/decoding\n\
                       - ~60-70% of original size\n\
                       - Good for portable players\n\n\
                     • High (Balanced) → -h flag (default)\n\
                       - Best balance of speed and size\n\
                       - ~55-65% of original size\n\
                       - Recommended for most uses\n\n\
                     • Very High (Smaller files) → -hh flag\n\
                       - More CPU intensive\n\
                       - ~53-63% of original size\n\
                       - Good for archival\n\n\
                     • Maximum (Best compression) → -hhh flag\n\
                       - ~52-62% of original size\n\
                       - Noticeably slower encoding\n\n\
                     • Ultra (Very slow) → -hh -x flags\n\
                       - Adds extra processing (-x flag)\n\
                       - ~51-61% of original size\n\
                       - Much slower encoding\n\n\
                     • Extreme (Slowest) → -hh -x4 to -x6 flags\n\
                       - Maximum extra processing\n\
                       - ~50-60% of original size\n\
                       - Extremely slow, minimal gains\n\n\
                     💡 All modes are bit-perfect lossless!\n\
                        The -x flag enables extra processing passes.\n\
                        Higher -x values (1-6) = more passes.",
                );
            }
            FormatSpecificHelp::Mp3Bitrate => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "MP3 Bitrate",
                    "MP3 bitrate determines quality and file size:\n\n\
                     Constant Bitrate (CBR):\n\
                     • 320 kbps - Highest quality, largest files\n\
                     • 256 kbps - Excellent quality, hard to distinguish\n\
                     • 192 kbps - Very good for most listeners\n\
                     • 128 kbps - Acceptable, noticeable quality loss\n\n\
                     Variable Bitrate (VBR):\n\
                     • V0 (~245 kbps avg) - Best VBR quality\n\
                       More efficient than CBR 320\n\
                     • V2 (~190 kbps avg) - Popular choice\n\
                       Great balance of quality/size\n\n\
                     💡 VBR recommendations:\n\
                     - V0 for archival/critical listening\n\
                     - V2 for general use\n\
                     - VBR adapts bitrate to music complexity\n\n\
                     ⚠️  Below 192 kbps, quality loss becomes\n\
                        noticeable on good equipment.",
                );
            }
            FormatSpecificHelp::AacProfile => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "AAC Profile",
                    "AAC profiles optimize for different bitrates:\n\n\
                     • LC-AAC (Low Complexity)\n\
                       - Standard AAC profile\n\
                       - Best for 128 kbps and higher\n\
                       - Full frequency response\n\
                       - Used by iTunes, streaming services\n\n\
                     • HE-AAC (High Efficiency / AAC+)\n\
                       - Uses SBR (Spectral Band Replication)\n\
                       - Best for 64-96 kbps\n\
                       - Recreates high frequencies\n\
                       - Good for internet radio\n\n\
                     • HE-AACv2 (AAC+ with PS)\n\
                       - Adds Parametric Stereo to HE-AAC\n\
                       - Best for ≤64 kbps\n\
                       - Mono core + stereo reconstruction\n\
                       - Maximum efficiency at low bitrates\n\n\
                     💡 Profile selection guide:\n\
                     - 128+ kbps: Use LC-AAC\n\
                     - 64-96 kbps: Use HE-AAC\n\
                     - ≤64 kbps: Use HE-AACv2",
                );
            }
            FormatSpecificHelp::AacBitrate => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "AAC Bitrate",
                    "AAC bitrates by profile:\n\n\
                     LC-AAC (128+ kbps recommended):\n\
                     • 320 kbps - Transparent quality\n\
                     • 256 kbps - Excellent, near-transparent\n\
                     • 192 kbps - Very good quality\n\
                     • 160 kbps - Good quality, balanced\n\
                     • 128 kbps - Good quality, efficient\n\n\
                     HE-AAC (64-96 kbps recommended):\n\
                     • 96 kbps - Very good with SBR\n\
                     • 64 kbps - Good quality for the bitrate\n\n\
                     HE-AACv2 (≤64 kbps recommended):\n\
                     • 64 kbps - Best quality at this rate\n\
                     • 48 kbps - Acceptable for speech/podcasts\n\n\
                     💡 AAC is ~30% more efficient than MP3:\n\
                     - AAC 128 ≈ MP3 192\n\
                     - AAC 192 ≈ MP3 256\n\n\
                     ⚠️  Using wrong profile wastes bits:\n\
                        LC-AAC at 64 kbps sounds worse than\n\
                        HE-AAC at the same bitrate!",
                );
            }
            FormatSpecificHelp::OpusQuality => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "Opus Quality",
                    "Opus quality presets optimize for different uses:\n\n\
                     • Low (~64-96 kbps)\n\
                       - Speech, podcasts, audiobooks\n\
                       - Clear voice reproduction\n\
                       - Minimal music artifacts\n\n\
                     • Medium (~128-160 kbps)\n\
                       - Music streaming quality\n\
                       - Good for most listeners\n\
                       - Efficient bandwidth use\n\n\
                     • High (~192-256 kbps)\n\
                       - Transparent quality for most\n\
                       - Hard to distinguish from source\n\
                       - Recommended for music lovers\n\n\
                     • Very High (~256-320 kbps)\n\
                       - Archival quality\n\
                       - Practically transparent\n\
                       - Future-proof encoding\n\n\
                     • Insane (~320-510 kbps)\n\
                       - Maximum possible quality\n\
                       - Overkill for most content\n\
                       - For the most demanding audiophiles\n\n\
                     💡 Opus excels at ALL bitrates:\n\
                     - Best speech codec at 32-64 kbps\n\
                     - Matches MP3 quality at 96 kbps\n\
                     - Transparent at 128-192 kbps\n\n\
                     ⚠️  Opus always outputs 48 kHz internally",
                );
            }
            FormatSpecificHelp::OpusContentType => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "Opus Content Type",
                    "Optimize encoder for content type:\n\n\
                     • Music\n\
                       - Optimized for musical content\n\
                       - Full frequency response\n\
                       - Stereo imaging preserved\n\
                       - Dynamic range maintained\n\
                       - Best for: songs, instrumental\n\n\
                     • Voice\n\
                       - Optimized for speech clarity\n\
                       - Enhanced voice frequencies\n\
                       - Reduced background noise\n\
                       - Improved intelligibility\n\
                       - Best for: podcasts, audiobooks,\n\
                         lectures, voice recordings\n\n\
                     💡 The encoder adapts its psychoacoustic\n\
                        model based on content type:\n\
                     - Music: preserves harmonics, ambience\n\
                     - Voice: focuses on speech band,\n\
                             reduces artifacts\n\n\
                     ⚠️  Using wrong mode won't break anything\n\
                        but may be slightly less optimal.",
                );
            }
        }
    }

    // Show format selection help popups
    if wizard.show_additional_help_for == Some(AdditionalOptionsHelp::CopyFiles) {
        // Using CopyFiles as a temporary placeholder for LosslessInfoIcon
        draw_help_box(
            f,
                ctx,
            wizard_area,
            "Lossless Formats",
            "Lossless formats preserve 100% of the original audio data.\n\
             They can be converted back and forth without quality loss.\n\n\
             • FLAC (Free Lossless Audio Codec)\n\
               - Reduces file size by 30-60% with NO quality loss\n\
               - Widely supported by modern players\n\
               - Best choice for music archiving\n\
               - Supports extensive metadata\n\n\
             • WAV (Waveform Audio)\n\
               - No compression, largest files\n\
               - Industry standard for professional audio\n\
               - Universal compatibility\n\
               - Limited metadata support\n\n\
             • AIFF (Audio Interchange File Format)\n\
               - Apple's equivalent to WAV\n\
               - Uncompressed, perfect quality\n\
               - Common in Mac/iOS ecosystem\n\
               - Better metadata than WAV\n\n\
             • WavPack\n\
               - Hybrid compression (lossless + correction file)\n\
               - Very efficient compression\n\
               - Less common, limited player support\n\
               - Good for special archiving needs",
        );
    } else if wizard.show_additional_help_for == Some(AdditionalOptionsHelp::CopySubdirectories) {
        // Using CopySubdirectories as a temporary placeholder for LossyInfoIcon
        draw_help_box(
            f,
                ctx,
            wizard_area,
            "Lossy Formats",
            "Lossy formats reduce file size by removing audio data that's\n\
             less noticeable to human ears. This process is IRREVERSIBLE!\n\n\
             • MP3 (MPEG-1 Audio Layer III)\n\
               - Most compatible format, plays everywhere\n\
               - Good quality at 256-320 kbps\n\
               - Older technology, less efficient\n\
               - V0/V2 VBR modes offer better quality/size ratio\n\n\
             • AAC (Advanced Audio Coding)\n\
               - 30% more efficient than MP3\n\
               - Three profiles available:\n\
                 • LC-AAC: Standard profile for 128+ kbps\n\
                 • HE-AAC: Best for 64-96 kbps (uses SBR)\n\
                 • HE-AACv2: Best for ≤64 kbps (adds PS)\n\
               - Standard for iTunes, YouTube, streaming\n\n\
             • Opus\n\
               - Most advanced lossy codec\n\
               - Quality presets:\n\
                 • Low: ~64-96 kbps (speech, podcasts)\n\
                 • Medium: ~128-160 kbps (music streaming)\n\
                 • High: ~192-256 kbps (transparent quality)\n\
                 • Very High: ~256-320 kbps (archival)\n\
               - Can optimize for Music or Voice content\n\
               - Growing support, not universal yet\n\n\
             ⚠️  NEVER convert between lossy formats!\n\
                Each conversion adds more quality loss.\n\
                Always convert from a lossless source.",
        );
    }
}

fn draw_format_list(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
) {
    // Add left padding
    let padded_area = Rect::new(
        area.x + 6,
        area.y,
        area.width.saturating_sub(12),
        area.height,
    );

    let mut lines = vec![];
    let mut y_offset = padded_area.y;

    // Lossless formats header with info icon
    lines.push(Line::from(vec![
        Span::styled(
            "Lossless Formats ",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    // Register lossless info icon click area
    mouse_areas.add(
        Rect::new(
            padded_area.x + "Lossless Formats ".len() as u16,
            y_offset,
            1,
            1,
        ),
        ButtonId::LosslessInfoIcon,
    );
    y_offset += 1;
    lines.push(Line::from(""));
    y_offset += 1;

    // Lossless formats
    let lossless_formats = vec![
        AudioFormat::Flac,
        AudioFormat::Wav,
        AudioFormat::Aiff,
        AudioFormat::WavPack,
    ];

    let mut format_index = 0;
    for format in lossless_formats.iter() {
        let is_selected = wizard.selected_format == Some(*format);
        let is_focused = wizard.selected_index == format_index;

        let radio = if is_selected { "◉" } else { "○" };
        let radio_style = if is_selected {
            Style::default().fg(ctx.theme.accent)
        } else {
            Style::default()
        };

        let text_style = if is_focused {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ctx.theme.text)
        };

        let line = if is_focused {
            Line::from(vec![
                Span::styled("   ", text_style),
                Span::styled(
                    radio,
                    radio_style
                        .fg(ctx.theme.text)
                        .bg(ctx.theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {}", format), text_style),
            ])
        } else {
            Line::from(vec![
                Span::raw("   "),
                Span::styled(radio, radio_style),
                Span::raw(" "),
                Span::styled(format.to_string(), text_style),
            ])
        };

        lines.push(line);

        // Register mouse area - limit width to actual text width
        let text_width = format.to_string().len() + 4; // Radio button + space + text
        mouse_areas.add(
            Rect::new(padded_area.x, y_offset, text_width as u16, 1),
            ButtonId::FormatOption(format_index),
        );
        y_offset += 1;
        format_index += 1;
    }

    // Add space between categories (2 blank lines)
    lines.push(Line::from(""));
    lines.push(Line::from(""));
    y_offset += 2;

    // Lossy formats header with info icon
    lines.push(Line::from(vec![
        Span::styled(
            "Lossy Formats ",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    // Register lossy info icon click area
    mouse_areas.add(
        Rect::new(
            padded_area.x + "Lossy Formats ".len() as u16,
            y_offset,
            1,
            1,
        ),
        ButtonId::LossyInfoIcon,
    );
    y_offset += 1;
    lines.push(Line::from(""));
    y_offset += 1;

    // Lossy formats
    let lossy_formats = vec![AudioFormat::Mp3, AudioFormat::Aac, AudioFormat::Opus];

    for format in lossy_formats.iter() {
        let is_selected = wizard.selected_format == Some(*format);
        let is_focused = wizard.selected_index == format_index;

        let radio = if is_selected { "◉" } else { "○" };
        let radio_style = if is_selected {
            Style::default().fg(ctx.theme.accent)
        } else {
            Style::default()
        };

        let text_style = if is_focused {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ctx.theme.text)
        };

        let line = if is_focused {
            Line::from(vec![
                Span::styled("   ", text_style),
                Span::styled(
                    radio,
                    radio_style
                        .fg(ctx.theme.text)
                        .bg(ctx.theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {}", format), text_style),
            ])
        } else {
            Line::from(vec![
                Span::raw("   "),
                Span::styled(radio, radio_style),
                Span::raw(" "),
                Span::styled(format.to_string(), text_style),
            ])
        };

        lines.push(line);

        // Register mouse area - limit width to actual text width
        let text_width = format.to_string().len() + 4; // Radio button + space + text
        mouse_areas.add(
            Rect::new(padded_area.x, y_offset, text_width as u16, 1),
            ButtonId::FormatOption(format_index),
        );
        y_offset += 1;
        format_index += 1;
    }

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
    f.render_widget(paragraph, padded_area);
}

fn draw_format_options(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
    wizard_area: Rect,
) {
    let padded_area = Rect::new(
        area.x + 3,
        area.y,
        area.width.saturating_sub(6),
        area.height,
    );

    // Debug which format is selected

    match wizard.selected_format {
        Some(AudioFormat::Flac) => {
            draw_flac_format_options(f, ctx, padded_area, wizard, mouse_areas, wizard_area)
        }
        Some(AudioFormat::Mp3) => draw_mp3_format_options(f, ctx, padded_area, wizard, mouse_areas),
        Some(AudioFormat::Aac) => draw_aac_format_options(f, ctx, padded_area, wizard, mouse_areas),
        Some(AudioFormat::Opus) => draw_opus_format_options(f, ctx, padded_area, wizard, mouse_areas),
        Some(AudioFormat::WavPack) => {
            draw_wavpack_format_options(f, ctx, padded_area, wizard, mouse_areas, wizard_area)
        }
        Some(AudioFormat::Wav) | Some(AudioFormat::Aiff) => {
            // WAV and AIFF have no format-specific options
            let mut lines = vec![];
            lines.push(Line::from(Span::styled(
                "Format Options",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from("No format-specific options"));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "This format will use the quality",
                Style::default().fg(ctx.theme.text_dim),
            )));
            lines.push(Line::from(Span::styled(
                "settings from the next page.",
                Style::default().fg(ctx.theme.text_dim),
            )));

            let paragraph = Paragraph::new(lines);
            f.render_widget(paragraph, padded_area);
        }
        None => {}
    }
}

fn draw_flac_format_options(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
    wizard_area: Rect,
) {
    // Debug log the area coordinates

    let mut lines = vec![];

    // Start tracking absolute Y position from the area's Y coordinate
    let mut current_y = area.y;

    // Compression Level
    lines.push(Line::from(vec![
        Span::styled(
            "Compression Level",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    lines.push(Line::from("")); // Add blank line after header

    // Register info icon click area for Compression Level
    // Position: Compression Level header = line 0 (0-indexed)
    // DEBUG: Log what Y coordinate we're using
    mouse_areas.add(
        Rect::new(
            area.x + "Compression Level".len() as u16 + 2,
            current_y,
            1,
            1,
        ),
        ButtonId::InfoIcon(FlacSection::CompressionLevel),
    );
    current_y += 2; // Move past header and blank line

    let compression_options = SimpleWizard::get_compression_level_options();
    for (i, (value, label)) in compression_options.iter().enumerate() {
        let is_selected = wizard.compression_level == Some(*value);
        let is_focused = wizard.selected_format == Some(AudioFormat::Flac)
            && wizard.in_quality_area
            && wizard.quality_index == i;

        let line = format_option_line(ctx, label, is_selected, is_focused);
        lines.push(line);

        // IMPORTANT: Mouse events use absolute screen coordinates, so we need to ensure
        // our click areas use absolute coordinates too
        mouse_areas.add(
            Rect::new(area.x, current_y + i as u16, area.width, 1),
            ButtonId::CompressionLevelOption(i),
        );
    }
    current_y += compression_options.len() as u16; // Move past compression options

    lines.push(Line::from("")); // Add 2 blank lines between sections
    lines.push(Line::from(""));
    current_y += 2; // Move past blank lines

    // Processing Options
    lines.push(Line::from(vec![
        Span::styled(
            "Processing Options",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    lines.push(Line::from("")); // Add blank line after header

    let processing_options = vec![
        (
            "Verify encoding",
            wizard.verify_encoding.unwrap_or(false),
            false,
        ),
        (
            "Store MD5 checksum",
            wizard.store_md5.unwrap_or(true),
            false,
        ),
        (
            "Re-encode FLAC files",
            wizard.get_effective_reencode_flac(),
            wizard.is_reencode_forced(),
        ),
    ];

    // Register info icon click area for Processing Options
    mouse_areas.add(
        Rect::new(
            area.x + "Processing Options".len() as u16 + 2,
            current_y,
            1,
            1,
        ),
        ButtonId::InfoIcon(FlacSection::ProcessingOptions),
    );
    current_y += 2; // Move past header and blank line
    for (i, (label, is_checked, is_disabled)) in processing_options.iter().enumerate() {
        let is_focused = wizard.selected_format == Some(AudioFormat::Flac)
            && wizard.in_quality_area
            && wizard.quality_index == compression_options.len() + i;

        let checkbox = if *is_checked { "☑" } else { "☐" };
        let checkbox_style = if *is_disabled {
            Style::default().fg(ctx.theme.disabled_fg) // Disabled option
        } else if *is_checked {
            Style::default().fg(ctx.theme.accent)
        } else {
            Style::default()
        };

        let text_style = if is_focused {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if *is_disabled {
            Style::default().fg(ctx.theme.disabled_fg) // Disabled option
        } else {
            Style::default().fg(ctx.theme.text)
        };

        let line = if is_focused {
            Line::from(vec![
                Span::styled(" ", text_style),
                Span::styled(
                    checkbox,
                    Style::default()
                        .fg(ctx.theme.text)
                        .bg(ctx.theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", label), text_style),
            ])
        } else {
            Line::from(vec![
                Span::raw(" "),
                Span::styled(checkbox, checkbox_style),
                Span::raw("  "),
                Span::styled(label.to_string(), text_style),
            ])
        };

        lines.push(line);

        mouse_areas.add(
            Rect::new(area.x, current_y + i as u16, area.width, 1),
            ButtonId::ProcessingOption(i),
        );
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);

    // Show help popup if info icon was clicked
    if let Some(help_section) = wizard.show_help_for {
        match help_section {
            FlacSection::CompressionLevel => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "Compression Level Help",
                    "FLAC compression is ALWAYS lossless - this only affects\n\
                     file size and encoding speed, NOT audio quality!\n\n\
                     \n\
                     ◉ 0 - Fastest\n\
                       Quick encoding but larger files\n\
                     \n\
                     ◉ 5 - Balanced\n\
                       Good compromise between speed and size\n\
                     \n\
                     ◉ 8 - Best (RECOMMENDED)\n\
                       Smallest possible files\n\n\
                     💡 On modern systems, the speed difference is negligible.\n\
                        Always use level 8 for the best compression!",
                );
            }
            FlacSection::ProcessingOptions => {
                let (title, content) = if wizard.selected_format == Some(AudioFormat::WavPack) {
                    (
                        "WavPack Additional Options",
                        "Additional options for WavPack encoding:\n\n\
                      \n\
                      ☑ Store MD5 checksum for verification\n\
                        Embeds whole-file MD5 in metadata\n\
                        Allows verification of file integrity later\n\
                        Essential for archival purposes\n\
                        Tiny space overhead (16 bytes)\n\n\
                      💡 MD5 checksums allow you to verify that\n\
                         your files haven't been corrupted over time.\n\
                         Highly recommended for long-term storage.",
                    )
                } else {
                    (
                        "Processing Options Help",
                        "Additional processing options for FLAC encoding:\n\n\
                      \n\
                      ☑ Verify encoding\n\
                        Decodes the file after encoding to check for errors\n\
                        Adds time but ensures perfect encoding\n\
                        Catches any encoder bugs or disk errors\n\
                      \n\
                      ☑ Store MD5 checksum\n\
                        Embeds MD5 hash of audio data in the file\n\
                        Allows future integrity verification\n\
                        Essential for archival purposes\n\
                      \n\
                      ☑ Re-encode FLAC files\n\
                        Force re-encoding instead of copying FLAC files\n\
                        By default, FLAC→FLAC conversions copy without transcoding\n\
                        Check this to apply compression or processing changes\n\
                        \n\
                        ⚠️  Automatically enabled when using:\n\
                           • Sample rate changes\n\
                           • Bit depth changes\n\
                           • Dithering",
                    )
                };
                draw_help_box(f, ctx, wizard_area, title, content);
            }
            _ => {} // Other help sections not relevant for format options page
        }
    }
}

fn draw_mp3_format_options(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
) {
    let mut lines = vec![];

    lines.push(Line::from(vec![
        Span::styled(
            "Bitrate",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    // Register click area for bitrate info icon
    mouse_areas.add(
        Rect::new(area.x + "Bitrate".len() as u16 + 2, area.y, 1, 1),
        ButtonId::FormatInfoIcon(FormatSpecificHelp::Mp3Bitrate),
    );
    lines.push(Line::from("")); // Add blank line after header

    let bitrates = vec![
        "320 kbps",
        "256 kbps",
        "192 kbps",
        "128 kbps",
        "V0 (VBR ~245 kbps)",
        "V2 (VBR ~190 kbps)",
    ];

    for (i, bitrate) in bitrates.iter().enumerate() {
        let is_selected = wizard.selected_quality.as_deref() == Some(*bitrate);
        let is_focused = wizard.selected_format == Some(AudioFormat::Mp3)
            && wizard.in_quality_area
            && wizard.quality_index == i;

        let line = format_option_line(ctx, bitrate, is_selected, is_focused);
        lines.push(line);

        mouse_areas.add(
            Rect::new(area.x, area.y + 2 + i as u16, area.width, 1), // +2 for header and blank line
            ButtonId::QualityOption(i),
        );
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

fn draw_aac_format_options(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
) {
    let mut lines = vec![];

    // Profile section
    lines.push(Line::from(vec![
        Span::styled(
            "Profile",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    // Register click area for profile info icon
    mouse_areas.add(
        Rect::new(area.x + "Profile".len() as u16 + 2, area.y, 1, 1),
        ButtonId::FormatInfoIcon(FormatSpecificHelp::AacProfile),
    );
    lines.push(Line::from("")); // Add blank line after header

    let profiles = vec![AacProfile::LcAac, AacProfile::HeAac, AacProfile::HeAacV2];

    let mut y_offset = 2u16; // Start after header and blank line

    for (i, profile) in profiles.iter().enumerate() {
        let is_selected = wizard.aac_profile == Some(*profile);
        let is_focused = wizard.selected_format == Some(AudioFormat::Aac)
            && wizard.in_quality_area
            && wizard.quality_index == i;

        let line = format_option_line(ctx, &profile.to_string(), is_selected, is_focused);
        lines.push(line);

        mouse_areas.add(
            Rect::new(area.x, area.y + y_offset, area.width, 1),
            ButtonId::QualityOption(i),
        );
        y_offset += 1;
    }

    // Add spacing before bitrate section
    lines.push(Line::from(""));
    lines.push(Line::from(""));
    y_offset += 2;

    // Bitrate section
    lines.push(Line::from(vec![
        Span::styled(
            "Bitrate",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    // Register click area for bitrate info icon
    mouse_areas.add(
        Rect::new(area.x + "Bitrate".len() as u16 + 2, area.y + y_offset, 1, 1),
        ButtonId::FormatInfoIcon(FormatSpecificHelp::AacBitrate),
    );
    lines.push(Line::from("")); // Add blank line after header
    y_offset += 2;

    // Show different bitrates based on selected profile
    let bitrates = wizard.get_aac_bitrates();

    for (i, bitrate) in bitrates.iter().enumerate() {
        let is_selected = wizard.selected_quality.as_deref() == Some(*bitrate);
        let is_focused = wizard.selected_format == Some(AudioFormat::Aac)
            && wizard.in_quality_area
            && wizard.quality_index == profiles.len() + i;

        let line = format_option_line(ctx, bitrate, is_selected, is_focused);
        lines.push(line);

        mouse_areas.add(
            Rect::new(area.x, area.y + y_offset, area.width, 1),
            ButtonId::QualityOption(profiles.len() + i),
        );
        y_offset += 1;
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

fn draw_opus_format_options(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
) {
    let mut lines = vec![];

    // Debug area size

    // Quality (Bitrate) section
    lines.push(Line::from(vec![
        Span::styled(
            "Quality (Bitrate)",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    // Register click area for quality info icon
    mouse_areas.add(
        Rect::new(area.x + "Quality (Bitrate)".len() as u16 + 2, area.y, 1, 1),
        ButtonId::FormatInfoIcon(FormatSpecificHelp::OpusQuality),
    );
    lines.push(Line::from("")); // Add blank line after header

    let qualities = vec![
        ("Low", "~64-96 kbps"),
        ("Medium", "~128-160 kbps"),
        ("High", "~192-256 kbps"),
        ("Very High", "~256-320 kbps"),
        ("Insane", "~320-510 kbps"),
    ];

    let mut y_offset = 2u16; // Start after header and blank line

    for (i, (quality, bitrate)) in qualities.iter().enumerate() {
        let display_text = format!("{:<10} {}", quality, bitrate);
        let is_selected = wizard.selected_quality.as_deref() == Some(*quality);
        let is_focused = wizard.selected_format == Some(AudioFormat::Opus)
            && wizard.in_quality_area
            && wizard.quality_index == i;

        let line = format_option_line(ctx, &display_text, is_selected, is_focused);
        lines.push(line);

        mouse_areas.add(
            Rect::new(area.x, area.y + y_offset, area.width, 1),
            ButtonId::QualityOption(i),
        );
        y_offset += 1;
    }

    // Add spacing before content type section
    lines.push(Line::from(""));
    lines.push(Line::from(""));
    y_offset += 2;

    // Optimize for section
    lines.push(Line::from(vec![
        Span::styled(
            "Optimize for",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    // Register click area for content type info icon
    mouse_areas.add(
        Rect::new(
            area.x + "Optimize for".len() as u16 + 2,
            area.y + y_offset,
            1,
            1,
        ),
        ButtonId::FormatInfoIcon(FormatSpecificHelp::OpusContentType),
    );
    lines.push(Line::from("")); // Add blank line after header
    y_offset += 2;

    let content_types = vec![OpusContentType::Music, OpusContentType::Voice];

    for (i, content_type) in content_types.iter().enumerate() {
        let is_selected = wizard.opus_content_type == Some(*content_type);
        let is_focused = wizard.selected_format == Some(AudioFormat::Opus)
            && wizard.in_quality_area
            && wizard.quality_index == qualities.len() + i;

        let line = format_option_line(ctx, &content_type.to_string(), is_selected, is_focused);
        lines.push(line);

        mouse_areas.add(
            Rect::new(area.x, area.y + y_offset, area.width, 1),
            ButtonId::QualityOption(qualities.len() + i),
        );
        y_offset += 1;
    }

    // Debug: log what we're rendering

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

fn draw_wavpack_format_options(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
    wizard_area: Rect,
) {
    let mut lines = vec![];
    let mut current_y = area.y;

    // Compression Level section
    lines.push(Line::from(vec![
        Span::styled(
            "Compression Level",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    // Register click area for compression level info icon
    mouse_areas.add(
        Rect::new(area.x + "Compression Level".len() as u16 + 2, area.y, 1, 1),
        ButtonId::FormatInfoIcon(FormatSpecificHelp::WavPackCompression),
    );
    lines.push(Line::from("")); // Add blank line after header
    current_y += 2;

    // WavPack compression modes with their corresponding command-line flags:
    // These map to WavPack encoder flags as follows:
    // - Fast = -f (fast mode, ~60-70% of original size)
    // - High = -h (high quality mode, default, ~55-65% of original size)
    // - Very High = -hh (very high quality mode, ~53-63% of original size)
    // - Maximum = -hhh (best compression mode, ~52-62% of original size)
    // - Ultra = -hh -x (very high + extra processing, ~51-61% of original size)
    // - Extreme = -hh -x4 to -x6 (very high + maximum extra processing, ~50-60% of original size)
    let modes = vec![
        "Fast (Low CPU, larger files)", // -f
        "High (Balanced)",              // -h (default)
        "Very High (Smaller files)",    // -hh
        "Maximum (Best compression)",   // -hhh
        "Ultra (Very slow)",            // -hh -x
        "Extreme (Slowest, smallest)",  // -hh -x4 to -x6
    ];

    for (i, mode) in modes.iter().enumerate() {
        let is_selected = wizard.selected_quality.as_deref() == Some(*mode);
        let is_focused = wizard.selected_format == Some(AudioFormat::WavPack)
            && wizard.in_quality_area
            && wizard.quality_index == i;

        let line = format_option_line(ctx, mode, is_selected, is_focused);
        lines.push(line);

        mouse_areas.add(
            Rect::new(area.x, current_y + i as u16, area.width, 1),
            ButtonId::QualityOption(i),
        );
    }
    current_y += modes.len() as u16;

    // Add spacing between sections
    lines.push(Line::from(""));
    lines.push(Line::from(""));
    current_y += 2;

    // Additional Options section
    lines.push(Line::from(vec![
        Span::styled(
            "Additional Options",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    // Register click area for additional options info icon
    mouse_areas.add(
        Rect::new(
            area.x + "Additional Options".len() as u16 + 2,
            current_y,
            1,
            1,
        ),
        ButtonId::InfoIcon(FlacSection::ProcessingOptions), // Reusing ProcessingOptions for WavPack
    );
    lines.push(Line::from("")); // Add blank line after header
    current_y += 2;

    // Verify encoding option
    let is_verify_checked = wizard.verify_encoding.unwrap_or(true);
    let is_verify_focused = wizard.selected_format == Some(AudioFormat::WavPack)
        && wizard.in_quality_area
        && wizard.quality_index == 6;

    let verify_checkbox = if is_verify_checked { "☑" } else { "☐" };
    let verify_checkbox_style = if is_verify_checked {
        Style::default().fg(ctx.theme.accent)
    } else {
        Style::default()
    };

    let verify_text_style = if is_verify_focused {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ctx.theme.text)
    };

    let verify_line = if is_verify_focused {
        Line::from(vec![
            Span::styled(" ", verify_text_style),
            Span::styled(
                verify_checkbox,
                Style::default()
                    .fg(ctx.theme.text)
                    .bg(ctx.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Verify encoding", verify_text_style),
        ])
    } else {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(verify_checkbox, verify_checkbox_style),
            Span::raw("  "),
            Span::styled("Verify encoding", Style::default().fg(ctx.theme.text)),
        ])
    };
    lines.push(verify_line);

    // Register verify checkbox click area
    mouse_areas.add(
        Rect::new(area.x, current_y, area.width, 1),
        ButtonId::ProcessingOption(1), // Using index 1 for WavPack verify
    );
    current_y += 1;

    // Store MD5 option
    let is_checked = wizard.store_md5.unwrap_or(true);
    let is_focused = wizard.selected_format == Some(AudioFormat::WavPack)
        && wizard.in_quality_area
        && wizard.quality_index == 7; // MD5 option is now at index 7

    let checkbox = if is_checked { "☑" } else { "☐" };
    let checkbox_style = if is_checked {
        Style::default().fg(ctx.theme.accent)
    } else {
        Style::default()
    };

    let text_style = if is_focused {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ctx.theme.text)
    };

    let line = if is_focused {
        Line::from(vec![
            Span::styled(" ", text_style),
            Span::styled(
                checkbox,
                Style::default()
                    .fg(ctx.theme.text)
                    .bg(ctx.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Store MD5 checksum", text_style),
        ])
    } else {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(checkbox, checkbox_style),
            Span::raw("  "),
            Span::styled("Store MD5 checksum", text_style),
        ])
    };
    lines.push(line);

    // Register MD5 checkbox click area
    mouse_areas.add(
        Rect::new(area.x, current_y, area.width, 1),
        ButtonId::ProcessingOption(3), // Using index 3 for WavPack MD5
    );

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);

    // Show help popup if info icon was clicked
    if wizard.show_help_for == Some(FlacSection::ProcessingOptions) {
        draw_help_box(
            f,
                ctx,
            wizard_area,
            "WavPack Additional Options",
            "Additional options for WavPack encoding:\n\n\
             \n\
             ☑ Verify encoding\n\
               Verifies the output while encoding\n\
               Ensures perfect bit-for-bit accuracy\n\
               Catches any encoding errors immediately\n\
               Small performance overhead (~10%)\n\n\
             ☑ Store MD5 checksum\n\
               Embeds whole-file MD5 in metadata\n\
               Allows verification of file integrity later\n\
               Essential for archival purposes\n\
               Tiny space overhead (16 bytes)\n\n\
             💡 Both options are recommended for\n\
                important archives and backups.\n\
                The performance impact is minimal\n\
                on modern systems.",
        );
    }
}

fn draw_quality_options(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
    wizard_area: Rect,
) {
    // This page shows resampling/quality options applicable to all formats
    match wizard.selected_format {
        Some(AudioFormat::Flac)
        | Some(AudioFormat::Wav)
        | Some(AudioFormat::Aiff)
        | Some(AudioFormat::WavPack) => {
            // Lossless formats can use bit depth, sample rate, dithering, and resampling
            draw_resampling_options(f, ctx, area, wizard, mouse_areas, wizard_area);
        }
        Some(AudioFormat::Mp3) | Some(AudioFormat::Aac) | Some(AudioFormat::Opus) => {
            // Lossy formats only need resampling if sample rate changes
            draw_lossy_quality_options(f, ctx, area, wizard, mouse_areas, wizard_area);
        }
        None => {
            let paragraph =
                Paragraph::new("Please select a format first.").alignment(Alignment::Center);
            f.render_widget(paragraph, area);
        }
    }
}

fn draw_resampling_options(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
    wizard_area: Rect,
) {
    // Debug log

    // Split the area into two columns
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let left_area = chunks[0];
    let right_area = chunks[1];

    // Add padding to each column
    let left_padded = Rect::new(
        left_area.x + 6,
        left_area.y,
        left_area.width.saturating_sub(8),
        left_area.height,
    );

    let right_padded = Rect::new(
        right_area.x + 2,
        right_area.y,
        right_area.width.saturating_sub(8),
        right_area.height,
    );

    // LEFT COLUMN: Bit Depth and Dithering
    let mut left_lines = vec![];
    let mut left_y = left_padded.y;

    // Bit Depth section
    left_lines.push(Line::from(vec![
        Span::styled(
            "Bit Depth",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    left_lines.push(Line::from("")); // Add blank line after header

    // Register info icon click area
    mouse_areas.add(
        Rect::new(left_padded.x + 11, left_y, 1, 1),
        ButtonId::InfoIcon(FlacSection::BitDepth),
    );
    left_y += 2; // Move past header and blank line

    let bit_depth_options = SimpleWizard::get_bit_depth_options();
    for (i, (value, label)) in bit_depth_options.iter().enumerate() {
        let is_selected = wizard.bit_depth == Some(*value);
        let is_focused =
            wizard.resampling_page_section == FlacSection::BitDepth && wizard.selected_index == i;

        let line = format_option_line(ctx, label, is_selected, is_focused);
        left_lines.push(line);

        mouse_areas.add(
            Rect::new(left_padded.x, left_y + i as u16, left_padded.width, 1),
            ButtonId::BitDepthOption(i),
        );
    }
    left_y += bit_depth_options.len() as u16;

    // Add spacing
    left_lines.push(Line::from(""));
    left_lines.push(Line::from(""));
    left_y += 2;

    // Dithering options (only show if applicable)
    if wizard.should_show_dithering() {
        left_lines.push(Line::from(vec![
            Span::styled(
                "Dithering",
                Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            ),
            Span::raw("  "),
            Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
        ]));
        left_lines.push(Line::from("")); // Add blank line after header

        // Register info icon click area
        mouse_areas.add(
            Rect::new(left_padded.x + 11, left_y, 1, 1),
            ButtonId::InfoIcon(FlacSection::Dithering),
        );
        left_y += 2;

        let dither_options = wizard.get_dither_options();
        for (i, dither_type) in dither_options.iter().enumerate() {
            let is_selected = wizard.dither_type == Some(*dither_type);
            let is_focused = wizard.resampling_page_section == FlacSection::Dithering
                && wizard.selected_index == i;

            let line = format_option_line(ctx, &dither_type.to_string(), is_selected, is_focused);
            left_lines.push(line);

            mouse_areas.add(
                Rect::new(left_padded.x, left_y + i as u16, left_padded.width, 1),
                ButtonId::DitherOption(i),
            );
        }
    }

    // RIGHT COLUMN: Sample Rate and Resampling Quality
    let mut right_lines = vec![];
    let mut right_y = right_padded.y;

    // Sample Rate section
    right_lines.push(Line::from(vec![
        Span::styled(
            "Sample Rate",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    right_lines.push(Line::from("")); // Add blank line after header

    // Register info icon click area
    mouse_areas.add(
        Rect::new(right_padded.x + 13, right_y, 1, 1),
        ButtonId::InfoIcon(FlacSection::SampleRate),
    );
    right_y += 2; // Move past header and blank line

    let sample_rate_options = wizard.get_sample_rate_options_for_format();
    for (i, (value, label)) in sample_rate_options.iter().enumerate() {
        let is_selected = wizard.sample_rate == Some(*value);
        let is_focused =
            wizard.resampling_page_section == FlacSection::SampleRate && wizard.selected_index == i;

        let line = format_option_line(ctx, label, is_selected, is_focused);
        right_lines.push(line);

        mouse_areas.add(
            Rect::new(right_padded.x, right_y + i as u16, right_padded.width, 1),
            ButtonId::SampleRateOption(i),
        );
    }
    right_y += sample_rate_options.len() as u16;

    // Add spacing
    right_lines.push(Line::from(""));
    right_lines.push(Line::from(""));
    right_y += 2;

    // Resampling Quality (only show if sample rate is changing)
    if wizard.should_show_resampling() {
        // Debug log
        right_lines.push(Line::from(vec![
            Span::styled(
                "Resampling Quality",
                Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            ),
            Span::raw("  "),
            Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
        ]));
        right_lines.push(Line::from("")); // Add blank line after header

        // Register info icon click area
        mouse_areas.add(
            Rect::new(right_padded.x + 20, right_y, 1, 1),
            ButtonId::InfoIcon(FlacSection::ResamplingQuality),
        );
        right_y += 2;

        let resample_options = SimpleWizard::get_resample_quality_options();
        for (i, (value, label)) in resample_options.iter().enumerate() {
            let is_selected = wizard.resample_quality == Some(*value);
            let is_focused = wizard.resampling_page_section == FlacSection::ResamplingQuality
                && wizard.selected_index == i;

            let line = format_option_line(ctx, label, is_selected, is_focused);
            right_lines.push(line);

            mouse_areas.add(
                Rect::new(right_padded.x, right_y + i as u16, right_padded.width, 1),
                ButtonId::ResampleQualityOption(i),
            );
        }
        right_y += resample_options.len() as u16;

        // Add spacing
        right_lines.push(Line::from(""));
        right_lines.push(Line::from(""));
        right_y += 2;

        // Nyquist Transition section
        // Debug log

        right_lines.push(Line::from(vec![
            Span::styled(
                "Nyquist Transition",
                Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            ),
            Span::raw("  "),
            Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
        ]));
        right_lines.push(Line::from("")); // Add blank line after header

        // Register info icon click area - "Nyquist Transition" = 18 chars + 2 spaces = 20
        mouse_areas.add(
            Rect::new(right_padded.x + 20, right_y, 1, 1),
            ButtonId::InfoIcon(FlacSection::NyquistTransition),
        );
        right_y += 2;

        let nyquist_options = SimpleWizard::get_nyquist_transition_options();
        for (i, transient) in nyquist_options.iter().enumerate() {
            let is_selected = wizard.nyquist_transition == Some(*transient);
            let is_focused = wizard.resampling_page_section == FlacSection::NyquistTransition
                && wizard.selected_index == i;

            let line = format_option_line(ctx, &transient.to_string(), is_selected, is_focused);
            right_lines.push(line);

            mouse_areas.add(
                Rect::new(right_padded.x, right_y + i as u16, right_padded.width, 1),
                ButtonId::NyquistTransitionOption(i),
            );
        }
        right_y += nyquist_options.len() as u16;

        // Add Insane mode checkbox (only when Brick Wall is selected)
        if wizard.is_insane_mode_available() {
            right_lines.push(Line::from("")); // Blank line
            right_y += 1;

            let insane_enabled = wizard.ssrc_insane_mode.unwrap_or(false);

            let checkbox = if insane_enabled { "☑" } else { "☐" };
            let checkbox_style = if insane_enabled {
                Style::default().fg(ctx.theme.accent)
            } else {
                Style::default()
            };

            let text_style = Style::default().fg(ctx.theme.text);

            right_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(checkbox, checkbox_style),
                Span::styled("  ⚠️  Enable 'Insane' Profile (Slow)", text_style),
            ]));

            mouse_areas.add(
                Rect::new(right_padded.x, right_y, right_padded.width, 1),
                ButtonId::SsrcInsaneCheckbox,
            );
        }
    } else {
        // Debug log
        right_lines.push(Line::from(Span::styled(
            "Resampling is not needed when",
            Style::default().fg(ctx.theme.text_dim),
        )));
        right_lines.push(Line::from(Span::styled(
            "keeping the same sample rate.",
            Style::default().fg(ctx.theme.text_dim),
        )));
    }

    // Render both columns
    let left_paragraph = Paragraph::new(left_lines);
    f.render_widget(left_paragraph, left_padded);

    // Debug log

    let right_paragraph = Paragraph::new(right_lines.clone()).wrap(Wrap { trim: true });
    f.render_widget(right_paragraph, right_padded);

    // Show help popups for this page
    if let Some(section) = wizard.show_help_for {
        match section {
            FlacSection::SampleRate => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "Sample Rate",
                    "The sample rate determines how many times per second\n\
                     the audio is sampled. Common rates:\n\n\
                     • 44.1 kHz - CD quality, standard for music\n\
                     • 48 kHz - Professional audio, video production\n\
                     • 88.2 kHz - 2x CD quality, high-resolution\n\n\
                     Higher rates capture more detail but create larger files.\n\
                     Most people can't hear differences above 48 kHz.\n\n\
                     ⚠️  Downsampling (reducing sample rate) is lossy!\n\
                        Once reduced, quality cannot be restored.",
                );
            }
            FlacSection::ResamplingQuality => {
                let help_text = if wizard.nyquist_transition == Some(NyquistTransition::BrickWall) {
                    "Resampling Quality (OVERRIDDEN BY SSRC):\n\n\
                     ⚠️  These settings are IGNORED when using\n\
                        Brick Wall (SSRC) Nyquist Transition!\n\n\
                     When using SoX resampling:\n\
                     • Ultra (rate -u) - Best quality\n\
                     • VHQ (rate -v) - Very High Quality\n\
                     • HQ (rate -h) - High Quality (default)\n\
                     • MQ (rate -m) - Medium Quality, fast\n\n\
                     💡 To use these settings, change Nyquist\n\
                        Transition to Gentle or Steep."
                } else {
                    "Resampling Quality:\n\n\
                     SoX resampling quality when changing sample rates.\n\
                     Higher quality = better sound but slower processing.\n\n\
                     • Ultra (rate -u) - Best quality (undocumented flag)\n\
                     • VHQ (rate -v) - Very High Quality\n\
                     • HQ (rate -h) - High Quality (SoX default)\n\
                     • MQ (rate -m) - Medium Quality, fast\n\n\
                     💡 Ultra recommended for archival work."
                };
                draw_help_box(f, ctx, wizard_area, "Resampling Quality", help_text);
            }
            FlacSection::BitDepth => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "Bit Depth",
                    "Bit depth determines the dynamic range and noise floor.\n\
                     Higher bit depth = more dynamic range, lower noise.\n\n\
                     • 32-bit float - Floating point, huge dynamic range\n\
                       Used in professional mixing/mastering\n\
                       No clipping possible during processing\n\n\
                     • 32-bit - Very high dynamic range (192 dB)\n\
                       Rarely needed for distribution\n\n\
                     • 24-bit - Professional standard (144 dB range)\n\
                       Excellent for recording and mastering\n\n\
                     • 16-bit - CD standard (96 dB range)\n\
                       Perfect for final distribution\n\n\
                     ⚠️  Reducing bit depth is lossy! Use dithering\n\
                        when converting to lower bit depths.",
                );
            }
            FlacSection::Dithering => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "Dithering",
                    "Dithering adds tiny amounts of noise to prevent\n\
                     quantization distortion when reducing bit depth.\n\n\
                     For 16-bit output:\n\
                     • None - No dithering (not recommended)\n\
                     • TPDF - Standard triangular dither\n\
                     • Shibata - Psychoacoustically optimized (RECOMMENDED)\n\
                     • Low/High Shibata - Variations for different content\n\
                     • Gesemann - Alternative psychoacoustic shaping\n\n\
                     For 24-bit output:\n\
                     • None - Often acceptable at 24-bit\n\
                     • TPDF - Safe choice if dithering\n\
                     • Sloped TPDF - Shaped for better noise spectrum\n\n\
                     💡 Always use dithering when converting to 16-bit!",
                );
            }
            FlacSection::NyquistTransition => {
                let help_text = if wizard.nyquist_transition == Some(NyquistTransition::BrickWall) {
                    "Nyquist Transition Filter:\n\n\
                     Controls how sharply frequencies are cut off at the\n\
                     Nyquist frequency (half the sample rate).\n\n\
                     • Gentle (95%) - DEFAULT\n\
                       Gradual roll-off, minimal pre-ringing\n\
                       Best for most music\n\n\
                     • Steep (99.7%)\n\
                       Sharper cutoff, some pre-ringing\n\
                       Good for classical/acoustic\n\n\
                     • Brick Wall (SSRC) - SELECTED\n\
                       Uses SSRC resampler instead of SoX\n\
                       Extremely sharp cutoff at Nyquist\n\
                       ⚠️  OVERRIDES SoX quality settings!\n\n\
                     ⚠️  INSANE MODE:\n\
                     When Brick Wall is selected, you can enable\n\
                     Insane mode for maximum quality (200 dB\n\
                     attenuation, 262144 FFT). This is EXTREMELY\n\
                     slow and should only be used for archival\n\
                     purposes where absolute quality is required.\n\n\
                     💡 When SSRC is selected, the SoX resampling\n\
                        quality options above are ignored."
                } else {
                    "Nyquist Transition Filter:\n\n\
                     Controls how sharply frequencies are cut off at the\n\
                     Nyquist frequency (half the sample rate).\n\n\
                     • Gentle (95%) - DEFAULT\n\
                       Gradual roll-off, minimal pre-ringing\n\
                       Best for most music\n\n\
                     • Steep (99.7%)\n\
                       Sharper cutoff, some pre-ringing\n\
                       Good for classical/acoustic\n\n\
                     • Brick Wall (SSRC)\n\
                       Uses SSRC resampler instead of SoX\n\
                       Extremely sharp cutoff at Nyquist\n\
                       May introduce more pre-ringing\n\n\
                     💡 Gentle is recommended for most content.\n\
                        Only use steeper settings if you need\n\
                        maximum frequency preservation."
                };
                draw_help_box(f, ctx, wizard_area, "Nyquist Transition", help_text);
            }
            _ => {} // Other help sections not relevant for this page
        }
    }
}
fn draw_lossy_quality_options(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
    wizard_area: Rect,
) {
    // For lossy formats, we only show sample rate and resampling options if needed
    let padded_area = Rect::new(
        area.x + 6,
        area.y,
        area.width.saturating_sub(12),
        area.height,
    );

    let mut lines = vec![];
    lines.push(Line::from(Span::styled(
        "Quality Settings",
        Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
    )));
    lines.push(Line::from(""));

    // Sample Rate (optional - only if changing from source)
    lines.push(Line::from(vec![
        Span::styled(
            "Sample Rate",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw("  "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    lines.push(Line::from("")); // Add blank line after header

    mouse_areas.add(
        Rect::new(padded_area.x + 13, padded_area.y + 2, 1, 1),
        ButtonId::InfoIcon(FlacSection::SampleRate),
    );

    let sample_rate_options = wizard.get_sample_rate_options_for_format();
    for (i, (value, label)) in sample_rate_options.iter().enumerate() {
        let is_selected = wizard.sample_rate == Some(*value);
        let is_focused =
            wizard.resampling_page_section == FlacSection::SampleRate && wizard.selected_index == i;

        let line = format_option_line(ctx, label, is_selected, is_focused);
        lines.push(line);

        mouse_areas.add(
            Rect::new(
                padded_area.x,
                padded_area.y + 4 + i as u16,
                padded_area.width,
                1,
            ), // +4 for header, blank line, and "Quality Settings"
            ButtonId::SampleRateOption(i),
        );
    }

    // Show resampling quality only if sample rate is changed
    if wizard.should_show_resampling() {
        lines.push(Line::from("")); // Add 2 blank lines between sections
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "Resampling Quality",
                Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            ),
            Span::raw("  "),
            Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
        ]));
        lines.push(Line::from("")); // Add blank line after header

        // Calculate Y position:
        // +2 for "Quality Settings" and blank line
        // +2 for "Sample Rate" header and blank line
        // + sample_rate_options.len() for the options
        // +2 for the two blank lines before "Resampling Quality"
        let resample_y = padded_area.y + 2 + 2 + sample_rate_options.len() as u16 + 2;
        mouse_areas.add(
            Rect::new(padded_area.x + 20, resample_y, 1, 1),
            ButtonId::InfoIcon(FlacSection::ResamplingQuality),
        );

        let resample_options = SimpleWizard::get_resample_quality_options();
        for (i, (value, label)) in resample_options.iter().enumerate() {
            let is_selected = wizard.resample_quality == Some(*value);
            let is_focused = wizard.resampling_page_section == FlacSection::ResamplingQuality
                && wizard.selected_index == i;

            let line = format_option_line(ctx, label, is_selected, is_focused);
            lines.push(line);

            mouse_areas.add(
                Rect::new(
                    padded_area.x,
                    resample_y + 2 + i as u16,
                    padded_area.width,
                    1,
                ), // +2 for header and blank line
                ButtonId::ResampleQualityOption(i),
            );
        }

        // Add Nyquist Transition for Opus only
        if wizard.selected_format == Some(AudioFormat::Opus) {
            lines.push(Line::from("")); // Add 2 blank lines between sections
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "Nyquist Transition",
                    Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
                ),
                Span::raw("  "),
                Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
            ]));
            lines.push(Line::from("")); // Add blank line after header

            // Calculate Y position for Nyquist:
            // resample_y is at the "Resampling Quality" header
            // +2 for header and blank line
            // + resample_options.len() for the resampling options
            // +2 for the two blank lines before "Nyquist Transition"
            let nyquist_y = resample_y + 2 + resample_options.len() as u16 + 2;
            mouse_areas.add(
                Rect::new(padded_area.x + 20, nyquist_y, 1, 1),
                ButtonId::InfoIcon(FlacSection::NyquistTransition),
            );

            let nyquist_options = SimpleWizard::get_nyquist_transition_options();
            for (i, nyquist_type) in nyquist_options.iter().enumerate() {
                let is_selected = wizard.nyquist_transition == Some(*nyquist_type);
                let is_focused = wizard.resampling_page_section == FlacSection::NyquistTransition
                    && wizard.selected_index == i;

                let line = format_option_line(ctx, &nyquist_type.to_string(), is_selected, is_focused);
                lines.push(line);

                mouse_areas.add(
                    Rect::new(
                        padded_area.x,
                        nyquist_y + 2 + i as u16,
                        padded_area.width,
                        1,
                    ),
                    ButtonId::NyquistTransitionOption(i),
                );
            }

            // Add Insane mode checkbox (only when Brick Wall is selected)
            if wizard.is_insane_mode_available() {
                lines.push(Line::from("")); // Blank line

                let insane_enabled = wizard.ssrc_insane_mode.unwrap_or(false);
                let checkbox = if insane_enabled { "☑" } else { "☐" };
                let checkbox_style = if insane_enabled {
                    Style::default().fg(ctx.theme.accent)
                } else {
                    Style::default()
                };
                let text_style = Style::default().fg(ctx.theme.text);

                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(checkbox, checkbox_style),
                    Span::styled("  ⚠️  Enable 'Insane' Profile (Slow)", text_style),
                ]));

                // Y position: nyquist header (nyquist_y) + header+blank (2) + options (3) + blank line (1)
                let insane_y = nyquist_y + 2 + nyquist_options.len() as u16 + 1;
                mouse_areas.add(
                    Rect::new(padded_area.x, insane_y, padded_area.width, 1),
                    ButtonId::SsrcInsaneCheckbox,
                );
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Note: Lossy formats have format-specific quality",
        Style::default().fg(ctx.theme.text_dim),
    )));
    lines.push(Line::from(Span::styled(
        "settings on the previous page.",
        Style::default().fg(ctx.theme.text_dim),
    )));

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, padded_area);

    // Show help popups if needed
    if let Some(help_section) = wizard.show_help_for {
        match help_section {
            FlacSection::SampleRate => {
                let help_text = match wizard.selected_format {
                    Some(AudioFormat::Mp3) => {
                        "MP3 Sample Rate Options:\n\n\
                         ◉ Same as source (RECOMMENDED)\n\
                           Lets the encoder handle resampling if needed\n\
                           MP3 will automatically downsample to 48 kHz max\n\
                         \n\
                         ◉ 44.1 kHz - CD standard\n\
                         ◉ 48 kHz - Maximum MP3 supports\n\n\
                         💡 MP3 doesn't support rates above 48 kHz.\n\
                            High-res sources will be downsampled."
                    }
                    Some(AudioFormat::Aac) => {
                        "AAC Sample Rate Options:\n\n\
                         ◉ Same as source (RECOMMENDED)\n\
                           Lets the encoder handle resampling if needed\n\
                         \n\
                         ◉ 44.1 kHz - CD standard\n\
                         ◉ 48 kHz - Professional standard\n\
                         ◉ 88.2 kHz - 2x CD quality\n\
                         ◉ 96 kHz - Studio quality\n\
                         ◉ 176.4 kHz - 4x CD quality\n\
                         ◉ 192 kHz - Maximum supported\n\n\
                         💡 Fraunhofer AAC supports up to 192 kHz.\n\
                            Higher sources will be downsampled."
                    }
                    Some(AudioFormat::Opus) => {
                        "Opus Sample Rate Options:\n\n\
                         ◉ Same as source (RECOMMENDED)\n\
                           Uses Opus's built-in resampling\n\
                           Opus internally always uses 48 kHz\n\
                         \n\
                         ◉ 48 kHz (override built-in resampling)\n\
                           Use SoX's high-quality resampler instead\n\
                           May give slightly better results\n\n\
                         💡 Opus always outputs 48 kHz internally.\n\
                            The choice is which resampler to use."
                    }
                    _ => {
                        "Sample rate determines how many times per second the audio\n\
                         is sampled. Higher rates can capture higher frequencies.\n\n\
                         ◉ Same as source (RECOMMENDED)\n\
                           Preserves original sample rate, no resampling needed\n\
                         \n\
                         ◉ 44.1 kHz - CD standard\n\
                         ◉ 48 kHz - Professional/DVD standard\n\
                         ◉ 88.2 kHz - High-resolution (2x CD quality)\n\n\
                         ⚠️  Downsampling requires high-quality resampling.\n\
                            Upsampling does NOT improve quality!"
                    }
                };
                draw_help_box(f, ctx, wizard_area, "Sample Rate Help", help_text);
            }
            FlacSection::ResamplingQuality => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "Resampling Quality Help",
                    "SoX resampling quality when changing sample rates.\n\
                     Higher quality = better sound but slower processing.\n\n\
                     \n\
                     ◉ Ultra (rate -u) - RECOMMENDED\n\
                       Best quality, uses undocumented SoX flag\n\
                     \n\
                     ◉ VHQ (rate -v)\n\
                       Very High Quality, faster than Ultra\n\
                     \n\
                     ◉ HQ (rate -h)\n\
                       High Quality, SoX default setting\n\
                     \n\
                     ◉ MQ (rate -m)\n\
                       Medium Quality, fast processing\n\n\
                     💡 Only applies when sample rate is changed.\n\
                        Use Ultra for archival work.",
                );
            }
            FlacSection::NyquistTransition => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "Nyquist Transition Help",
                    "Anti-aliasing filter steepness during resampling.\n\
                     Controls how much high frequency content is preserved.\n\n\
                     ◉ Gentle (95%) - Smooth rolloff, preserves transients\n\
                     ◉ Steep (99.7%) - Sharp cutoff, minimal aliasing\n\
                     ◉ Brick Wall (SSRC) - Uses different resampler\n\n\
                     💡 Gentle preserves more high-frequency content.\n\
                        Steep prevents aliasing artifacts better.\n\
                        SSRC overrides quality setting above.",
                );
            }
            _ => {}
        }
    }
}

fn draw_additional_options(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    mouse_areas: &mut MouseAreas,
    wizard_area: Rect,
) {
    // Add left padding
    let padded_area = Rect::new(
        area.x + 6,
        area.y,
        area.width.saturating_sub(6),
        area.height,
    );

    let mut lines = vec![];

    // ReplayGain mode with info icon
    let header_text = "ReplayGain scan mode:";
    let header_len = header_text.len();
    lines.push(Line::from(vec![
        Span::styled(
            header_text,
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw(" "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    lines.push(Line::from("")); // Add blank line after header

    // Add click area for info icon - header is at line 0
    mouse_areas.add(
        Rect::new(padded_area.x + header_len as u16 + 1, padded_area.y, 1, 1),
        ButtonId::AdditionalInfoIcon(AdditionalOptionsHelp::ReplayGain),
    );

    let replaygain_modes = vec![
        (
            ReplayGainMode::Album,
            "Album mode (consistent volume across album)",
        ),
        (
            ReplayGainMode::Track,
            "Track mode (consistent volume per track)",
        ),
        (
            ReplayGainMode::Both,
            "Both (scan and tag for both album and track)",
        ),
        (ReplayGainMode::Off, "Off (no ReplayGain scanning)"),
    ];

    for (i, (mode, desc)) in replaygain_modes.into_iter().enumerate() {
        let is_selected = wizard.replaygain_mode == Some(mode);
        let is_focused = wizard.additional_options_index == i;

        let line = format_option_line(ctx, desc, is_selected, is_focused);
        lines.push(line);

        // Options start at line 2 (after header and blank line)
        mouse_areas.add(
            Rect::new(
                padded_area.x,
                padded_area.y + 2 + i as u16,
                padded_area.width,
                1,
            ),
            ButtonId::AdditionalOption(i),
        );
    }

    // Now at line 6 (0=header, 1=blank, 2-5=options)
    lines.push(Line::from("")); // line 6
    lines.push(Line::from("")); // line 7
    lines.push(Line::from(vec![
        // line 8
        Span::styled(
            "After converting:",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw(" "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));
    // Add click area for After converting info icon
    let copy_header_len = "After converting:".len();
    mouse_areas.add(
        Rect::new(
            padded_area.x + copy_header_len as u16 + 1,
            padded_area.y + 8,
            1,
            1,
        ),
        ButtonId::AdditionalInfoIcon(AdditionalOptionsHelp::CopyFiles),
    );
    lines.push(Line::from("")); // line 9 - blank line after header

    // Copy files field with checkbox
    let is_copy_files_focused = wizard.additional_options_index == 4;
    let copy_files_checkbox = if wizard.copy_files_enabled {
        "☑"
    } else {
        "☐"
    };
    let checkbox_style = if wizard.copy_files_enabled {
        Style::default().fg(ctx.theme.accent)
    } else {
        Style::default()
    };

    let field_style = if is_copy_files_focused {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ctx.theme.text)
    };

    let truncated_text = if wizard.copy_files_extensions.len() > 50 {
        format!("{}...", &wizard.copy_files_extensions[..47])
    } else {
        wizard.copy_files_extensions.clone()
    };

    let copy_files_line = if is_copy_files_focused {
        Line::from(vec![
            Span::styled(" ", field_style),
            Span::styled(
                copy_files_checkbox,
                Style::default()
                    .fg(ctx.theme.text)
                    .bg(ctx.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Copy files: [", field_style),
            Span::styled(truncated_text, field_style),
            Span::styled("]", field_style),
        ])
    } else {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(copy_files_checkbox, checkbox_style),
            Span::raw("  Copy files: ["),
            Span::styled(
                wizard.copy_files_extensions.clone(),
                Style::default().fg(ctx.theme.accent),
            ),
            Span::raw("]"),
        ])
    };

    lines.push(copy_files_line); // line 10
                                 // Register checkbox click area (first 3 characters: " ☑ ")
    mouse_areas.add(
        Rect::new(padded_area.x, padded_area.y + 10, 3, 1),
        ButtonId::AdditionalOptionCheckbox(4),
    );
    // Register field click area (rest of the line minus info icon)
    mouse_areas.add(
        Rect::new(
            padded_area.x + 3,
            padded_area.y + 10,
            padded_area.width.saturating_sub(6),
            1,
        ),
        ButtonId::AdditionalOption(4),
    );
    // Register info icon click area (last 3 chars for "  ⓘ")
    let info_x = padded_area.x + padded_area.width.saturating_sub(3);
    mouse_areas.add(
        Rect::new(info_x, padded_area.y + 10, 3, 1),
        ButtonId::AdditionalInfoIcon(AdditionalOptionsHelp::CopyFiles),
    );

    // Copy subdirectories field with checkbox
    let is_copy_subdirs_focused = wizard.additional_options_index == 5;
    let copy_subdirs_checkbox = if wizard.copy_subdirectories_enabled {
        "☑"
    } else {
        "☐"
    };
    let subdirs_checkbox_style = if wizard.copy_subdirectories_enabled {
        Style::default().fg(ctx.theme.accent)
    } else {
        Style::default()
    };

    let field_style = if is_copy_subdirs_focused {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ctx.theme.text)
    };

    let display_text = if wizard.copy_subdirectories.is_empty() {
        "".to_string()
    } else {
        wizard.copy_subdirectories.clone()
    };

    let subdirs_line = if is_copy_subdirs_focused {
        Line::from(vec![
            Span::styled(" ", field_style),
            Span::styled(
                copy_subdirs_checkbox,
                Style::default()
                    .fg(ctx.theme.text)
                    .bg(ctx.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Copy subdirectories: [", field_style),
            Span::styled(display_text, field_style),
            Span::styled("]", field_style),
        ])
    } else {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(copy_subdirs_checkbox, subdirs_checkbox_style),
            Span::raw("  Copy subdirectories: ["),
            Span::styled(
                wizard.copy_subdirectories.clone(),
                Style::default().fg(ctx.theme.accent),
            ),
            Span::raw("]"),
        ])
    };

    lines.push(subdirs_line); // line 11
                              // Register checkbox click area (first 3 characters: " ☑ ")
    mouse_areas.add(
        Rect::new(padded_area.x, padded_area.y + 11, 3, 1),
        ButtonId::AdditionalOptionCheckbox(5),
    );
    // Register field click area (rest of the line)
    mouse_areas.add(
        Rect::new(
            padded_area.x + 3,
            padded_area.y + 11,
            padded_area.width.saturating_sub(3),
            1,
        ),
        ButtonId::AdditionalOption(5),
    );

    // Merge all tracks option - moved here, right after Copy subdirectories
    let merge_option = (
        "Merge all tracks into single file",
        wizard.merge_to_single.unwrap_or(false),
    );
    let is_focused = wizard.additional_options_index == 6;
    let checkbox = if merge_option.1 { "☑" } else { "☐" };
    let checkbox_style = if merge_option.1 {
        Style::default().fg(ctx.theme.accent)
    } else {
        Style::default()
    };

    let text_style = if is_focused {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ctx.theme.text)
    };

    let line = if is_focused {
        Line::from(vec![
            Span::styled(" ", text_style),
            Span::styled(checkbox, Style::default().fg(ctx.theme.selected_fg).bg(ctx.theme.accent)),
            Span::styled(format!("  {}", merge_option.0), text_style),
        ])
    } else {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(checkbox, checkbox_style),
            Span::raw("  "),
            Span::styled(merge_option.0.to_string(), text_style),
        ])
    };

    lines.push(line); // line 12

    mouse_areas.add(
        Rect::new(padded_area.x, padded_area.y + 12, padded_area.width, 1),
        ButtonId::AdditionalOption(6),
    );

    // Add spacing before destination option
    lines.push(Line::from("")); // line 13
    lines.push(Line::from("")); // line 14

    // Destination option
    lines.push(Line::from(vec![
        // line 15
        Span::styled(
            "Destination:",
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::raw(" "),
        Span::styled("ⓘ", Style::default().fg(ctx.theme.accent)),
    ]));

    // Add click area for destination info icon
    let destination_header_len = "Destination:".len();
    mouse_areas.add(
        Rect::new(
            padded_area.x + destination_header_len as u16 + 1,
            padded_area.y + 15,
            1,
            1,
        ),
        ButtonId::AdditionalInfoIcon(AdditionalOptionsHelp::SourceFiles),
    );
    lines.push(Line::from("")); // line 16 - blank line after header

    // Ask every time radio button
    let is_ask_focused = wizard.additional_options_index == 7;
    let is_ask_selected = matches!(wizard.destination_mode, DestinationMode::AskEveryTime);
    let radio_ask = if is_ask_selected { "◉" } else { "○" };

    let ask_style = if is_ask_focused {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ctx.theme.text)
    };

    let ask_line = if is_ask_focused {
        Line::from(vec![
            Span::styled(" ", ask_style),
            Span::styled(radio_ask, Style::default().fg(ctx.theme.selected_fg).bg(ctx.theme.accent)),
            Span::styled("  Ask every time", ask_style),
        ])
    } else {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                radio_ask,
                if is_ask_selected {
                    Style::default().fg(ctx.theme.accent)
                } else {
                    Style::default()
                },
            ),
            Span::raw("  "),
            Span::styled("Ask every time", ask_style),
        ])
    };

    lines.push(ask_line); // line 17

    mouse_areas.add(
        Rect::new(padded_area.x, padded_area.y + 17, padded_area.width, 1),
        ButtonId::AdditionalOption(7),
    );

    // Custom path radio button
    let is_custom_focused = wizard.additional_options_index == 8;
    let is_custom_selected = matches!(wizard.destination_mode, DestinationMode::Custom(_));
    let radio_custom = if is_custom_selected { "◉" } else { "○" };

    let custom_style = if is_custom_focused {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ctx.theme.text)
    };

    // Get the custom path if it exists, or show placeholder
    let (custom_path, is_placeholder) =
        if let DestinationMode::Custom(path) = &wizard.destination_mode {
            if path.is_empty() {
                ("./output".to_string(), true)
            } else {
                (path.clone(), false)
            }
        } else {
            ("./output".to_string(), true)
        };

    // For the custom line, we need to calculate field width to place the Browse button
    let radio_and_label = " ○  Custom: [";
    let field_content = &custom_path;
    let field_end = "]";
    let browse_button = " Browse ";

    // Calculate positions
    let radio_label_len = radio_and_label.len();
    let field_width = 30; // Fixed width for the path field
    let browse_button_start = radio_label_len + field_width + field_end.len() + 1; // +1 for space

    let custom_line = if is_custom_focused {
        let mut spans = vec![
            Span::styled(" ", custom_style),
            Span::styled(
                radio_custom,
                Style::default().fg(ctx.theme.selected_fg).bg(ctx.theme.accent),
            ),
            Span::styled("  Custom: [", custom_style),
        ];

        // Add the path, truncated if needed
        let display_path = if field_content.len() > field_width {
            format!(
                "...{}",
                &field_content[field_content.len() - (field_width - 3)..]
            )
        } else {
            format!("{:<width$}", field_content, width = field_width)
        };
        spans.push(Span::styled(display_path, custom_style));
        spans.push(Span::styled("]", custom_style));

        // Add space before Browse button
        spans.push(Span::raw(" "));

        // Add Browse button - highlight if focused AND Custom is selected
        let browse_style = if wizard.browse_button_focused && is_custom_selected {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.warning)
                .add_modifier(Modifier::BOLD) // focused action emphasis
        } else if wizard.hovered_button == Some(ButtonId::BrowseButton) {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.surface)
                .add_modifier(Modifier::BOLD) // hovered action emphasis
        } else {
            Style::default().fg(ctx.theme.text).bg(ctx.theme.surface)
        };
        spans.push(Span::styled(browse_button, browse_style));

        Line::from(spans)
    } else {
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(
                radio_custom,
                if is_custom_selected {
                    Style::default().fg(ctx.theme.accent)
                } else {
                    Style::default()
                },
            ),
            Span::raw("  Custom: ["),
        ];

        // Add the path, truncated if needed
        let display_path = if field_content.len() > field_width {
            format!(
                "...{}",
                &field_content[field_content.len() - (field_width - 3)..]
            )
        } else {
            format!("{:<width$}", field_content, width = field_width)
        };
        spans.push(Span::styled(
            display_path,
            if is_placeholder {
                Style::default().fg(ctx.theme.text_dim)
            } else {
                Style::default().fg(ctx.theme.accent)
            },
        ));
        spans.push(Span::raw("]"));

        // Add space before Browse button
        spans.push(Span::raw(" "));

        // Add Browse button
        let browse_style = if wizard.hovered_button == Some(ButtonId::BrowseButton) {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.surface)
                .add_modifier(Modifier::BOLD) // hovered action emphasis
        } else {
            Style::default().fg(ctx.theme.text).bg(ctx.theme.surface)
        };
        spans.push(Span::styled(browse_button, browse_style));

        Line::from(spans)
    };

    lines.push(custom_line); // line 18

    // Register separate mouse areas for the radio/field and the Browse button
    // Radio button and field area
    mouse_areas.add(
        Rect::new(
            padded_area.x,
            padded_area.y + 18,
            browse_button_start as u16,
            1,
        ),
        ButtonId::AdditionalOption(8),
    );

    // Browse button area
    mouse_areas.add(
        Rect::new(
            padded_area.x + browse_button_start as u16,
            padded_area.y + 18,
            browse_button.len() as u16,
            1,
        ),
        ButtonId::BrowseButton,
    );

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, padded_area);

    // Show help popup if info icon was clicked
    if wizard.show_help_for == Some(FlacSection::BitDepth) && wizard.current_step == 2 {
        draw_help_box(
            f,
                ctx,
            wizard_area,
            "ReplayGain Help",
            "ReplayGain analyzes audio to enable consistent playback volume.\n\
             It adds metadata tags without altering the actual audio data.\n\n\
             \n\
             ◉ Album mode\n\
               Maintains consistent volume across an entire album\n\
               Preserves relative dynamics between tracks\n\
               Best for album listening\n\
             \n\
             ◉ Track mode\n\
               Normalizes each track individually\n\
               All tracks play at similar volumes\n\
               Best for shuffle/playlist playback\n\
             \n\
             ◉ Both\n\
               Scans and tags for BOTH album and track gain\n\
               Players can choose which mode to use\n\
               Recommended for maximum flexibility\n\
             \n\
             ◉ Off\n\
               No ReplayGain scanning or tagging\n\
               Original dynamics preserved\n\n\
             💡 ReplayGain is non-destructive and widely supported.\n\
                Most modern players respect ReplayGain tags.",
        );
    }

    // Show additional help popups
    if let Some(help_section) = wizard.show_additional_help_for {
        match help_section {
            AdditionalOptionsHelp::ReplayGain => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "ReplayGain Help",
                    "ReplayGain analyzes audio to enable consistent playback volume.\n\
                     It adds metadata tags without altering the actual audio data.\n\n\
                     \n\
                     ◉ Album mode\n\
                       Maintains consistent volume across an entire album\n\
                       Preserves relative dynamics between tracks\n\
                       Best for album listening\n\
                     \n\
                     ◉ Track mode\n\
                       Normalizes each track individually\n\
                       All tracks play at similar volumes\n\
                       Best for shuffle/playlist playback\n\
                     \n\
                     ◉ Both\n\
                       Scans and tags for BOTH album and track gain\n\
                       Players can choose which mode to use\n\
                       Recommended for maximum flexibility\n\
                     \n\
                     ◉ Off\n\
                       No ReplayGain scanning or tagging\n\
                       Original dynamics preserved\n\n\
                     💡 ReplayGain is non-destructive and widely supported.\n\
                        Most modern players respect ReplayGain tags.",
                );
            }
            AdditionalOptionsHelp::CopyFiles | AdditionalOptionsHelp::CopySubdirectories => {
                // Split into multiple pages for better readability
                let pages = vec![
                    // Page 1: Overview
                    "Copy additional files and folders from the source directory.\n\n\
                     This section controls what non-audio files and folders are\n\
                     copied along with your converted audio files.\n\n\
                     Two options are available:\n\n\
                     █ Copy files\n\
                       Copy specific file types by extension\n\
                       (txt, cue, log, pdf, jpg, etc.)\n\n\
                     █ Copy subdirectories\n\
                       Copy entire folders and their contents\n\
                       (artwork, scans, booklet, etc.)\n\n\
                     Both fields are fully editable. Click the info icon\n\
                     next to each option for detailed help.\n\n\
                     💡 Single-click checkboxes to enable/disable\n\
                        Double-click or press Enter on fields to edit",
                    // Page 2: Copy files details
                    "█ Copy files\n\n\
                     Enter file extensions you want to copy.\n\
                     The field is editable - type your own extensions\n\
                     separated by commas.\n\n\
                     Common extensions:\n\
                     • txt - Text files (lyrics, notes)\n\
                     • cue - Cue sheets for CD images\n\
                     • log - EAC/XLD rip logs\n\
                     • nfo - Information files\n\
                     • pdf - Digital booklets\n\
                     • jpg, png - Album artwork and scans\n\n\
                     Example: txt, cue, log, jpg, png\n\n\
                     You can add any extensions:\n\
                     doc, m3u, accurip, md5, sfv, etc.\n\n\
                     Leave the field blank to skip copying files.",
                    // Page 3: Copy subdirectories details
                    "█ Copy subdirectories\n\n\
                     Enter folder names or patterns you want to copy.\n\
                     The field is editable - type your own folder names\n\
                     separated by commas.\n\n\
                     Common patterns:\n\
                     • * - Copy ALL subdirectories\n\
                     • artwork - Album artwork folder\n\
                     • scans - CD/vinyl scans\n\
                     • booklet - Digital booklets\n\
                     • CD*, Disc* - Multi-disc patterns\n\n\
                     Example: artwork, scans, booklet, CD*\n\n\
                     You can enter any folder names:\n\
                     extras, bonus, logs, info, etc.\n\n\
                     Leave the field blank to skip copying folders.",
                ];

                // Ensure page index is valid
                let page_count = pages.len();
                let current_page = if wizard.help_page >= page_count {
                    page_count - 1
                } else {
                    wizard.help_page
                };

                draw_help_box_with_pages(f, ctx, wizard_area, "After Converting", &pages, current_page);
            }
            AdditionalOptionsHelp::MergeToSingle => {
                draw_help_box(
                    f,
                ctx,
                    wizard_area,
                    "Merge Tracks Help",
                    "Merge all tracks to single file with cue sheet.\n\n\
                     When enabled:\n\
                     • All input tracks are combined into one continuous file\n\
                     • A cue sheet is generated to preserve track boundaries\n\
                     • Track metadata is preserved in the cue sheet\n\n\
                     Useful for:\n\
                     • Live albums or DJ mixes\n\
                     • Archiving complete albums as single files\n\
                     • Creating gapless playback files\n\
                     • Reducing file count\n\
                     • Preserving exact track transitions\n\n\
                     💡 Tips:\n\
                        • Ensure tracks are in the correct order first\n\
                        • The cue sheet allows players to navigate tracks\n\
                        • Some players may not support cue sheets\n\
                        • Original track gaps are preserved",
                );
            }
            AdditionalOptionsHelp::SourceFiles => {
                let pages = vec![
                    // Page 1: Overview
                    "Choose where to save converted files.\n\n\
                     Two destination modes are available:\n\n\
                     ◉ Ask every time\n\
                       • Prompts for destination on each conversion\n\
                       • Most flexible option\n\
                       • Good for varied workflows\n\
                       • No default path needed\n\n\
                     ○ Custom\n\
                       • Set a fixed output directory\n\
                       • All conversions go to this location\n\
                       • Streamlines repeated conversions\n\
                       • Press Enter or double-click to edit path",
                    // Page 2: Tips and path options
                    "💡 Tips for Custom Paths:\n\n\
                     Custom paths can include:\n\
                     • Absolute paths:\n\
                       /home/user/music/converted\n\
                       C:\\Users\\Name\\Music\\Converted\n\n\
                     • Relative paths:\n\
                       ./output\n\
                       ../converted_audio\n\n\
                     • Environment variables:\n\
                       $HOME/music\n\
                       %USERPROFILE%\\Music\n\n\
                     • Use 'Ask every time' for one-off conversions\n\
                     • Use 'Custom' for batch processing\n\n\
                     ⚠️  The custom path must exist or be creatable.\n\
                         The Browse button helps you select folders.",
                ];

                // Ensure page index is valid
                let page_count = pages.len();
                let current_page = if wizard.help_page >= page_count {
                    page_count - 1
                } else {
                    wizard.help_page
                };

                draw_help_box_with_pages(f, ctx, wizard_area, "Destination Help", &pages, current_page);
            }
        }
    }
}

fn draw_confirmation(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    wizard: &SimpleWizard,
    _mouse_areas: &mut MouseAreas,
) {
    // Add left padding
    let padded_area = Rect::new(
        area.x + 6,
        area.y,
        area.width.saturating_sub(6),
        area.height,
    );

    let mut lines = vec![];

    lines.push(Line::from(Span::styled(
        "🎯 Conversion Settings Summary",
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(ctx.theme.accent),
    )));
    lines.push(Line::from(Span::raw("━".repeat(40))));
    lines.push(Line::from(""));

    // Format
    if let Some(format) = wizard.selected_format {
        lines.push(Line::from(vec![
            Span::styled(
                "Output Format: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format.to_string(), Style::default().fg(ctx.theme.accent)),
        ]));
    }

    // Quality/Advanced settings
    match wizard.selected_format {
        Some(AudioFormat::Flac) => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "FLAC Settings:",
                Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            )));

            // Bit depth
            let bit_depth_text = match wizard.bit_depth {
                Some(0) => "Same as source",
                Some(16) => "16-bit",
                Some(24) => "24-bit",
                Some(32) => "32-bit",
                _ => "Unknown",
            };
            lines.push(Line::from(format!("  Bit Depth: {}", bit_depth_text)));

            // Dithering
            if wizard.should_show_dithering() {
                if let Some(dither) = wizard.dither_type {
                    lines.push(Line::from(format!("  Dithering: {}", dither)));
                }
            }

            // Sample rate
            let sample_rate_text = match wizard.sample_rate {
                Some(0) => "Same as source",
                Some(rate) => &format!("{} Hz", rate),
                None => "Unknown",
            };
            lines.push(Line::from(format!("  Sample Rate: {}", sample_rate_text)));

            // Resampling quality if applicable
            if wizard.should_show_resampling() {
                if let Some(quality) = wizard.resample_quality {
                    let quality_text = match quality {
                        0 => "Ultra",
                        1 => "VHQ",
                        2 => "HQ",
                        3 => "MQ",
                        _ => "Unknown",
                    };
                    lines.push(Line::from(format!(
                        "  Resampling Quality: {}",
                        quality_text
                    )));
                }

                if let Some(nyquist) = wizard.nyquist_transition {
                    lines.push(Line::from(format!("  Nyquist Transition: {}", nyquist)));
                }
            }

            // Compression
            if let Some(level) = wizard.compression_level {
                lines.push(Line::from(format!("  Compression Level: {}", level)));
            }

            // Processing options
            lines.push(Line::from(""));
            lines.push(Line::from("  Processing Options:"));
            if wizard.verify_encoding.unwrap_or(false) {
                lines.push(Line::from("    ✓ Verify encoding"));
            }
            if wizard.calculate_replaygain.unwrap_or(false) {
                lines.push(Line::from("    ✓ Calculate ReplayGain"));
            }
            if wizard.store_md5.unwrap_or(true) {
                lines.push(Line::from("    ✓ Store MD5 checksum"));
            }
        }
        Some(AudioFormat::WavPack) => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "WavPack Settings:",
                Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            )));

            if let Some(quality) = &wizard.selected_quality {
                lines.push(Line::from(format!("  Compression: {}", quality)));
            }

            if wizard.store_md5.unwrap_or(true) {
                lines.push(Line::from("  ✓ Store MD5 checksum"));
            }
        }
        Some(AudioFormat::Mp3) => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "MP3 Settings:",
                Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            )));

            if let Some(quality) = &wizard.selected_quality {
                lines.push(Line::from(format!("  Bitrate: {}", quality)));
            }
        }
        Some(AudioFormat::Aac) => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "AAC Settings:",
                Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            )));

            if let Some(profile) = wizard.aac_profile {
                lines.push(Line::from(format!("  Profile: {}", profile)));
            }

            if let Some(quality) = &wizard.selected_quality {
                lines.push(Line::from(format!("  Bitrate: {}", quality)));
            }
        }
        Some(AudioFormat::Opus) => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Opus Settings:",
                Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            )));

            if let Some(quality) = &wizard.selected_quality {
                lines.push(Line::from(format!("  Quality: {}", quality)));
            }

            if let Some(content_type) = wizard.opus_content_type {
                lines.push(Line::from(format!("  Optimized for: {}", content_type)));
            }

            // Sample rate - only show if not "Same as source"
            if wizard.sample_rate.is_some() && wizard.sample_rate != Some(0) {
                lines.push(Line::from(format!(
                    "  Sample Rate: 48 kHz (bypassing Opus built-in resampler)"
                )));

                // Show resampling quality
                if let Some(quality) = wizard.resample_quality {
                    let quality_text = match quality {
                        0 => "Ultra",
                        1 => "VHQ",
                        2 => "HQ",
                        3 => "MQ",
                        _ => "Unknown",
                    };
                    lines.push(Line::from(format!(
                        "  Resampling Quality: {}",
                        quality_text
                    )));
                }

                // Show Nyquist Transition
                if let Some(nyquist) = wizard.nyquist_transition {
                    lines.push(Line::from(format!("  Nyquist Transition: {}", nyquist)));
                }
            }
        }
        Some(_) => {
            // WAV, AIFF
            lines.push(Line::from(""));
            lines.push(Line::from("No format-specific settings"));
        }
        None => {}
    }

    // Additional options
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Additional Options:",
        Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
    )));

    if let Some(mode) = wizard.replaygain_mode {
        lines.push(Line::from(format!("  ReplayGain: {}", mode)));
    }

    if wizard.copy_files_enabled && !wizard.copy_files_extensions.is_empty() {
        lines.push(Line::from(format!(
            "  ✓ Copy files: [{}]",
            wizard.copy_files_extensions
        )));
    }
    if wizard.copy_subdirectories_enabled && !wizard.copy_subdirectories.is_empty() {
        lines.push(Line::from(format!(
            "  ✓ Copy subdirectories: [{}]",
            wizard.copy_subdirectories
        )));
    }
    if wizard.merge_to_single.unwrap_or(false) {
        lines.push(Line::from("  ✓ Merge to single file"));
    }

    // Add destination info
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Destination:",
        Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
    )));
    match &wizard.destination_mode {
        DestinationMode::AskEveryTime => {
            lines.push(Line::from("  Ask every time"));
        }
        DestinationMode::Custom(path) => {
            if path.is_empty() {
                lines.push(Line::from(vec![
                    Span::raw("  Custom: "),
                    Span::styled(
                        "./output",
                        Style::default()
                            .fg(ctx.theme.text_dim)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Span::raw(" (default)"),
                ]));
            } else {
                lines.push(Line::from(format!("  Custom: {}", path)));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::raw("━".repeat(40))));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Ready to convert? Click the Start button below or press Enter.",
        Style::default()
            .fg(ctx.theme.success)
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        "Use Back button or press Esc to modify settings.",
        Style::default().fg(ctx.theme.text_dim),
    )));

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, padded_area);
}

fn draw_navigation(f: &mut Frame, ctx: &WizardRenderCtx, area: Rect, wizard: &SimpleWizard, mouse_areas: &mut MouseAreas) {
    // Check if any help is being displayed
    let help_is_shown = wizard.show_help_for.is_some() || wizard.show_additional_help_for.is_some();

    // Right-align buttons (Windows-style)
    let button_width = 16u16;
    let spacing = 2u16;
    // Always calculate space for 4 buttons to keep consistent positioning
    let total_width = button_width * 4 + spacing * 3;
    let x_offset = area.width.saturating_sub(total_width);

    // Load Preset button (only on first page) - position 0
    if wizard.current_step == 0 {
        let load_preset_area = Rect::new(area.x + x_offset, area.y + 1, button_width, 1);

        let load_preset_style = if wizard.focused_nav_button == Some(ButtonId::LoadPreset) {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.focus_bg)
                .add_modifier(Modifier::BOLD) // focused primary action
        } else if wizard.hovered_button == Some(ButtonId::LoadPreset) {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.hover_bg)
                .add_modifier(Modifier::BOLD) // hovered primary action
        } else {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.selected_bg) // selected primary action
        };

        let load_preset = Paragraph::new(" Load Preset ")
            .style(load_preset_style)
            .alignment(Alignment::Center);
        f.render_widget(load_preset, load_preset_area);

        // Only register mouse area if help is not shown
        if !help_is_shown {
            mouse_areas.add(load_preset_area, ButtonId::LoadPreset);
        }
    }

    // Legacy wizard preset persistence is intentionally disabled.
    // The main Tonepoet preset manager owns save/delete/duplicate consistency.

    // Back button - position 1 (always reserve space, even if not shown)
    if wizard.current_step > 0 {
        let back_area = Rect::new(
            area.x + x_offset + (button_width + spacing),
            area.y + 1,
            button_width,
            1,
        );

        let back_style = if wizard.focused_nav_button == Some(ButtonId::Back) {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.warning)
                .add_modifier(Modifier::BOLD) // focused action emphasis
        } else if wizard.hovered_button == Some(ButtonId::Back) {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.surface)
                .add_modifier(Modifier::BOLD) // hovered action emphasis
        } else {
            Style::default().fg(ctx.theme.text).bg(ctx.theme.surface)
        };

        let back = Paragraph::new("  ◀ Back  ")
            .style(back_style)
            .alignment(Alignment::Center);
        f.render_widget(back, back_area);

        // Only register mouse area if help is not shown
        if !help_is_shown {
            mouse_areas.add(back_area, ButtonId::Back);
        }
    }

    // Next/Start button - position 2 (always in same position)
    let next_area = Rect::new(
        area.x + x_offset + (button_width + spacing) * 2,
        area.y + 1,
        button_width,
        1,
    );
    let (next_text, base_style) = if wizard.current_step < 3 {
        (
            "  Next ▶  ",
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            "  Start ▶  ",
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.success)
                .add_modifier(Modifier::BOLD),
        )
    };

    let next_style = if wizard.focused_nav_button == Some(ButtonId::Next) {
        // Focused state uses the focus/background emphasis roles.
        if wizard.current_step < 3 {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.hover_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.success)
                .add_modifier(Modifier::BOLD)
        }
    } else if wizard.hovered_button == Some(ButtonId::Next) {
        // Hovered state uses the hover/background emphasis roles.
        if wizard.current_step < 3 {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.success)
                .add_modifier(Modifier::BOLD)
        }
    } else {
        base_style
    };

    let next = Paragraph::new(next_text)
        .style(next_style)
        .alignment(Alignment::Center);
    f.render_widget(next, next_area);

    // Only register mouse area if help is not shown
    if !help_is_shown {
        mouse_areas.add(next_area, ButtonId::Next);
    }

    // Cancel button - position 3 (always in same position)
    let cancel_area = Rect::new(
        area.x + x_offset + (button_width + spacing) * 3,
        area.y + 1,
        button_width,
        1,
    );

    let cancel_style = if wizard.focused_nav_button == Some(ButtonId::Cancel) {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.error)
            .add_modifier(Modifier::BOLD) // focused destructive action
    } else if wizard.hovered_button == Some(ButtonId::Cancel) {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.error_dim)
            .add_modifier(Modifier::BOLD) // hovered destructive action
    } else {
        Style::default().fg(ctx.theme.text).bg(ctx.theme.surface)
    };

    let cancel = Paragraph::new("  Cancel  ")
        .style(cancel_style)
        .alignment(Alignment::Center);
    f.render_widget(cancel, cancel_area);

    // Only register mouse area if help is not shown
    if !help_is_shown {
        mouse_areas.add(cancel_area, ButtonId::Cancel);
    }
}

fn draw_help_box(f: &mut Frame, ctx: &WizardRenderCtx, wizard_area: Rect, title: &str, content: &str) {
    // Help box covers the entire wizard area
    let help_area = wizard_area;

    // Clear the area first
    f.render_widget(Clear, help_area);

    // Create a help box using the configured border/title roles.
    let help_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border))
        .title(Span::styled(
            format!(" {} ", title),
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(ctx.theme.background)); // Same as wizard background

    let help_inner = help_block.inner(help_area);

    // Render the block
    f.render_widget(help_block, help_area);

    // Add content with padding
    let padded_inner = Rect::new(
        help_inner.x + 6,
        help_inner.y + 2,
        help_inner.width.saturating_sub(12),
        help_inner.height.saturating_sub(4),
    );

    // Build content with close instruction
    let full_content = format!("{}\n\n[Press Esc or click anywhere to close]", content);

    // Render the content with primary text and slight emphasis
    let help_paragraph = Paragraph::new(full_content)
        .style(
            Style::default()
                .fg(ctx.theme.text)
                .add_modifier(Modifier::BOLD),
        )
        .wrap(Wrap { trim: true })
        .alignment(Alignment::Left);

    f.render_widget(help_paragraph, padded_inner);

    // Add a subtle hint at the bottom
    let hint = Paragraph::new("Click anywhere or press Esc to close")
        .style(Style::default().fg(ctx.theme.accent))
        .alignment(Alignment::Center);

    let hint_area = Rect::new(
        help_area.x,
        help_area.y + help_area.height - 2,
        help_area.width,
        1,
    );
    f.render_widget(hint, hint_area);
}

fn draw_help_box_with_pages(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    wizard_area: Rect,
    title: &str,
    pages: &[&str],
    current_page: usize,
) {
    // Help box covers the entire wizard area
    let help_area = wizard_area;

    // Clear the area first
    f.render_widget(Clear, help_area);

    // Create a help box using the configured border/title roles.
    let help_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border))
        .title(Span::styled(
            format!(" {} ", title),
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(ctx.theme.background)); // Same as wizard background

    let help_inner = help_block.inner(help_area);

    // Render the block
    f.render_widget(help_block, help_area);

    // Add content with padding
    let padded_inner = Rect::new(
        help_inner.x + 6,
        help_inner.y + 2,
        help_inner.width.saturating_sub(12),
        help_inner.height.saturating_sub(4),
    );

    // Get current page content
    let content = pages.get(current_page).unwrap_or(&pages[0]);

    // Build content with navigation instruction if multiple pages
    let full_content = if pages.len() > 1 {
        format!("{}\n\n[Page {} of {} - Use ← → arrows to navigate]\n[Press Esc or click anywhere to close]",
                content, current_page + 1, pages.len())
    } else {
        format!("{}\n\n[Press Esc or click anywhere to close]", content)
    };

    // Render the content with primary text and slight emphasis
    let help_paragraph = Paragraph::new(full_content)
        .style(
            Style::default()
                .fg(ctx.theme.text)
                .add_modifier(Modifier::BOLD),
        )
        .wrap(Wrap { trim: true })
        .alignment(Alignment::Left);

    f.render_widget(help_paragraph, padded_inner);

    // Add navigation hint at the bottom for multi-page help
    let hint = if pages.len() > 1 {
        format!(
            "← Previous | Page {} of {} | Next → | Esc to close",
            current_page + 1,
            pages.len()
        )
    } else {
        "Click anywhere or press Esc to close".to_string()
    };

    let hint_paragraph = Paragraph::new(hint)
        .style(Style::default().fg(ctx.theme.accent))
        .alignment(Alignment::Center);

    let hint_area = Rect::new(
        help_area.x,
        help_area.y + help_area.height - 2,
        help_area.width,
        1,
    );
    f.render_widget(hint_paragraph, hint_area);
}

fn draw_popup(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    wizard_area: Rect,
    popup_state: &PopupState,
    mouse_areas: &mut MouseAreas,
    hovered_button: Option<ButtonId>,
) {
    // Calculate popup dimensions based on type
    let (width, height, title) = match &popup_state.popup_type {
        PopupType::PresetName => (60, 8, " Presets "),
        PopupType::TextInput { field } => {
            let title = match field {
                EditingField::CopyFiles => " File Extensions ",
                EditingField::CopySubdirectories => " Subdirectories ",
                EditingField::CustomDestination => " Custom Destination ",
            };
            (80, 9, title) // Keep height constant
        }
        PopupType::PresetList { presets, .. } => {
            let height = (presets.len() as u16 + 4).min(20); // +4 for title, spacing, and buttons
            (60, height, " Load Preset ")
        }
        PopupType::FileBrowser(_) => {
            // File browser uses its own custom rendering, just return dummy values
            (80, 20, " Select Directory ")
        }
        PopupType::NewFolder { .. } => (60, 8, " New Folder "),
    };

    // Center the popup
    let popup_area = centered_rect(width, height, wizard_area);

    // Clear the area behind the popup
    f.render_widget(Clear, popup_area);

    // Create the popup block with rounded borders
    let popup_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border))
        .title(Span::styled(
            title,
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(ctx.theme.overlay));

    let inner_area = popup_block.inner(popup_area);
    f.render_widget(popup_block, popup_area);

    // Draw popup content based on type
    match &popup_state.popup_type {
        PopupType::PresetName => {
            draw_preset_name_popup(f, ctx, inner_area, popup_state, mouse_areas, hovered_button);
        }
        PopupType::TextInput { .. } => {
            draw_text_input_popup(
                f,
                ctx,
                inner_area,
                popup_state,
                mouse_areas,
                hovered_button,
                "",
                "",
            );
        }
        PopupType::PresetList {
            presets,
            selected_index,
        } => {
            draw_preset_list_popup(
                f,
                ctx,
                inner_area,
                presets,
                *selected_index,
                mouse_areas,
                popup_state,
                hovered_button,
            );
        }
        PopupType::FileBrowser(browser) => {
            draw_file_browser(f, ctx, f.size(), browser, mouse_areas, hovered_button);
        }
        PopupType::NewFolder { .. } => {
            draw_new_folder_popup(f, ctx, inner_area, popup_state, mouse_areas, hovered_button);
        }
    }
}

fn draw_preset_name_popup(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    popup_state: &PopupState,
    mouse_areas: &mut MouseAreas,
    hovered_button: Option<ButtonId>,
) {
    // Split area for label, input field, and buttons
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Label
            Constraint::Length(3), // Input field
            Constraint::Min(0),    // Spacing
        ])
        .split(area);

    // Register popup background area (excluding button area)
    let bg_area = Rect::new(area.x, area.y, area.width, area.height - 2);
    mouse_areas.add(bg_area, ButtonId::PopupBackground);

    // Draw label
    let label = Paragraph::new("Enter preset name:")
        .style(Style::default().fg(ctx.theme.text))
        .alignment(Alignment::Left);
    f.render_widget(label, chunks[0]);

    // Draw input field
    let input_area = Rect::new(chunks[1].x + 1, chunks[1].y, chunks[1].width - 2, 3);
    let input_is_focused = matches!(popup_state.focused_element, PopupFocus::Input);
    draw_input_field(
        f,
                ctx,
        input_area,
        &popup_state.input_text,
        popup_state.cursor_pos,
        popup_state.view_offset,
        input_is_focused,
    );

    // Draw buttons (OK and Cancel)
    let button_area = Rect::new(area.x, area.y + area.height - 2, area.width, 1);
    draw_popup_buttons(f, ctx, button_area, mouse_areas, popup_state, hovered_button);
}

fn draw_text_input_popup(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    popup_state: &PopupState,
    mouse_areas: &mut MouseAreas,
    hovered_button: Option<ButtonId>,
    _title: &str,
    prompt: &str,
) {
    // Similar to preset name but with different label
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Label
            Constraint::Length(3), // Input field
            Constraint::Min(0),    // Spacing
            Constraint::Length(1), // Buttons
            Constraint::Length(1), // Error message or empty space
        ])
        .split(area);

    // Register popup background area (excluding button and error area)
    let bg_area = Rect::new(area.x, area.y, area.width, area.height - 2);
    mouse_areas.add(bg_area, ButtonId::PopupBackground);

    // Draw label based on field type or use provided prompt
    let label_text = if !prompt.is_empty() {
        prompt
    } else {
        match &popup_state.popup_type {
            PopupType::TextInput { field } => match field {
                EditingField::CopyFiles => "Enter file extensions (comma-separated):",
                EditingField::CopySubdirectories => {
                    "Enter subdirectory patterns (comma-separated):"
                }
                EditingField::CustomDestination => "Enter destination path:",
            },
            _ => "",
        }
    };

    let label = Paragraph::new(label_text)
        .style(Style::default().fg(ctx.theme.text))
        .alignment(Alignment::Left);
    f.render_widget(label, chunks[0]);

    // Draw input field
    let input_area = Rect::new(chunks[1].x + 1, chunks[1].y, chunks[1].width - 2, 3);
    let input_is_focused = matches!(popup_state.focused_element, PopupFocus::Input);
    draw_input_field(
        f,
                ctx,
        input_area,
        &popup_state.input_text,
        popup_state.cursor_pos,
        popup_state.view_offset,
        input_is_focused,
    );

    // Draw buttons
    draw_popup_buttons(f, ctx, chunks[3], mouse_areas, popup_state, hovered_button);

    // Draw error message if present (in the last line before border)
    if let Some(error_msg) = &popup_state.error_message {
        let error = Paragraph::new(error_msg.as_str())
            .style(Style::default().fg(ctx.theme.error).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center);
        f.render_widget(error, chunks[4]);
    }
}

fn draw_popup_buttons(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    mouse_areas: &mut MouseAreas,
    popup_state: &PopupState,
    hovered_button: Option<ButtonId>,
) {
    let button_width = 10u16;
    let spacing = 2u16;
    let total_width = button_width * 2 + spacing;
    let x_offset = (area.width.saturating_sub(total_width)) / 2;

    // OK button
    let ok_area = Rect::new(area.x + x_offset, area.y, button_width, 1);
    let ok_is_focused = matches!(popup_state.focused_element, PopupFocus::OkButton);
    let ok_is_hovered = matches!(hovered_button, Some(ButtonId::PopupOk));

    let ok_style = if ok_is_focused {
        // Focused state uses success fill with selected foreground.
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.success)
            .add_modifier(Modifier::BOLD)
    } else if ok_is_hovered {
        // Hovered state uses success fill with selected foreground.
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.success)
            .add_modifier(Modifier::BOLD)
    } else {
        // Normal state uses success fill with selected foreground.
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.success)
            .add_modifier(Modifier::BOLD)
    };

    let ok_button = Paragraph::new("   OK   ")
        .style(ok_style)
        .alignment(Alignment::Center);
    f.render_widget(ok_button, ok_area);
    mouse_areas.add(ok_area, ButtonId::PopupOk);

    // Cancel button
    let cancel_area = Rect::new(
        area.x + x_offset + button_width + spacing,
        area.y,
        button_width,
        1,
    );
    let cancel_is_focused = matches!(popup_state.focused_element, PopupFocus::CancelButton);
    let cancel_is_hovered = matches!(hovered_button, Some(ButtonId::PopupCancel));

    let cancel_style = if cancel_is_focused {
        // Focused cancel state uses disabled-surface styling.
        Style::default()
            .fg(ctx.theme.text)
            .bg(ctx.theme.disabled_bg)
            .add_modifier(Modifier::BOLD)
    } else if cancel_is_hovered {
        // Hovered cancel state uses disabled-surface styling.
        Style::default()
            .fg(ctx.theme.text)
            .bg(ctx.theme.disabled_bg)
    } else {
        // Normal cancel state uses disabled-surface styling.
        Style::default().fg(ctx.theme.text).bg(ctx.theme.disabled_bg)
    };

    let cancel_button = Paragraph::new(" Cancel ")
        .style(cancel_style)
        .alignment(Alignment::Center);
    f.render_widget(cancel_button, cancel_area);
    mouse_areas.add(cancel_area, ButtonId::PopupCancel);
}

fn draw_input_field(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    text: &str,
    cursor_pos: usize,
    view_offset: usize,
    is_active: bool,
) {
    // First, draw a border around the input area
    let border_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if is_active {
            ctx.theme.accent
        } else {
            ctx.theme.border
        }));

    let inner_area = border_block.inner(area);
    f.render_widget(border_block, area);

    // Calculate visible text based on view offset
    let visible_text = if text.len() > view_offset {
        &text[view_offset..]
    } else {
        ""
    };

    // Truncate if needed
    let max_chars = inner_area.width as usize;
    let display_text = if visible_text.len() > max_chars {
        &visible_text[..max_chars]
    } else {
        visible_text
    };

    // Fill the input interior with the configured input background role.
    let fill_block = Block::default().style(Style::default().bg(ctx.theme.input_bg));
    f.render_widget(fill_block, inner_area);

    // Draw the text content
    let text_style = if is_active {
        Style::default().fg(ctx.theme.text).bg(ctx.theme.input_bg)
    } else {
        Style::default().fg(ctx.theme.text_muted).bg(ctx.theme.input_bg)
    };

    let field = Paragraph::new(display_text).style(text_style);

    f.render_widget(field, inner_area);

    // Draw cursor if active
    if is_active && cursor_pos >= view_offset && cursor_pos - view_offset < max_chars {
        let cursor_x = inner_area.x + (cursor_pos - view_offset) as u16;
        let cursor_y = inner_area.y;
        f.set_cursor(cursor_x, cursor_y);
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn draw_preset_list_popup(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    presets: &[String],
    selected_index: usize,
    mouse_areas: &mut MouseAreas,
    popup_state: &PopupState,
    hovered_button: Option<ButtonId>,
) {
    // Split area for list and buttons
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),    // Preset list
            Constraint::Length(2), // Buttons
        ])
        .split(area);

    // Register popup background area (excluding button area)
    let bg_area = Rect::new(area.x, area.y, area.width, area.height - 2);
    mouse_areas.add(bg_area, ButtonId::PopupBackground);

    // Draw preset list
    let mut lines = vec![];
    for (i, preset) in presets.iter().enumerate() {
        let style = if i == selected_index {
            Style::default()
                .fg(ctx.theme.selected_fg)
                .bg(ctx.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ctx.theme.text)
        };
        lines.push(Line::from(Span::styled(format!(" {} ", preset), style)));

        // Register mouse area for each preset
        if i < chunks[0].height as usize {
            let preset_area = Rect::new(chunks[0].x, chunks[0].y + i as u16, chunks[0].width, 1);
            mouse_areas.add(preset_area, ButtonId::PresetItem(i));
        }
    }

    let list = Paragraph::new(lines)
        .style(Style::default().fg(ctx.theme.text))
        .wrap(Wrap { trim: false });
    f.render_widget(list, chunks[0]);

    // Draw buttons (OK and Cancel)
    let button_area = chunks[1];
    draw_popup_buttons(f, ctx, button_area, mouse_areas, popup_state, hovered_button);
}

fn format_option_line(ctx: &WizardRenderCtx, text: &str, is_selected: bool, is_focused: bool) -> Line<'static> {
    let radio = if is_selected { "◉" } else { "○" };
    let radio_style = if is_selected {
        Style::default().fg(ctx.theme.accent)
    } else {
        Style::default().fg(ctx.theme.text)
    };

    let text_style = if is_focused {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ctx.theme.text)
    };

    if is_focused {
        Line::from(vec![
            Span::styled(" ", text_style),
            Span::styled(
                radio,
                Style::default()
                    .fg(ctx.theme.text)
                    .bg(ctx.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {}", text), text_style),
        ])
    } else {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(radio, radio_style),
            Span::raw(" "),
            Span::styled(text.to_string(), text_style),
        ])
    }
}

// File browser UI functions
fn draw_file_browser(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    browser: &crate::types::FileBrowser,
    mouse_areas: &mut MouseAreas,
    hovered_button: Option<ButtonId>,
) {
    // Calculate popup dimensions - 70% width, 80% height
    let popup_width = (area.width as f32 * 0.7).max(60.0) as u16;
    let popup_height = (area.height as f32 * 0.8).max(20.0) as u16;
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    // Clear the popup area
    f.render_widget(Clear, popup_area);

    // Popup overlay background.
    let bg_color = ctx.theme.overlay;

    // Create popup block
    let popup_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Select Directory ")
        .title_style(
            Style::default()
                .fg(ctx.theme.title)
                .add_modifier(Modifier::BOLD),
        )
        .title_alignment(Alignment::Left)
        .border_style(Style::default().fg(ctx.theme.border))
        .style(Style::default().bg(bg_color));

    f.render_widget(popup_block.clone(), popup_area);

    let inner = popup_block.inner(popup_area);

    // Layout inside the popup
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Current path
            Constraint::Min(0),    // File list
            Constraint::Length(1), // Buttons (single line)
        ])
        .split(inner);

    // Current path display
    draw_current_path(f, ctx, chunks[0], browser);

    // File list
    draw_directory_list(f, ctx, chunks[1], browser, mouse_areas);

    // Buttons
    draw_browser_buttons(f, ctx, chunks[2], browser, mouse_areas, hovered_button);
}

fn draw_current_path(f: &mut Frame, ctx: &WizardRenderCtx, area: Rect, browser: &crate::types::FileBrowser) {
    let path_str = browser.current_path.display().to_string();
    let display = if path_str.len() > (area.width as usize - 4) {
        format!(
            "📁 ...{}",
            &path_str[path_str.len() - (area.width as usize - 8)..]
        )
    } else {
        format!("📁 {}", path_str)
    };

    let paragraph = Paragraph::new(display)
        .style(Style::default().fg(ctx.theme.text))
        .alignment(Alignment::Left);

    f.render_widget(paragraph, area);
}

fn draw_directory_list(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    browser: &crate::types::FileBrowser,
    mouse_areas: &mut MouseAreas,
) {
    // Calculate visible range
    // Subtract 2 for the top and bottom borders
    let visible_height = area.height.saturating_sub(2) as usize;
    let selected = browser.selected_index;
    let total_entries = browser.entries.len();

    // Calculate scroll offset to keep selected item visible
    let scroll_offset = if total_entries == 0 {
        0
    } else {
        // If selected item is above the current view, scroll up to show it
        if selected < visible_height / 2 {
            0
        } else if selected >= total_entries.saturating_sub(visible_height / 2) {
            // If near the end, show the last items
            total_entries.saturating_sub(visible_height)
        } else {
            // Center the selected item in the view
            selected.saturating_sub(visible_height / 2)
        }
    };

    // Track visible items for mouse interaction
    // Note: We need to account for the border of the list widget (1 pixel on each side)
    for (i, idx) in (scroll_offset..total_entries.min(scroll_offset + visible_height)).enumerate() {
        let item_area = Rect::new(
            area.x + 1,                   // +1 for left border
            area.y + 1 + i as u16,        // +1 for top border
            area.width.saturating_sub(2), // -2 for left and right borders
            1,
        );
        mouse_areas.add(item_area, ButtonId::FileItem(idx));
    }

    // Create list items only for visible entries
    let items: Vec<ListItem> = browser
        .entries
        .iter()
        .skip(scroll_offset)
        .take(visible_height)
        .enumerate()
        .map(|(i, entry)| {
            let icon = if entry.name == ".." { "⬆️" } else { "📁" };

            // Format entry display
            let mut spans = vec![];

            spans.push(Span::raw(format!("{} ", icon)));

            // File name
            spans.push(Span::raw(&entry.name));

            // Check if this is the selected index
            let is_selected = scroll_offset + i == browser.selected_index;

            let style = if is_selected && browser.focus == crate::types::BrowserFocus::List {
                // Currently focused only
                Style::default()
                    .bg(ctx.theme.surface)
                    .fg(ctx.theme.text)
                    .add_modifier(Modifier::BOLD)
            } else if entry.is_dir {
                Style::default().fg(ctx.theme.accent)
            } else {
                Style::default().fg(ctx.theme.text)
            };

            ListItem::new(Line::from(spans)).style(style)
        })
        .collect();

    let list_widget = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ctx.theme.border))
            .style(Style::default()),
    );

    f.render_widget(list_widget, area);

    // Add scroll indicator if needed
    if browser.entries.len() > visible_height {
        let scrollbar_x = area.x + area.width - 1;
        let scrollbar_height = area.height - 2; // Account for borders

        // Calculate thumb position and size
        let thumb_height = (visible_height as f32 / browser.entries.len() as f32
            * scrollbar_height as f32)
            .max(1.0) as u16;
        let thumb_pos =
            (scroll_offset as f32 / browser.entries.len() as f32 * scrollbar_height as f32) as u16;

        // Draw scrollbar track
        for y in 0..scrollbar_height {
            let style = if y >= thumb_pos && y < thumb_pos + thumb_height {
                Style::default().fg(ctx.theme.accent)
            } else {
                Style::default().fg(ctx.theme.text_dim)
            };

            let scrollbar = Paragraph::new("│").style(style);
            f.render_widget(scrollbar, Rect::new(scrollbar_x, area.y + 1 + y, 1, 1));
        }
    }
}

fn draw_browser_buttons(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    browser: &crate::types::FileBrowser,
    mouse_areas: &mut MouseAreas,
    hovered_button: Option<ButtonId>,
) {
    // Calculate total button width: New(10) + space(1) + Select(12) + space(1) + Cancel(10) = 34
    let total_button_width = 34;
    let center_offset = (area.width.saturating_sub(total_button_width)) / 2;

    // Create a centered area for the buttons
    let button_area = Rect::new(
        area.x + center_offset,
        area.y,
        total_button_width.min(area.width),
        area.height,
    );

    let button_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(10), // New
            Constraint::Length(1),  // Spacer
            Constraint::Length(12), // Select
            Constraint::Length(1),  // Spacer
            Constraint::Length(10), // Cancel
        ])
        .split(button_area);

    // New button follows the primary action role set.
    let new_style = if hovered_button == Some(ButtonId::NewFolder) {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.hover_bg) // hovered primary action
    } else if browser.focus == crate::types::BrowserFocus::NewButton {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.focus_bg) // focused primary action
    } else {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.selected_bg) // selected primary action
    };

    let new_button = Paragraph::new(" New ")
        .style(new_style)
        .alignment(Alignment::Center);
    f.render_widget(new_button, button_layout[0]);
    mouse_areas.add(button_layout[0], ButtonId::NewFolder);

    // Select button uses the success action role.
    let select_style = if hovered_button == Some(ButtonId::FileBrowserSelect) {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.success)
            .add_modifier(Modifier::BOLD) // hovered success action
    } else if browser.focus == crate::types::BrowserFocus::SelectButton {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.success)
            .add_modifier(Modifier::BOLD) // focused success action
    } else {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.success)
            .add_modifier(Modifier::BOLD) // success action
    };

    let select_button = Paragraph::new(" Select ")
        .style(select_style)
        .alignment(Alignment::Center);
    f.render_widget(select_button, button_layout[2]);
    mouse_areas.add(button_layout[2], ButtonId::FileBrowserSelect);

    // Cancel button - Match wizard's Cancel button styling
    let cancel_style = if hovered_button == Some(ButtonId::FileBrowserCancel) {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.error_dim)
            .add_modifier(Modifier::BOLD) // hovered destructive action
    } else if browser.focus == crate::types::BrowserFocus::CancelButton {
        Style::default()
            .fg(ctx.theme.selected_fg)
            .bg(ctx.theme.error)
            .add_modifier(Modifier::BOLD) // focused destructive action
    } else {
        Style::default().fg(ctx.theme.text).bg(ctx.theme.surface) // neutral action surface
    };

    let cancel_button = Paragraph::new(" Cancel ")
        .style(cancel_style)
        .alignment(Alignment::Center);
    f.render_widget(cancel_button, button_layout[4]);
    mouse_areas.add(button_layout[4], ButtonId::FileBrowserCancel);
}

fn draw_new_folder_popup(
    f: &mut Frame,
    ctx: &WizardRenderCtx,
    area: Rect,
    popup_state: &crate::types::PopupState,
    mouse_areas: &mut MouseAreas,
    hovered_button: Option<ButtonId>,
) {
    // Use existing text input popup rendering
    draw_text_input_popup(
        f,
                ctx,
        area,
        popup_state,
        mouse_areas,
        hovered_button,
        "New Folder Name",
        "Enter the name for the new folder:",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    fn sentinel_theme() -> WizardTheme {
        WizardTheme {
            background: Color::Rgb(1, 2, 3),
            surface: Color::Rgb(4, 5, 6),
            overlay: Color::Rgb(7, 8, 9),
            border: Color::Rgb(10, 11, 12),
            title: Color::Rgb(13, 14, 15),
            text: Color::Rgb(16, 17, 18),
            text_muted: Color::Rgb(19, 20, 21),
            text_dim: Color::Rgb(22, 23, 24),
            accent: Color::Rgb(25, 26, 27),
            selected_bg: Color::Rgb(28, 29, 30),
            selected_fg: Color::Rgb(31, 32, 33),
            hover_bg: Color::Rgb(34, 35, 36),
            focus_bg: Color::Rgb(37, 38, 39),
            success: Color::Rgb(40, 41, 42),
            warning: Color::Rgb(43, 44, 45),
            error: Color::Rgb(46, 47, 48),
            error_dim: Color::Rgb(49, 50, 51),
            disabled_bg: Color::Rgb(52, 53, 54),
            disabled_fg: Color::Rgb(55, 56, 57),
            input_bg: Color::Rgb(58, 59, 60),
        }
    }

    fn render_colors(wizard: &SimpleWizard, theme: WizardTheme) -> Vec<(Color, Color)> {
        let backend = TestBackend::new(120, 50);
        let mut terminal = Terminal::new(backend).expect("terminal");

        terminal
            .draw(|frame| {
                draw_wizard_with_theme(frame, wizard, theme);
            })
            .expect("draw");

        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| (cell.fg, cell.bg))
            .collect()
    }

    fn assert_fg(cells: &[(Color, Color)], color: Color, role: &str) {
        assert!(
            cells.iter().any(|(fg, _)| *fg == color),
            "WizardTheme.{role} should affect at least one rendered foreground cell"
        );
    }

    fn assert_bg(cells: &[(Color, Color)], color: Color, role: &str) {
        assert!(
            cells.iter().any(|(_, bg)| *bg == color),
            "WizardTheme.{role} should affect at least one rendered background cell"
        );
    }

    #[test]
    fn wizard_theme_role_contract_renders_declared_roles() {
        let theme = sentinel_theme();

        let default_wizard = SimpleWizard::new();
        let default_cells = render_colors(&default_wizard, theme);
        assert_bg(&default_cells, theme.background, "background");
        assert_fg(&default_cells, theme.text, "text");
        assert_fg(&default_cells, theme.border, "border");
        assert_fg(&default_cells, theme.title, "title");
        assert_fg(&default_cells, theme.accent, "accent");
        assert_bg(&default_cells, theme.selected_bg, "selected_bg");
        assert_fg(&default_cells, theme.selected_fg, "selected_fg");

        let mut popup_wizard = SimpleWizard::new();
        popup_wizard.popup_state = Some(PopupState {
            popup_type: PopupType::PresetName,
            input_text: "Preset name".to_string(),
            cursor_pos: 0,
            view_offset: 0,
            error_message: None,
            focused_element: PopupFocus::Input,
        });
        let popup_cells = render_colors(&popup_wizard, theme);
        assert_bg(&popup_cells, theme.overlay, "overlay");
        assert_bg(&popup_cells, theme.input_bg, "input_bg");

        let mut disabled_wizard = SimpleWizard::new();
        // Step 0's right pane hosts the format options with the disabled
        // forced "Re-encode FLAC files" row.
        disabled_wizard.current_step = 0;
        disabled_wizard.selected_format = Some(AudioFormat::Flac);
        disabled_wizard.bit_depth = Some(16);
        let disabled_cells = render_colors(&disabled_wizard, theme);
        assert_fg(&disabled_cells, theme.disabled_fg, "disabled_fg");

        assert!(
            !popup_cells.iter().any(|(_, bg)| *bg == theme.selected_fg),
            "selected_fg is a foreground role and must not be used as a popup/input background"
        );
        assert!(
            !popup_cells.iter().any(|(_, bg)| *bg == theme.text_dim),
            "text_dim is a foreground role and must not be used as a popup/input background"
        );
    }

    #[test]
    fn draw_wizard_with_theme_uses_passed_theme() {
        let wizard = SimpleWizard::new();
        let theme = sentinel_theme();
        let backend = TestBackend::new(120, 50);
        let mut terminal = Terminal::new(backend).expect("terminal");

        terminal
            .draw(|frame| {
                draw_wizard_with_theme(frame, &wizard, theme);
            })
            .expect("draw");

        let buffer = terminal.backend().buffer();
        assert!(
            buffer.content().iter().any(|cell| cell.bg == theme.background),
            "wizard background should come from the passed WizardTheme"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.fg == theme.text),
            "wizard body text should come from the passed WizardTheme"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.fg == theme.border),
            "wizard border should come from the passed WizardTheme"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.fg == theme.title),
            "wizard title should come from the passed WizardTheme"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.fg == theme.accent),
            "wizard accent foreground should come from the passed WizardTheme"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.bg == theme.selected_bg),
            "wizard selected/highlight background should come from the passed WizardTheme"
        );
    }

    #[test]
    fn popup_and_input_backgrounds_use_background_roles() {
        let mut wizard = SimpleWizard::new();
        wizard.popup_state = Some(PopupState {
            popup_type: PopupType::PresetName,
            input_text: "Preset name".to_string(),
            cursor_pos: 0,
            view_offset: 0,
            error_message: None,
            focused_element: PopupFocus::Input,
        });
        let theme = sentinel_theme();
        let backend = TestBackend::new(120, 50);
        let mut terminal = Terminal::new(backend).expect("terminal");

        terminal
            .draw(|frame| {
                draw_wizard_with_theme(frame, &wizard, theme);
            })
            .expect("draw");

        let buffer = terminal.backend().buffer();
        assert!(
            buffer.content().iter().any(|cell| cell.bg == theme.overlay),
            "popup background should come from WizardTheme.overlay"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.bg == theme.input_bg),
            "input field background should come from WizardTheme.input_bg"
        );
        assert!(
            !buffer.content().iter().any(|cell| cell.bg == theme.selected_fg),
            "selected_fg is a foreground role and must not be used as a structural background"
        );
        assert!(
            !buffer.content().iter().any(|cell| cell.bg == theme.text_dim),
            "text_dim is a foreground role and must not be used as a structural background"
        );
    }

    #[test]
    fn disabled_options_use_disabled_foreground_role() {
        let mut wizard = SimpleWizard::new();
        // Step 0's right pane is the FLAC format-options screen; bit_depth
        // Some(16) forces re-encode, disabling "Re-encode FLAC files".
        wizard.current_step = 0;
        wizard.selected_format = Some(AudioFormat::Flac);
        wizard.bit_depth = Some(16);
        let theme = sentinel_theme();
        let backend = TestBackend::new(120, 50);
        let mut terminal = Terminal::new(backend).expect("terminal");

        terminal
            .draw(|frame| {
                draw_wizard_with_theme(frame, &wizard, theme);
            })
            .expect("draw");

        let buffer = terminal.backend().buffer();
        assert!(
            buffer.content().iter().any(|cell| cell.fg == theme.disabled_fg),
            "disabled wizard text should come from WizardTheme.disabled_fg"
        );
    }

    #[test]
    fn wizard_renderer_source_has_no_hardcoded_rgb_literals() {
        let source = include_str!("ui.rs");
        let production_source = source
            .split("#[cfg(test)]")
            .next()
            .expect("production source prefix");
        assert!(
            !production_source.contains("Color::Rgb"),
            "renderer code should use WizardTheme roles; RGB sentinels belong in tests or theme.rs"
        );
        for role in [
            "background",
            "surface",
            "overlay",
            "border",
            "title",
            "text",
            "text_muted",
            "text_dim",
            "accent",
            "selected_bg",
            "selected_fg",
            "hover_bg",
            "focus_bg",
            "success",
            "warning",
            "error",
            "error_dim",
            "disabled_bg",
            "disabled_fg",
            "input_bg",
        ] {
            let needle = format!("ctx.theme.{role}");
            assert!(
                production_source.contains(&needle),
                "WizardTheme role `{role}` must have visible production renderer usage or be removed from WizardTheme"
            );
        }
        for forbidden in ["bg(ctx.theme.selected_fg)", "bg(ctx.theme.text_dim)"] {
            assert!(
                !production_source.contains(forbidden),
                "foreground role used as structural background: {forbidden}"
            );
        }
        for stale_comment in [
            "dark gray",
            "custom dark gray",
            "cyan color",
            "white title",
            "teal",
            "mint-white",
            "Pale yellow",
            "Light red",
            "Darker red",
            "green base",
        ] {
            assert!(
                !production_source.contains(stale_comment),
                "stale hardcoded-color comment remains in wizard renderer: {stale_comment}"
            );
        }
    }

    #[test]
    fn wizard_production_code_has_no_ambient_theme_or_file_debug_io() {
        let sources = [
            ("ui.rs", include_str!("ui.rs").split("#[cfg(test)]").next().unwrap_or("")),
            ("theme.rs", include_str!("theme.rs")),
            ("events.rs", include_str!("events.rs")),
            ("types.rs", include_str!("types.rs")),
            ("main.rs", include_str!("main.rs")),
        ];
        let forbidden = [
            concat!("CURRENT", "_WIZARD", "_THEME"),
            concat!("bind", "_wizard", "_theme"),
            concat!("wizard", "_theme()"),
            concat!("Wizard", "Theme", "Guard"),
            concat!("thread", "_local!"),
            concat!("wizard", "_areas", ".log"),
            concat!("Open", "Options"),
        ];

        for (path, source) in sources {
            for token in forbidden {
                assert!(
                    !source.contains(token),
                    "forbidden Wizard ambient-theme/debug token `{token}` found in {path}"
                );
            }
        }
    }

}
