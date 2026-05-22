use super::types::{
    AacProfile, AdditionalOptionsHelp, AudioFormat, DestinationMode, DitherType, EditingField,
    FlacSection, FormatSpecificHelp, OpusContentType, PopupFocus, PopupState, PopupType,
    ReplayGainMode, SimpleWizard,
};
use super::ui::ButtonId;
use crate::presets::{ConversionPreset, PresetManager};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::fs::OpenOptions;
use std::io::Write;

impl SimpleWizard {
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Debug logging
        use std::fs::OpenOptions;
        use std::io::Write;
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open("wizard_keys.log")
        {
            let _ = writeln!(file, "\nKey event: {:?}, current_step: {}, selected_format: {:?}, in_quality_area: {}, quality_index: {}", 
                           key.code, self.current_step, self.selected_format, self.in_quality_area, self.quality_index);
        }

        // Handle help navigation if any help is showing
        if self.show_help_for.is_some()
            || self.show_additional_help_for.is_some()
            || self.show_format_help_for.is_some()
        {
            match key.code {
                KeyCode::Left => {
                    if self.help_page > 0 {
                        self.help_page -= 1;
                    }
                    return true;
                }
                KeyCode::Right => {
                    // We'll handle max page checking in the UI based on which help is shown
                    self.help_page += 1;
                    return true;
                }
                KeyCode::Esc => {
                    self.show_help_for = None;
                    self.show_additional_help_for = None;
                    self.show_format_help_for = None;
                    self.help_page = 0;
                    return true;
                }
                _ => {
                    // Allow other keys to fall through to normal handlers
                }
            }
        }

        // Handle popup input if active
        if self.popup_state.is_some() {
            return self.handle_popup_key(key);
        }

        let handled = match self.current_step {
            0 => self.handle_format_selection_key(key),
            1 => self.handle_quality_options_key(key),
            2 => self.handle_additional_options_key(key),
            3 => self.handle_confirmation_key(key),
            _ => false,
        };

        // If the key wasn't handled and it's Escape, exit the wizard
        if !handled && key.code == KeyCode::Esc {
            self.should_exit = true;
            return true;
        }

        handled
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, button_id: Option<ButtonId>) -> bool {
        // Debug logging
        use std::fs::OpenOptions;
        use std::io::Write;
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open("wizard_areas.log")
        {
            let _ = writeln!(
                file,
                "handle_mouse called: mouse.kind={:?}, button_id={:?}",
                mouse.kind, button_id
            );
        }

        // Handle mouse move for hover highlighting
        if mouse.kind == MouseEventKind::Moved {
            self.hovered_button = button_id;
            return true; // Return true to trigger redraw
        }

        // Handle scroll events for file browser popup
        if let Some(popup_state) = &mut self.popup_state {
            if let PopupType::FileBrowser(browser) = &mut popup_state.popup_type {
                match mouse.kind {
                    MouseEventKind::ScrollDown => {
                        // Scroll down by 3 items
                        let new_index = (browser.selected_index + 3)
                            .min(browser.entries.len().saturating_sub(1));
                        browser.selected_index = new_index;
                        return true;
                    }
                    MouseEventKind::ScrollUp => {
                        // Scroll up by 3 items
                        browser.selected_index = browser.selected_index.saturating_sub(3);
                        return true;
                    }
                    _ => {}
                }
            }
        }

        if mouse.kind != MouseEventKind::Down(crossterm::event::MouseButton::Left) {
            return false;
        }

        // Don't automatically close help on any click - let specific handlers manage this
        // This allows clicking on info icons to switch between different help popups

        match button_id {
            Some(ButtonId::Back) => {
                self.previous_step();
                true
            }
            Some(ButtonId::Next) => {
                if self.current_step == 3 {
                    // Check if we need to ask for destination
                    if self.destination_mode == DestinationMode::AskEveryTime {
                        self.needs_destination_selection = true;
                        self.show_destination_browser();
                    } else {
                        self.should_start_conversion = true;
                    }
                } else {
                    self.next_step();
                }
                true
            }
            Some(ButtonId::Cancel) => {
                if let Ok(mut file) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("wizard_areas.log")
                {
                    let _ = writeln!(file, "CANCEL BUTTON CLICKED - Setting should_exit flag");
                }
                self.should_exit = true; // Set flag to exit wizard
                true // Button was handled
            }
            Some(ButtonId::SavePreset) => {
                // Show the preset name popup
                self.popup_state = Some(PopupState {
                    popup_type: PopupType::PresetName,
                    input_text: String::new(),
                    cursor_pos: 0,
                    view_offset: 0,
                    error_message: None,
                    focused_element: PopupFocus::Input,
                });
                true
            }
            Some(ButtonId::BrowseButton) => {
                // Browse button clicked - open file browser
                use std::env;
                use std::path::PathBuf;

                // Get current path or use home directory
                let start_path = match &self.destination_mode {
                    DestinationMode::Custom(path) if !path.is_empty() => PathBuf::from(path),
                    _ => env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
                };

                let browser = crate::types::FileBrowser::new(start_path);
                self.popup_state = Some(PopupState {
                    popup_type: PopupType::FileBrowser(Box::new(browser)),
                    input_text: String::new(),
                    cursor_pos: 0,
                    view_offset: 0,
                    error_message: None,
                    focused_element: PopupFocus::Input,
                });
                true
            }
            Some(ButtonId::LoadPreset) => {
                // Load the list of available presets
                match PresetManager::new() {
                    Ok(manager) => {
                        match manager.list_presets() {
                            Ok(presets) => {
                                if presets.is_empty() {
                                    // No presets available - show a message popup
                                    self.popup_state = Some(PopupState {
                                        popup_type: PopupType::PresetName,
                                        input_text: String::new(),
                                        cursor_pos: 0,
                                        view_offset: 0,
                                        error_message: Some(
                                            "No presets found. Save a preset first!".to_string(),
                                        ),
                                        focused_element: PopupFocus::OkButton,
                                    });
                                } else {
                                    // Show preset selection popup
                                    self.popup_state = Some(PopupState {
                                        popup_type: PopupType::PresetList {
                                            presets,
                                            selected_index: 0,
                                        },
                                        input_text: String::new(),
                                        cursor_pos: 0,
                                        view_offset: 0,
                                        error_message: None,
                                        focused_element: PopupFocus::Input,
                                    });
                                }
                            }
                            Err(e) => {
                                if let Ok(mut file) = OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open("wizard_areas.log")
                                {
                                    let _ = writeln!(file, "Failed to list presets: {}", e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if let Ok(mut file) = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open("wizard_areas.log")
                        {
                            let _ = writeln!(file, "Failed to create PresetManager: {}", e);
                        }
                    }
                }
                true
            }
            Some(ButtonId::PopupOk) => {
                if let Some(popup_state) = &self.popup_state {
                    match &popup_state.popup_type {
                        PopupType::PresetName => {
                            // Check if preset already exists
                            match PresetManager::new() {
                                Ok(manager) => {
                                    let preset_name = popup_state.input_text.clone();
                                    if manager.preset_exists(&preset_name) {
                                        // Show overwrite confirmation
                                        self.popup_state = Some(PopupState {
                                            popup_type: PopupType::OverwriteConfirm { preset_name },
                                            input_text: String::new(),
                                            cursor_pos: 0,
                                            view_offset: 0,
                                            error_message: None,
                                            focused_element: PopupFocus::Input,
                                        });
                                        return true;
                                    }

                                    // Preset doesn't exist, save it
                                    let mut preset = ConversionPreset::from(&*self);
                                    preset.name = preset_name;

                                    match manager.save_preset(&preset) {
                                        Ok(_) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ = writeln!(
                                                    file,
                                                    "Successfully saved preset: {}",
                                                    preset.name
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ =
                                                    writeln!(file, "Failed to save preset: {}", e);
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    if let Ok(mut file) = OpenOptions::new()
                                        .create(true)
                                        .append(true)
                                        .open("wizard_areas.log")
                                    {
                                        let _ = writeln!(
                                            file,
                                            "Failed to create preset manager: {}",
                                            e
                                        );
                                    }
                                }
                            }
                        }
                        PopupType::OverwriteConfirm { preset_name } => {
                            // User confirmed overwrite
                            match PresetManager::new() {
                                Ok(manager) => {
                                    let mut preset = ConversionPreset::from(&*self);
                                    preset.name = preset_name.clone();

                                    match manager.save_preset(&preset) {
                                        Ok(_) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ = writeln!(
                                                    file,
                                                    "Successfully overwrote preset: {}",
                                                    preset_name
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ = writeln!(
                                                    file,
                                                    "Failed to overwrite preset: {}",
                                                    e
                                                );
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    if let Ok(mut file) = OpenOptions::new()
                                        .create(true)
                                        .append(true)
                                        .open("wizard_areas.log")
                                    {
                                        let _ = writeln!(
                                            file,
                                            "Failed to create preset manager: {}",
                                            e
                                        );
                                    }
                                }
                            }
                        }
                        PopupType::TextInput { field } => {
                            // Apply the text input to the appropriate field
                            match field {
                                EditingField::CopyFiles => {
                                    self.copy_files_extensions = popup_state.input_text.clone();
                                }
                                EditingField::CopySubdirectories => {
                                    self.copy_subdirectories = popup_state.input_text.clone();
                                }
                                EditingField::CustomDestination => {
                                    // Validate the path
                                    let input_path = popup_state.input_text.trim();
                                    if input_path.is_empty() {
                                        // Empty path is allowed (will use default)
                                        if let DestinationMode::Custom(ref mut path) =
                                            self.destination_mode
                                        {
                                            *path = String::new();
                                        } else {
                                            self.destination_mode =
                                                DestinationMode::Custom(String::new());
                                        }
                                    } else {
                                        // Check if path is valid
                                        let path = std::path::Path::new(input_path);

                                        if path.exists() && path.is_dir() {
                                            // Path exists and is a directory - valid
                                            if let DestinationMode::Custom(ref mut dest_path) =
                                                self.destination_mode
                                            {
                                                *dest_path = input_path.to_string();
                                            } else {
                                                self.destination_mode =
                                                    DestinationMode::Custom(input_path.to_string());
                                            }
                                        } else if let Some(parent) = path.parent() {
                                            // Check if parent exists (we can create the final folder)
                                            if parent.exists() && parent.is_dir() {
                                                // Additional check: verify we can actually write to the parent
                                                // For paths like /your-mom, even though / exists, we likely can't write there
                                                let test_path = parent.join(".convert_wizard_test");
                                                match std::fs::File::create(&test_path) {
                                                    Ok(_) => {
                                                        // We can write here, clean up test file
                                                        let _ = std::fs::remove_file(&test_path);

                                                        // Store the path as-is, we'll create the folder during conversion
                                                        if let DestinationMode::Custom(
                                                            ref mut dest_path,
                                                        ) = self.destination_mode
                                                        {
                                                            *dest_path = input_path.to_string();
                                                        } else {
                                                            self.destination_mode =
                                                                DestinationMode::Custom(
                                                                    input_path.to_string(),
                                                                );
                                                        }
                                                    }
                                                    Err(_) => {
                                                        // Can't write to parent directory
                                                        if let Some(ref mut popup) =
                                                            self.popup_state
                                                        {
                                                            popup.error_message = Some("Invalid Path - No write permission".to_string());
                                                        }
                                                        return true; // Don't close popup
                                                    }
                                                }
                                            } else {
                                                // Invalid path - keep popup open with error
                                                if let Some(ref mut popup) = self.popup_state {
                                                    popup.error_message =
                                                        Some("Invalid Path".to_string());
                                                }
                                                return true; // Don't close popup
                                            }
                                        } else {
                                            // Invalid path - keep popup open with error
                                            if let Some(ref mut popup) = self.popup_state {
                                                popup.error_message =
                                                    Some("Invalid Path".to_string());
                                            }
                                            return true; // Don't close popup
                                        }
                                    }
                                }
                            }
                        }
                        PopupType::PresetList {
                            presets,
                            selected_index,
                        } => {
                            // Load the selected preset
                            if let Some(preset_name) = presets.get(*selected_index) {
                                match PresetManager::new() {
                                    Ok(manager) => match manager.load_preset(preset_name) {
                                        Ok(preset) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ = writeln!(
                                                    file,
                                                    "Successfully loaded preset: {}",
                                                    preset_name
                                                );
                                                let _ = writeln!(
                                                    file,
                                                    "Preset format: {:?}",
                                                    preset.selected_format
                                                );
                                            }
                                            self.load_preset(&preset);
                                        }
                                        Err(e) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ =
                                                    writeln!(file, "Failed to load preset: {}", e);
                                            }
                                        }
                                    },
                                    Err(e) => {
                                        if let Ok(mut file) = OpenOptions::new()
                                            .create(true)
                                            .append(true)
                                            .open("wizard_areas.log")
                                        {
                                            let _ = writeln!(
                                                file,
                                                "Failed to create preset manager: {}",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        PopupType::FileBrowser(_) | PopupType::NewFolder { .. } => {
                            // These are handled elsewhere
                        }
                    }
                }
                self.popup_state = None;
                true
            }
            Some(ButtonId::PopupCancel) => {
                self.popup_state = None;
                true
            }
            Some(ButtonId::PopupBackground) => {
                // Capture click on popup background to prevent click-through
                true
            }
            Some(ButtonId::FormatOption(idx)) => {
                if self.current_step == 0 {
                    self.selected_index = idx;
                    self.select_format();
                } else if self.current_step == 1 && self.selected_format != Some(AudioFormat::Flac)
                {
                    self.selected_index = idx;
                    self.select_quality();
                }
                true
            }
            Some(ButtonId::QualityOption(idx)) => {
                if self.current_step == 0
                    && self.selected_format.is_some()
                    && self.selected_format != Some(AudioFormat::Flac)
                {
                    self.quality_index = idx;
                    self.select_quality();
                }
                true
            }
            Some(ButtonId::BitDepthOption(idx)) => {
                if self.current_step == 1 {
                    // Page 1: Bit depth for all lossless formats
                    self.resampling_page_section = FlacSection::BitDepth;
                    self.selected_index = idx;
                    self.select_bit_depth();
                }
                true
            }
            Some(ButtonId::SampleRateOption(idx)) => {
                if self.current_step == 1 {
                    // Page 1: Sample rate for all formats
                    self.resampling_page_section = FlacSection::SampleRate;
                    self.selected_index = idx;
                    self.select_sample_rate();
                }
                true
            }
            Some(ButtonId::CompressionLevelOption(idx)) => {
                if self.current_step == 0 && self.selected_format == Some(AudioFormat::Flac) {
                    self.in_quality_area = true;
                    self.quality_index = idx;
                    self.select_compression_level();
                }
                true
            }
            Some(ButtonId::ResampleQualityOption(idx)) => {
                if self.current_step == 1 {
                    // Page 1: Resample quality for all formats
                    self.resampling_page_section = FlacSection::ResamplingQuality;
                    self.selected_index = idx;
                    self.select_resample_quality();
                }
                true
            }
            Some(ButtonId::DitherOption(idx)) => {
                if self.current_step == 1 {
                    // Page 1: Dithering for all lossless formats
                    self.resampling_page_section = FlacSection::Dithering;
                    self.selected_index = idx;
                    self.select_dither_type();
                }
                true
            }
            Some(ButtonId::ProcessingOption(idx)) => {
                if self.current_step == 0 && self.selected_format == Some(AudioFormat::Flac) {
                    self.in_quality_area = true;
                    self.quality_index = 3 + idx; // 3 compression options + processing options
                    self.toggle_processing_option();
                } else if self.current_step == 0
                    && self.selected_format == Some(AudioFormat::WavPack)
                {
                    if idx == 1 {
                        // Toggle verify for WavPack
                        self.verify_encoding = Some(!self.verify_encoding.unwrap_or(true));
                    } else if idx == 3 {
                        // Toggle MD5 for WavPack
                        self.store_md5 = Some(!self.store_md5.unwrap_or(true));
                    }
                }
                true
            }
            Some(ButtonId::NyquistTransitionOption(idx)) => {
                if self.current_step == 1 {
                    self.resampling_page_section = FlacSection::NyquistTransition;
                    self.selected_index = idx;
                    self.select_nyquist_transition();
                }
                true
            }
            Some(ButtonId::SsrcInsaneCheckbox) => {
                if self.current_step == 1 && self.is_insane_mode_available() {
                    self.ssrc_insane_mode = Some(!self.ssrc_insane_mode.unwrap_or(false));
                }
                true
            }
            Some(ButtonId::AdditionalOption(idx)) => {
                if self.current_step == 2 {
                    // Check for double-click on editable fields
                    let now = std::time::Instant::now();
                    let is_double_click = self.last_click_field == Some(idx)
                        && now.duration_since(self.last_click_time).as_millis() < 500;

                    self.additional_options_index = idx;

                    if idx == 4 || idx == 5 {
                        if is_double_click {
                            // Double-click: show popup
                            match idx {
                                4 => {
                                    self.popup_state = Some(PopupState {
                                        popup_type: PopupType::TextInput {
                                            field: EditingField::CopyFiles,
                                        },
                                        input_text: self.copy_files_extensions.clone(),
                                        cursor_pos: self.copy_files_extensions.len(),
                                        view_offset: 0,
                                        error_message: None,
                                        focused_element: PopupFocus::Input,
                                    });
                                }
                                5 => {
                                    self.popup_state = Some(PopupState {
                                        popup_type: PopupType::TextInput {
                                            field: EditingField::CopySubdirectories,
                                        },
                                        input_text: self.copy_subdirectories.clone(),
                                        cursor_pos: self.copy_subdirectories.len(),
                                        view_offset: 0,
                                        error_message: None,
                                        focused_element: PopupFocus::Input,
                                    });
                                }
                                _ => {}
                            }
                        }
                        // Single click just selects the field
                    } else if idx == 8 {
                        if is_double_click {
                            // Double-click on custom path: show popup
                            if let DestinationMode::Custom(ref path) = self.destination_mode {
                                self.popup_state = Some(PopupState {
                                    popup_type: PopupType::TextInput {
                                        field: EditingField::CustomDestination,
                                    },
                                    input_text: path.clone(),
                                    cursor_pos: path.len(),
                                    view_offset: 0,
                                    error_message: None,
                                    focused_element: PopupFocus::Input,
                                });
                            } else {
                                // Switch to custom mode and show popup
                                self.destination_mode = DestinationMode::Custom(String::new());
                                self.popup_state = Some(PopupState {
                                    popup_type: PopupType::TextInput {
                                        field: EditingField::CustomDestination,
                                    },
                                    input_text: String::new(),
                                    cursor_pos: 0,
                                    view_offset: 0,
                                    error_message: None,
                                    focused_element: PopupFocus::Input,
                                });
                            }
                        } else {
                            // Single click: select custom mode
                            self.toggle_additional_option();
                        }
                    } else {
                        // Other options toggle immediately
                        self.toggle_additional_option();
                    }

                    self.last_click_field = Some(idx);
                    self.last_click_time = now;
                }
                true
            }
            Some(ButtonId::AdditionalOptionCheckbox(idx)) => {
                if self.current_step == 2 {
                    match idx {
                        4 => self.copy_files_enabled = !self.copy_files_enabled,
                        5 => self.copy_subdirectories_enabled = !self.copy_subdirectories_enabled,
                        _ => {}
                    }
                }
                true
            }
            Some(ButtonId::InfoIcon(section)) => {
                // Debug logging - log before any conditions
                use std::fs::OpenOptions;
                use std::io::Write;
                if let Ok(mut file) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("wizard_areas.log")
                {
                    let _ = writeln!(file, "\nInfoIcon handler called:");
                    let _ = writeln!(file, "  section: {:?}", section);
                    let _ = writeln!(file, "  current_step: {}", self.current_step);
                    let _ = writeln!(file, "  selected_format: {:?}", self.selected_format);
                    let _ = writeln!(file, "  show_help_for before: {:?}", self.show_help_for);
                }

                if (self.current_step == 0
                    && (self.selected_format == Some(AudioFormat::Flac)
                        || self.selected_format == Some(AudioFormat::WavPack)))
                    || self.current_step == 1
                    || self.current_step == 2
                {
                    // Toggle help display
                    if self.show_help_for == Some(section) {
                        self.show_help_for = None;
                    } else {
                        self.show_help_for = Some(section);
                        self.help_page = 0;
                    }

                    if let Ok(mut file) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("wizard_areas.log")
                    {
                        let _ = writeln!(file, "  show_help_for after: {:?}", self.show_help_for);
                    }
                }
                true
            }
            Some(ButtonId::AdditionalInfoIcon(help_section)) => {
                if self.current_step == 2 {
                    // Toggle additional help display
                    if self.show_additional_help_for == Some(help_section) {
                        self.show_additional_help_for = None;
                    } else {
                        self.show_additional_help_for = Some(help_section);
                    }
                }
                true
            }
            Some(ButtonId::LosslessInfoIcon) => {
                if self.current_step == 0 {
                    // Toggle lossless help display (using CopyFiles as placeholder)
                    if self.show_additional_help_for == Some(AdditionalOptionsHelp::CopyFiles) {
                        self.show_additional_help_for = None;
                    } else {
                        self.show_additional_help_for = Some(AdditionalOptionsHelp::CopyFiles);
                    }
                }
                true
            }
            Some(ButtonId::LossyInfoIcon) => {
                if self.current_step == 0 {
                    // Toggle lossy help display (using CopySubdirectories as placeholder)
                    if self.show_additional_help_for
                        == Some(AdditionalOptionsHelp::CopySubdirectories)
                    {
                        self.show_additional_help_for = None;
                    } else {
                        self.show_additional_help_for =
                            Some(AdditionalOptionsHelp::CopySubdirectories);
                    }
                }
                true
            }
            Some(ButtonId::FormatInfoIcon(help_section)) => {
                if self.current_step == 0 {
                    // Toggle format-specific help display
                    if self.show_format_help_for == Some(help_section) {
                        self.show_format_help_for = None;
                    } else {
                        self.show_format_help_for = Some(help_section);
                    }
                }
                true
            }
            Some(ButtonId::PresetItem(idx)) => {
                // User clicked on a preset in the list
                if let Some(popup_state) = &mut self.popup_state {
                    if let PopupType::PresetList {
                        presets,
                        selected_index,
                    } = &mut popup_state.popup_type
                    {
                        // Handle double-click detection
                        let now = std::time::Instant::now();
                        if self.last_click_field == Some(idx)
                            && now.duration_since(self.last_click_time).as_millis() < 500
                        {
                            // Double-click detected - load the preset
                            if let Some(preset_name) = presets.get(idx) {
                                match PresetManager::new() {
                                    Ok(manager) => match manager.load_preset(preset_name) {
                                        Ok(preset) => {
                                            self.load_preset(&preset);
                                            self.popup_state = None;
                                            self.last_click_field = None;
                                        }
                                        Err(e) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ =
                                                    writeln!(file, "Failed to load preset: {}", e);
                                            }
                                        }
                                    },
                                    Err(e) => {
                                        if let Ok(mut file) = OpenOptions::new()
                                            .create(true)
                                            .append(true)
                                            .open("wizard_areas.log")
                                        {
                                            let _ = writeln!(
                                                file,
                                                "Failed to create preset manager: {}",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        } else {
                            // Single click - just select
                            *selected_index = idx;
                            self.last_click_field = Some(idx);
                            self.last_click_time = now;
                        }
                    }
                }
                true
            }
            Some(ButtonId::FileItem(idx)) => {
                // Handle file browser item clicks
                if let Some(popup_state) = &mut self.popup_state {
                    if let PopupType::FileBrowser(browser) = &mut popup_state.popup_type {
                        let action =
                            handle_file_browser_mouse(browser, mouse, ButtonId::FileItem(idx));
                        match action {
                            crate::types::BrowserAction::Selected(path) => {
                                self.destination_mode =
                                    DestinationMode::Custom(path.to_string_lossy().to_string());
                                self.popup_state = None;
                                // If we were selecting destination for conversion, now start conversion
                                if self.needs_destination_selection {
                                    self.needs_destination_selection = false;
                                    self.should_start_conversion = true;
                                }
                                return true;
                            }
                            crate::types::BrowserAction::Cancelled => {
                                self.popup_state = None;
                                // If we were selecting destination for conversion, cancel the conversion
                                if self.needs_destination_selection {
                                    self.needs_destination_selection = false;
                                    // Don't start conversion
                                }
                                return true;
                            }
                            crate::types::BrowserAction::Continue => return true,
                        }
                    }
                }
                true
            }
            Some(ButtonId::NewFolder) => {
                // Handle New folder button click
                if let Some(popup_state) = &mut self.popup_state {
                    if let PopupType::FileBrowser(browser) = &mut popup_state.popup_type {
                        let action = handle_file_browser_mouse(browser, mouse, ButtonId::NewFolder);
                        match action {
                            crate::types::BrowserAction::Continue => {
                                // Check if we need to show new folder popup
                                if browser.show_new_folder_popup {
                                    browser.show_new_folder_popup = false;
                                    let parent_path = browser.current_path.clone();
                                    self.popup_state = Some(PopupState {
                                        popup_type: PopupType::NewFolder { parent_path },
                                        input_text: String::new(),
                                        cursor_pos: 0,
                                        view_offset: 0,
                                        error_message: None,
                                        focused_element: PopupFocus::Input,
                                    });
                                }
                                return true;
                            }
                            _ => return true,
                        }
                    }
                }
                true
            }
            Some(ButtonId::FileBrowserSelect) => {
                // Handle Select button click in file browser
                if let Some(popup_state) = &self.popup_state {
                    if let PopupType::FileBrowser(browser) = &popup_state.popup_type {
                        self.destination_mode = DestinationMode::Custom(
                            browser.current_path.to_string_lossy().to_string(),
                        );
                        self.popup_state = None;
                    }
                }
                true
            }
            Some(ButtonId::FileBrowserCancel) => {
                // Handle Cancel button click in file browser
                self.popup_state = None;
                true
            }
            None => {
                // If any help is showing, close it on click in empty area
                if self.show_help_for.is_some()
                    || self.show_additional_help_for.is_some()
                    || self.show_format_help_for.is_some()
                {
                    self.show_help_for = None;
                    self.show_additional_help_for = None;
                    self.show_format_help_for = None;
                    self.help_page = 0; // Reset help page when closing
                    true
                } else {
                    false
                }
            }
        }
    }

    fn handle_format_selection_key(&mut self, key: KeyEvent) -> bool {
        // Debug which path we're taking
        use std::fs::OpenOptions;
        use std::io::Write;
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open("wizard_keys.log")
        {
            let _ = writeln!(file, "  handle_format_selection_key: is_in_quality_options()={}, focused_nav_button={:?}", 
                           self.is_in_quality_options(), self.focused_nav_button);
        }

        // First check if Esc is pressed and we have help showing
        if key.code == KeyCode::Esc
            && (self.show_help_for.is_some()
                || self.show_additional_help_for.is_some()
                || self.show_format_help_for.is_some())
        {
            self.show_help_for = None;
            self.show_additional_help_for = None;
            self.show_format_help_for = None;
            return true;
        }

        // Handle navigation button focus
        if let Some(nav_button) = &self.focused_nav_button {
            match key.code {
                KeyCode::Tab => {
                    // Tab through navigation buttons
                    match nav_button {
                        ButtonId::LoadPreset => self.focused_nav_button = Some(ButtonId::Next),
                        ButtonId::Next => self.focused_nav_button = Some(ButtonId::Cancel),
                        ButtonId::Cancel => {
                            // Return to format list
                            self.focused_nav_button = None;
                            self.selected_index = 0;
                            self.quality_index = 0;
                            self.in_quality_area = false;
                        }
                        _ => {}
                    }
                    return true;
                }
                KeyCode::Enter => {
                    // Activate the focused button
                    match nav_button {
                        ButtonId::LoadPreset => {
                            // Trigger load preset action
                            self.handle_mouse(
                                MouseEvent {
                                    kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                                    column: 0,
                                    row: 0,
                                    modifiers: crossterm::event::KeyModifiers::empty(),
                                },
                                Some(ButtonId::LoadPreset),
                            );
                        }
                        ButtonId::Next => {
                            if self.selected_format.is_some() {
                                self.next_step();
                            }
                            self.focused_nav_button = None;
                        }
                        ButtonId::Cancel => {
                            self.should_exit = true;
                            self.focused_nav_button = None; // Clear focus so UI updates
                        }
                        _ => {}
                    }
                    return true;
                }
                _ => return false,
            }
        }

        // Check if we're in FLAC options (right side)
        if self.selected_format == Some(AudioFormat::Flac) && self.in_quality_area {
            // Handle navigation within FLAC options
            match key.code {
                KeyCode::Tab => {
                    // Move to navigation buttons
                    self.focused_nav_button = Some(ButtonId::LoadPreset);
                    true
                }
                KeyCode::BackTab => {
                    // Return to format list
                    self.quality_index = 0;
                    self.in_quality_area = false;
                    true
                }
                KeyCode::Up => {
                    if self.quality_index > 0 {
                        self.quality_index -= 1;
                    }
                    true
                }
                KeyCode::Down => {
                    // FLAC has 3 compression options + 2 processing options
                    if self.quality_index < 4 {
                        self.quality_index += 1;
                    }
                    true
                }
                KeyCode::Char(' ') => {
                    if self.quality_index < 3 {
                        // Compression options
                        self.select_compression_level();
                    } else {
                        // Processing options
                        self.toggle_processing_option();
                    }
                    true
                }
                KeyCode::Enter => {
                    self.next_step();
                    true
                }
                KeyCode::Esc => {
                    // If help is shown, close it; otherwise handle normally
                    if self.show_help_for.is_some() {
                        self.show_help_for = None;
                        true
                    } else if self.show_additional_help_for.is_some() {
                        self.show_additional_help_for = None;
                        true
                    } else {
                        // Return to format list
                        self.quality_index = 0;
                        self.in_quality_area = false;
                        true
                    }
                }
                _ => self.handle_format_list_navigation(key),
            }
        } else if self.selected_format.is_some() && self.is_in_quality_options() {
            // Debug logging
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("wizard_keys.log")
            {
                let _ = writeln!(file, "  --> In quality options handler!");
            }
            // Handle navigation within quality options for formats that show them on the right
            match key.code {
                KeyCode::Tab => {
                    // Move to navigation buttons
                    self.focused_nav_button = Some(ButtonId::LoadPreset);
                    true
                }
                KeyCode::BackTab => {
                    // Return to format list
                    self.quality_index = 0;
                    self.in_quality_area = false;
                    true
                }
                KeyCode::Up => {
                    if self.quality_index > 0 {
                        self.quality_index -= 1;
                    }
                    true
                }
                KeyCode::Down => {
                    let max_index = match self.selected_format {
                        Some(AudioFormat::Mp3) => 5, // 6 quality options (0-5)
                        Some(AudioFormat::Aac) => {
                            // 3 profiles + dynamic number of bitrates - 1
                            let bitrates = self.get_aac_bitrates();
                            3 + bitrates.len() - 1
                        }
                        Some(AudioFormat::Opus) => 6, // 5 qualities + 2 content types - 1
                        Some(AudioFormat::WavPack) => 7, // 6 compression options + 2 checkboxes
                        _ => 0,
                    };

                    // Debug logging
                    use std::fs::OpenOptions;
                    use std::io::Write;
                    if let Ok(mut file) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("wizard_keys.log")
                    {
                        let _ = writeln!(file, "Down key in quality options: format={:?}, quality_index={}, max_index={}", 
                                       self.selected_format, self.quality_index, max_index);
                    }

                    if let Ok(mut file) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("wizard_keys.log")
                    {
                        let _ = writeln!(
                            file,
                            "  BEFORE INCREMENT: quality_index={}, max_index={}, can increment={}",
                            self.quality_index,
                            max_index,
                            self.quality_index < max_index
                        );
                    }

                    if self.quality_index < max_index {
                        let old_index = self.quality_index;
                        self.quality_index += 1;

                        if let Ok(mut file) = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open("wizard_keys.log")
                        {
                            let _ = writeln!(
                                file,
                                "  AFTER INCREMENT: {} -> {}",
                                old_index, self.quality_index
                            );
                            if old_index == 3 && self.quality_index == 4 {
                                let _ = writeln!(
                                    file,
                                    "  *** SUCCESSFULLY MOVED FROM VERY HIGH TO INSANE ***"
                                );
                            }
                        }
                    } else {
                        if let Ok(mut file) = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open("wizard_keys.log")
                        {
                            let _ = writeln!(file, "  AT MAX, NOT INCREMENTING");
                        }
                    }
                    true
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if self.selected_format == Some(AudioFormat::WavPack) {
                        if self.quality_index == 6 {
                            // Toggle verify checkbox
                            self.verify_encoding = Some(!self.verify_encoding.unwrap_or(true));
                        } else if self.quality_index == 7 {
                            // Toggle MD5 checkbox
                            self.store_md5 = Some(!self.store_md5.unwrap_or(true));
                        } else {
                            self.select_quality();
                        }
                    } else {
                        self.select_quality();
                    }
                    if key.code == KeyCode::Enter {
                        self.next_step();
                    }
                    true
                }
                KeyCode::Esc => {
                    // Return to format list
                    self.quality_index = 0;
                    self.in_quality_area = false;
                    true
                }
                KeyCode::Char('i') => {
                    // Show help based on format
                    match self.selected_format {
                        Some(AudioFormat::WavPack) => {
                            if self.quality_index >= 6 {
                                // Show help for additional options (both checkboxes)
                                if self.show_help_for == Some(FlacSection::ProcessingOptions) {
                                    self.show_help_for = None;
                                } else {
                                    self.show_help_for = Some(FlacSection::ProcessingOptions);
                                    self.help_page = 0;
                                }
                            }
                        }
                        Some(AudioFormat::Aac) => {
                            let profiles =
                                vec![AacProfile::LcAac, AacProfile::HeAac, AacProfile::HeAacV2];
                            if self.quality_index < profiles.len() {
                                // Show AAC profile help
                                if self.show_format_help_for == Some(FormatSpecificHelp::AacProfile)
                                {
                                    self.show_format_help_for = None;
                                } else {
                                    self.show_format_help_for =
                                        Some(FormatSpecificHelp::AacProfile);
                                }
                            } else {
                                // Show AAC bitrate help
                                if self.show_format_help_for == Some(FormatSpecificHelp::AacBitrate)
                                {
                                    self.show_format_help_for = None;
                                } else {
                                    self.show_format_help_for =
                                        Some(FormatSpecificHelp::AacBitrate);
                                }
                            }
                        }
                        Some(AudioFormat::Mp3) => {
                            // Show MP3 bitrate help
                            if self.show_format_help_for == Some(FormatSpecificHelp::Mp3Bitrate) {
                                self.show_format_help_for = None;
                            } else {
                                self.show_format_help_for = Some(FormatSpecificHelp::Mp3Bitrate);
                            }
                        }
                        Some(AudioFormat::Opus) => {
                            if self.quality_index < 5 {
                                // Show Opus quality help
                                if self.show_format_help_for
                                    == Some(FormatSpecificHelp::OpusQuality)
                                {
                                    self.show_format_help_for = None;
                                } else {
                                    self.show_format_help_for =
                                        Some(FormatSpecificHelp::OpusQuality);
                                }
                            } else {
                                // Show Opus content type help
                                if self.show_format_help_for
                                    == Some(FormatSpecificHelp::OpusContentType)
                                {
                                    self.show_format_help_for = None;
                                } else {
                                    self.show_format_help_for =
                                        Some(FormatSpecificHelp::OpusContentType);
                                }
                            }
                        }
                        _ => {}
                    }
                    true
                }
                _ => false, // Don't handle other keys when in quality options
            }
        } else {
            // Debug logging
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("wizard_keys.log")
            {
                let _ = writeln!(file, "  --> Falling through to format list navigation");
            }
            // Handle format list navigation
            self.handle_format_list_navigation(key)
        }
    }

    fn handle_format_list_navigation(&mut self, key: KeyEvent) -> bool {
        // Debug logging
        use std::fs::OpenOptions;
        use std::io::Write;
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open("wizard_keys.log")
        {
            let _ = writeln!(
                file,
                "  --> handle_format_list_navigation called with key: {:?}",
                key.code
            );
        }

        match key.code {
            KeyCode::Tab => {
                // Move to quality options for formats that have them
                if self.selected_format == Some(AudioFormat::Flac)
                    || self.selected_format == Some(AudioFormat::WavPack)
                    || self.selected_format == Some(AudioFormat::Mp3)
                    || self.selected_format == Some(AudioFormat::Aac)
                    || self.selected_format == Some(AudioFormat::Opus)
                {
                    // Move to quality options
                    self.quality_index = 0;
                    self.in_quality_area = true;
                    true
                } else {
                    // No quality options, go directly to navigation buttons
                    self.focused_nav_button = Some(ButtonId::LoadPreset);
                    true
                }
            }
            KeyCode::Up => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                }
                true
            }
            KeyCode::Down => {
                if self.selected_index < 6 {
                    self.selected_index += 1;
                }
                true
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.select_format();
                if key.code == KeyCode::Enter && self.selected_format.is_some() {
                    self.next_step();
                }
                true
            }
            KeyCode::Esc => {
                // If help is shown, close it; otherwise let main handler exit
                if self.show_help_for.is_some() {
                    self.show_help_for = None;
                    true
                } else if self.show_additional_help_for.is_some() {
                    self.show_additional_help_for = None;
                    true
                } else {
                    // Let the main handler exit the wizard
                    false
                }
            }
            KeyCode::Char('q') => true, // Exit signal
            KeyCode::Char('i') => {
                // Show help for the currently focused format
                if self.selected_index < 4 {
                    // Lossless formats - show lossless help
                    if self.show_additional_help_for == Some(AdditionalOptionsHelp::CopyFiles) {
                        self.show_additional_help_for = None;
                    } else {
                        self.show_additional_help_for = Some(AdditionalOptionsHelp::CopyFiles);
                    }
                } else {
                    // Lossy formats - show lossy help
                    if self.show_additional_help_for
                        == Some(AdditionalOptionsHelp::CopySubdirectories)
                    {
                        self.show_additional_help_for = None;
                    } else {
                        self.show_additional_help_for =
                            Some(AdditionalOptionsHelp::CopySubdirectories);
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn handle_quality_options_key(&mut self, key: KeyEvent) -> bool {
        // Handle navigation button focus first
        if let Some(nav_button) = &self.focused_nav_button {
            match key.code {
                KeyCode::Tab => {
                    // Tab through navigation buttons
                    match nav_button {
                        ButtonId::Back => self.focused_nav_button = Some(ButtonId::Next),
                        ButtonId::Next => self.focused_nav_button = Some(ButtonId::Cancel),
                        ButtonId::Cancel => {
                            // Return to quality options
                            self.focused_nav_button = None;
                            self.selected_index = 0;
                            self.resampling_page_section = FlacSection::BitDepth;
                        }
                        _ => {}
                    }
                    return true;
                }
                KeyCode::Enter => {
                    // Activate the focused button
                    match nav_button {
                        ButtonId::Back => {
                            self.previous_step();
                            self.focused_nav_button = None;
                        }
                        ButtonId::Next => {
                            self.next_step();
                            self.focused_nav_button = None;
                        }
                        ButtonId::Cancel => {
                            self.should_exit = true;
                            self.focused_nav_button = None;
                        }
                        _ => {}
                    }
                    return true;
                }
                _ => return false,
            }
        }

        match self.selected_format {
            Some(AudioFormat::Flac)
            | Some(AudioFormat::Wav)
            | Some(AudioFormat::Aiff)
            | Some(AudioFormat::WavPack) => {
                // All lossless formats use resampling page navigation on step 1
                self.handle_resampling_page_key(key)
            }
            Some(AudioFormat::Mp3) | Some(AudioFormat::Aac) | Some(AudioFormat::Opus) => {
                // Lossy formats also show resampling options
                self.handle_resampling_page_key(key)
            }
            None => false,
        }
    }

    fn handle_resampling_page_key(&mut self, key: KeyEvent) -> bool {
        // Debug logging
        use std::fs::OpenOptions;
        use std::io::Write;
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open("wizard_keys.log")
        {
            let _ = writeln!(
                file,
                "  handle_resampling_page_key called: key={:?}, section={:?}, index={}",
                key.code, self.resampling_page_section, self.selected_index
            );
        }

        match key.code {
            KeyCode::Tab => {
                // Move to next section
                self.selected_index = 0;
                self.resampling_page_section = match self.resampling_page_section {
                    FlacSection::BitDepth => {
                        if self.should_show_dithering() {
                            FlacSection::Dithering
                        } else {
                            FlacSection::SampleRate
                        }
                    }
                    FlacSection::Dithering => FlacSection::SampleRate,
                    FlacSection::SampleRate => {
                        if self.should_show_resampling() {
                            FlacSection::ResamplingQuality
                        } else {
                            // No resampling options, go to navigation buttons
                            self.focused_nav_button = Some(ButtonId::Back);
                            return true;
                        }
                    }
                    FlacSection::ResamplingQuality => {
                        if self.should_show_resampling()
                            && (self.selected_format == Some(AudioFormat::Flac)
                                || self.selected_format == Some(AudioFormat::Wav)
                                || self.selected_format == Some(AudioFormat::Aiff)
                                || self.selected_format == Some(AudioFormat::WavPack)
                                || self.selected_format == Some(AudioFormat::Opus))
                        {
                            FlacSection::NyquistTransition
                        } else {
                            // No Nyquist for MP3/AAC, go to navigation buttons
                            self.focused_nav_button = Some(ButtonId::Back);
                            return true;
                        }
                    }
                    FlacSection::NyquistTransition => {
                        // After the last section, go to navigation buttons
                        self.focused_nav_button = Some(ButtonId::Back);
                        return true;
                    }
                    _ => FlacSection::BitDepth,
                };
                true
            }
            KeyCode::BackTab => {
                // Move to previous section
                self.selected_index = 0;
                self.resampling_page_section = match self.resampling_page_section {
                    FlacSection::BitDepth => {
                        if self.should_show_resampling() {
                            FlacSection::NyquistTransition
                        } else {
                            FlacSection::SampleRate
                        }
                    }
                    FlacSection::SampleRate => {
                        // For lossy formats, we don't have bit depth/dithering
                        if self.selected_format == Some(AudioFormat::Mp3)
                            || self.selected_format == Some(AudioFormat::Aac)
                            || self.selected_format == Some(AudioFormat::Opus)
                        {
                            if self.should_show_resampling()
                                && self.selected_format == Some(AudioFormat::Opus)
                            {
                                FlacSection::NyquistTransition // Opus can have Nyquist
                            } else if self.should_show_resampling() {
                                FlacSection::ResamplingQuality // MP3/AAC stop at resampling
                            } else {
                                FlacSection::SampleRate // No resampling, stay here
                            }
                        } else if self.should_show_dithering() {
                            FlacSection::Dithering
                        } else {
                            FlacSection::BitDepth
                        }
                    }
                    FlacSection::Dithering => FlacSection::BitDepth,
                    FlacSection::ResamplingQuality => FlacSection::SampleRate,
                    FlacSection::NyquistTransition => {
                        if self.should_show_resampling() {
                            FlacSection::ResamplingQuality
                        } else {
                            FlacSection::SampleRate
                        }
                    }
                    _ => FlacSection::BitDepth,
                };
                true
            }
            KeyCode::Up => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                } else {
                    // Move to previous section
                    match self.resampling_page_section {
                        FlacSection::BitDepth => {
                            if self.should_show_resampling() {
                                self.resampling_page_section = FlacSection::NyquistTransition;
                                self.selected_index =
                                    Self::get_nyquist_transition_options().len() - 1;
                            } else {
                                self.resampling_page_section = FlacSection::SampleRate;
                                self.selected_index =
                                    self.get_sample_rate_options_for_format().len() - 1;
                            }
                        }
                        FlacSection::Dithering => {
                            self.resampling_page_section = FlacSection::BitDepth;
                            self.selected_index = Self::get_bit_depth_options().len() - 1;
                        }
                        FlacSection::SampleRate => {
                            if self.should_show_dithering() {
                                self.resampling_page_section = FlacSection::Dithering;
                                self.selected_index = self.get_dither_options().len() - 1;
                            } else {
                                self.resampling_page_section = FlacSection::BitDepth;
                                self.selected_index = Self::get_bit_depth_options().len() - 1;
                            }
                        }
                        FlacSection::ResamplingQuality => {
                            self.resampling_page_section = FlacSection::SampleRate;
                            self.selected_index =
                                self.get_sample_rate_options_for_format().len() - 1;
                        }
                        FlacSection::NyquistTransition => {
                            self.resampling_page_section = FlacSection::ResamplingQuality;
                            self.selected_index = Self::get_resample_quality_options().len() - 1;
                        }
                        _ => {}
                    }
                }
                true
            }
            KeyCode::Down => {
                let max_index = match self.resampling_page_section {
                    FlacSection::BitDepth => Self::get_bit_depth_options().len() - 1,
                    FlacSection::Dithering => self.get_dither_options().len() - 1,
                    FlacSection::SampleRate => self.get_sample_rate_options_for_format().len() - 1,
                    FlacSection::ResamplingQuality => {
                        Self::get_resample_quality_options().len() - 1
                    }
                    FlacSection::NyquistTransition => {
                        Self::get_nyquist_transition_options().len() - 1
                    }
                    _ => 0,
                };

                if self.selected_index < max_index {
                    self.selected_index += 1;
                } else {
                    // Move to next section
                    self.selected_index = 0;
                    self.resampling_page_section = match self.resampling_page_section {
                        FlacSection::BitDepth => {
                            if self.should_show_dithering() {
                                FlacSection::Dithering
                            } else {
                                FlacSection::SampleRate
                            }
                        }
                        FlacSection::Dithering => FlacSection::SampleRate,
                        FlacSection::SampleRate => {
                            if self.should_show_resampling() {
                                FlacSection::ResamplingQuality
                            } else {
                                FlacSection::BitDepth
                            }
                        }
                        FlacSection::ResamplingQuality => {
                            if self.should_show_resampling() {
                                FlacSection::NyquistTransition
                            } else {
                                FlacSection::BitDepth
                            }
                        }
                        FlacSection::NyquistTransition => {
                            if self.selected_format == Some(AudioFormat::Mp3)
                                || self.selected_format == Some(AudioFormat::Aac)
                                || self.selected_format == Some(AudioFormat::Opus)
                            {
                                FlacSection::SampleRate // For lossy formats, loop back to sample rate
                            } else {
                                FlacSection::BitDepth
                            }
                        }
                        _ => self.resampling_page_section,
                    };
                }
                true
            }
            KeyCode::Enter => {
                self.next_step();
                true
            }
            KeyCode::Char(' ') => {
                match self.resampling_page_section {
                    FlacSection::BitDepth => self.select_bit_depth(),
                    FlacSection::Dithering => self.select_dither_type(),
                    FlacSection::SampleRate => self.select_sample_rate(),
                    FlacSection::ResamplingQuality => self.select_resample_quality(),
                    FlacSection::NyquistTransition => self.select_nyquist_transition(),
                    _ => {}
                }
                true
            }
            KeyCode::Esc => {
                // If help is shown, close it; otherwise let main handler exit
                if self.show_help_for.is_some() {
                    self.show_help_for = None;
                    true
                } else if self.show_additional_help_for.is_some() {
                    self.show_additional_help_for = None;
                    true
                } else {
                    // Let the main handler exit the wizard
                    false
                }
            }
            KeyCode::Char('q') => true,
            KeyCode::Char('i') => {
                // Show help for the current resampling page section
                if self.show_help_for == Some(self.resampling_page_section) {
                    self.show_help_for = None;
                } else {
                    self.show_help_for = Some(self.resampling_page_section);
                    self.help_page = 0;
                }
                true
            }
            _ => false,
        }
    }

    #[allow(dead_code)]
    fn handle_simple_quality_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => {
                if self.quality_index > 0 {
                    self.quality_index -= 1;
                }
                true
            }
            KeyCode::Down => {
                let max_index = match self.selected_format {
                    Some(AudioFormat::Mp3) => 5,
                    Some(AudioFormat::Aac) => {
                        // 3 profiles + dynamic number of bitrates - 1 (0-based)
                        let bitrates = self.get_aac_bitrates();
                        3 + bitrates.len() - 1
                    }
                    Some(AudioFormat::Opus) => 6, // 5 qualities + 2 content types - 1
                    Some(AudioFormat::WavPack) => 6, // 6 compression options + 1 MD5 checkbox
                    _ => 0,
                };
                if self.quality_index < max_index {
                    self.quality_index += 1;
                }
                true
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if self.selected_format == Some(AudioFormat::WavPack) && self.quality_index == 6 {
                    // Toggle MD5 checkbox
                    self.store_md5 = Some(!self.store_md5.unwrap_or(true));
                } else {
                    self.select_quality();
                }
                if key.code == KeyCode::Enter {
                    self.next_step();
                }
                true
            }
            KeyCode::Esc => {
                // Let the main handler exit the wizard
                false
            }
            KeyCode::Char('q') => true,
            KeyCode::Char('i') => {
                // Show format-specific help based on the selected format and current index
                match self.selected_format {
                    Some(AudioFormat::Aac) => {
                        let profiles =
                            vec![AacProfile::LcAac, AacProfile::HeAac, AacProfile::HeAacV2];
                        if self.quality_index < profiles.len() {
                            // Show AAC profile help
                            if self.show_format_help_for == Some(FormatSpecificHelp::AacProfile) {
                                self.show_format_help_for = None;
                            } else {
                                self.show_format_help_for = Some(FormatSpecificHelp::AacProfile);
                            }
                        } else {
                            // Show AAC bitrate help
                            if self.show_format_help_for == Some(FormatSpecificHelp::AacBitrate) {
                                self.show_format_help_for = None;
                            } else {
                                self.show_format_help_for = Some(FormatSpecificHelp::AacBitrate);
                            }
                        }
                    }
                    Some(AudioFormat::Mp3) => {
                        // Show MP3 bitrate help
                        if self.show_format_help_for == Some(FormatSpecificHelp::Mp3Bitrate) {
                            self.show_format_help_for = None;
                        } else {
                            self.show_format_help_for = Some(FormatSpecificHelp::Mp3Bitrate);
                        }
                    }
                    Some(AudioFormat::Opus) => {
                        if self.quality_index < 5 {
                            // Show Opus quality help
                            if self.show_format_help_for == Some(FormatSpecificHelp::OpusQuality) {
                                self.show_format_help_for = None;
                            } else {
                                self.show_format_help_for = Some(FormatSpecificHelp::OpusQuality);
                            }
                        } else {
                            // Show Opus content type help
                            if self.show_format_help_for
                                == Some(FormatSpecificHelp::OpusContentType)
                            {
                                self.show_format_help_for = None;
                            } else {
                                self.show_format_help_for =
                                    Some(FormatSpecificHelp::OpusContentType);
                            }
                        }
                    }
                    Some(AudioFormat::WavPack) => {
                        // Show WavPack compression help
                        if self.show_format_help_for == Some(FormatSpecificHelp::WavPackCompression)
                        {
                            self.show_format_help_for = None;
                        } else {
                            self.show_format_help_for =
                                Some(FormatSpecificHelp::WavPackCompression);
                        }
                    }
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }

    fn handle_additional_options_key(&mut self, key: KeyEvent) -> bool {
        // If editing, handle text input
        if let Some(field) = self.editing_field {
            return self.handle_text_edit(key, field);
        }

        match key.code {
            KeyCode::Up => {
                if self.additional_options_index > 0 {
                    self.additional_options_index -= 1;
                    // Reset browse button focus when leaving Custom destination
                    if self.additional_options_index != 8 {
                        self.browse_button_focused = false;
                    }
                }
                true
            }
            KeyCode::Down => {
                if self.additional_options_index < 8 {
                    self.additional_options_index += 1;
                    // Reset browse button focus when moving to Custom destination
                    if self.additional_options_index == 8 {
                        self.browse_button_focused = false;
                    }
                }
                true
            }
            KeyCode::Tab => {
                // Tab navigation cycle: options → browse → back → next → cancel → options
                if let Some(nav_button) = &self.focused_nav_button {
                    // Currently on a nav button, move to the next one
                    match nav_button {
                        ButtonId::Back => self.focused_nav_button = Some(ButtonId::Next),
                        ButtonId::Next => self.focused_nav_button = Some(ButtonId::Cancel),
                        ButtonId::Cancel => {
                            // Return to options, reset to first item
                            self.focused_nav_button = None;
                            self.additional_options_index = 0;
                            self.browse_button_focused = false;
                        }
                        _ => {}
                    }
                } else if self.browse_button_focused {
                    // Currently on browse button, move to Back button
                    self.browse_button_focused = false;
                    self.focused_nav_button = Some(ButtonId::Back);
                } else if self.additional_options_index == 8
                    && matches!(self.destination_mode, DestinationMode::Custom(_))
                {
                    // On Custom field, move to browse button
                    self.browse_button_focused = true;
                } else if matches!(self.destination_mode, DestinationMode::Custom(_)) {
                    // Custom is selected but we're not on index 8, still allow tabbing to browse
                    self.browse_button_focused = true;
                } else {
                    // On any other option and Custom not selected, jump to navigation buttons
                    self.focused_nav_button = Some(ButtonId::Back);
                }
                true
            }
            KeyCode::Enter => {
                // Check if a navigation button is focused
                if let Some(nav_button) = &self.focused_nav_button {
                    match nav_button {
                        ButtonId::Back => {
                            self.previous_step();
                            self.focused_nav_button = None;
                            return true;
                        }
                        ButtonId::Next => {
                            self.next_step();
                            self.focused_nav_button = None;
                            return true;
                        }
                        ButtonId::Cancel => {
                            self.should_exit = true;
                            return true;
                        }
                        _ => {}
                    }
                }

                match self.additional_options_index {
                    4 => {
                        // Show popup for copy files
                        self.popup_state = Some(PopupState {
                            popup_type: PopupType::TextInput {
                                field: EditingField::CopyFiles,
                            },
                            input_text: self.copy_files_extensions.clone(),
                            cursor_pos: self.copy_files_extensions.len(),
                            view_offset: 0,
                            error_message: None,
                            focused_element: PopupFocus::Input,
                        });
                        true
                    }
                    5 => {
                        // Show popup for subdirectories
                        self.popup_state = Some(PopupState {
                            popup_type: PopupType::TextInput {
                                field: EditingField::CopySubdirectories,
                            },
                            input_text: self.copy_subdirectories.clone(),
                            cursor_pos: self.copy_subdirectories.len(),
                            view_offset: 0,
                            error_message: None,
                            focused_element: PopupFocus::Input,
                        });
                        true
                    }
                    8 => {
                        if self.browse_button_focused {
                            // Browse button logic would go here
                            // For now, just return true to indicate it was handled
                            true
                        } else {
                            // Show popup for custom destination
                            if let DestinationMode::Custom(ref path) = self.destination_mode {
                                self.popup_state = Some(PopupState {
                                    popup_type: PopupType::TextInput {
                                        field: EditingField::CustomDestination,
                                    },
                                    input_text: path.clone(),
                                    cursor_pos: path.len(),
                                    view_offset: 0,
                                    error_message: None,
                                    focused_element: PopupFocus::Input,
                                });
                            } else {
                                // Switch to custom mode first
                                self.destination_mode = DestinationMode::Custom(String::new());
                                self.popup_state = Some(PopupState {
                                    popup_type: PopupType::TextInput {
                                        field: EditingField::CustomDestination,
                                    },
                                    input_text: String::new(),
                                    cursor_pos: 0,
                                    view_offset: 0,
                                    error_message: None,
                                    focused_element: PopupFocus::Input,
                                });
                            }
                            true
                        }
                    }
                    _ => {
                        self.next_step();
                        true
                    }
                }
            }
            KeyCode::Char(' ') => {
                match self.additional_options_index {
                    4 => {
                        // Toggle checkbox for copy files
                        self.copy_files_enabled = !self.copy_files_enabled;
                    }
                    5 => {
                        // Toggle checkbox for copy subdirectories
                        self.copy_subdirectories_enabled = !self.copy_subdirectories_enabled;
                    }
                    _ => {
                        self.toggle_additional_option();
                    }
                }
                true
            }
            KeyCode::Esc => {
                // If help is shown, close it; otherwise let main handler exit
                if self.show_help_for.is_some() {
                    self.show_help_for = None;
                    true
                } else if self.show_additional_help_for.is_some() {
                    self.show_additional_help_for = None;
                    true
                } else {
                    // Let the main handler exit the wizard
                    false
                }
            }
            KeyCode::Char('q') => true,
            KeyCode::Char('i') => {
                // Show help for the currently focused additional option
                match self.additional_options_index {
                    0..=3 => {
                        // ReplayGain options
                        if self.show_additional_help_for == Some(AdditionalOptionsHelp::ReplayGain)
                        {
                            self.show_additional_help_for = None;
                        } else {
                            self.show_additional_help_for = Some(AdditionalOptionsHelp::ReplayGain);
                            self.help_page = 0;
                        }
                    }
                    4 => {
                        // Copy files help
                        if self.show_additional_help_for == Some(AdditionalOptionsHelp::CopyFiles) {
                            self.show_additional_help_for = None;
                        } else {
                            self.show_additional_help_for = Some(AdditionalOptionsHelp::CopyFiles);
                            self.help_page = 0;
                        }
                    }
                    5 => {
                        // Copy subdirectories help
                        if self.show_additional_help_for
                            == Some(AdditionalOptionsHelp::CopySubdirectories)
                        {
                            self.show_additional_help_for = None;
                        } else {
                            self.show_additional_help_for =
                                Some(AdditionalOptionsHelp::CopySubdirectories);
                            self.help_page = 0;
                        }
                    }
                    6 => {
                        // Merge to single help
                        if self.show_additional_help_for
                            == Some(AdditionalOptionsHelp::MergeToSingle)
                        {
                            self.show_additional_help_for = None;
                        } else {
                            self.show_additional_help_for =
                                Some(AdditionalOptionsHelp::MergeToSingle);
                            self.help_page = 0;
                        }
                    }
                    7 | 8 => {
                        // Destination help
                        if self.show_additional_help_for == Some(AdditionalOptionsHelp::SourceFiles)
                        {
                            self.show_additional_help_for = None;
                        } else {
                            self.show_additional_help_for =
                                Some(AdditionalOptionsHelp::SourceFiles);
                            self.help_page = 0;
                        }
                    }
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }

    fn handle_confirmation_key(&mut self, key: KeyEvent) -> bool {
        // Handle navigation button focus first
        if let Some(nav_button) = &self.focused_nav_button {
            match key.code {
                KeyCode::Tab => {
                    // Tab through navigation buttons
                    match nav_button {
                        ButtonId::SavePreset => self.focused_nav_button = Some(ButtonId::Back),
                        ButtonId::Back => self.focused_nav_button = Some(ButtonId::Next),
                        ButtonId::Next => self.focused_nav_button = Some(ButtonId::Cancel),
                        ButtonId::Cancel => {
                            // Return to Save Preset button
                            self.focused_nav_button = Some(ButtonId::SavePreset);
                        }
                        _ => {}
                    }
                    return true;
                }
                KeyCode::Enter => {
                    // Activate the focused button
                    match nav_button {
                        ButtonId::SavePreset => {
                            // Trigger save preset action
                            self.handle_mouse(
                                MouseEvent {
                                    kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                                    column: 0,
                                    row: 0,
                                    modifiers: crossterm::event::KeyModifiers::empty(),
                                },
                                Some(ButtonId::SavePreset),
                            );
                        }
                        ButtonId::Back => {
                            self.previous_step();
                            self.focused_nav_button = None;
                        }
                        ButtonId::Next => {
                            // Check if we need to ask for destination
                            if self.destination_mode == DestinationMode::AskEveryTime {
                                self.needs_destination_selection = true;
                                self.show_destination_browser();
                            } else {
                                self.should_start_conversion = true;
                            }
                            self.focused_nav_button = None;
                        }
                        ButtonId::Cancel => {
                            self.should_exit = true;
                            self.focused_nav_button = None;
                        }
                        _ => {}
                    }
                    return true;
                }
                _ => return false,
            }
        }

        match key.code {
            KeyCode::Tab => {
                // Start Tab navigation at Save Preset button
                self.focused_nav_button = Some(ButtonId::SavePreset);
                true
            }
            KeyCode::Enter => {
                // Check if we need to ask for destination
                if self.destination_mode == DestinationMode::AskEveryTime {
                    self.needs_destination_selection = true;
                    self.show_destination_browser();
                } else {
                    self.should_start_conversion = true;
                }
                true
            }
            KeyCode::Esc => {
                // Let the main handler exit the wizard
                false
            }
            KeyCode::Char('q') => true,
            _ => false,
        }
    }

    fn handle_popup_key(&mut self, key: KeyEvent) -> bool {
        if let Some(popup_state) = &mut self.popup_state {
            // Special handling for file browser
            if let PopupType::FileBrowser(browser) = &mut popup_state.popup_type {
                match handle_file_browser_key(browser, key) {
                    crate::types::BrowserAction::Selected(path) => {
                        // Update the destination with selected path
                        self.destination_mode =
                            DestinationMode::Custom(path.to_string_lossy().to_string());
                        self.popup_state = None;
                        // If we were selecting destination for conversion, now start conversion
                        if self.needs_destination_selection {
                            self.needs_destination_selection = false;
                            self.should_start_conversion = true;
                        }
                        return true;
                    }
                    crate::types::BrowserAction::Cancelled => {
                        self.popup_state = None;
                        // If we were selecting destination for conversion, cancel the conversion
                        if self.needs_destination_selection {
                            self.needs_destination_selection = false;
                            // Don't start conversion
                        }
                        return true;
                    }
                    crate::types::BrowserAction::Continue => {
                        // Check if we need to show new folder popup
                        if browser.show_new_folder_popup {
                            browser.show_new_folder_popup = false;
                            let parent_path = browser.current_path.clone();
                            self.popup_state = Some(PopupState {
                                popup_type: PopupType::NewFolder { parent_path },
                                input_text: String::new(),
                                cursor_pos: 0,
                                view_offset: 0,
                                error_message: None,
                                focused_element: PopupFocus::Input,
                            });
                        }
                        return true;
                    }
                }
            }

            // Special handling for new folder popup
            if let PopupType::NewFolder { parent_path } = &popup_state.popup_type {
                match key.code {
                    KeyCode::Esc => {
                        // Go back to file browser
                        let browser = crate::types::FileBrowser::new(parent_path.clone());
                        self.popup_state = Some(PopupState {
                            popup_type: PopupType::FileBrowser(Box::new(browser)),
                            input_text: String::new(),
                            cursor_pos: 0,
                            view_offset: 0,
                            error_message: None,
                            focused_element: PopupFocus::Input,
                        });
                        return true;
                    }
                    KeyCode::Enter => {
                        // Handle Enter based on what's focused
                        match popup_state.focused_element {
                            PopupFocus::CancelButton => {
                                // Cancel button is focused, go back to browser
                                let browser = crate::types::FileBrowser::new(parent_path.clone());
                                self.popup_state = Some(PopupState {
                                    popup_type: PopupType::FileBrowser(Box::new(browser)),
                                    input_text: String::new(),
                                    cursor_pos: 0,
                                    view_offset: 0,
                                    error_message: None,
                                    focused_element: PopupFocus::Input,
                                });
                                return true;
                            }
                            PopupFocus::Input | PopupFocus::OkButton => {
                                // Create the new folder
                                let folder_name = popup_state.input_text.clone();
                                if !folder_name.is_empty() {
                                    let new_path = parent_path.join(&folder_name);
                                    match std::fs::create_dir(&new_path) {
                                        Ok(_) => {
                                            // Success - open browser at parent path and select the new folder
                                            let mut browser =
                                                crate::types::FileBrowser::new(parent_path.clone());

                                            // Find and select the newly created folder
                                            for (index, entry) in browser.entries.iter().enumerate()
                                            {
                                                if entry.name == folder_name {
                                                    browser.selected_index = index;
                                                    break;
                                                }
                                            }

                                            self.popup_state = Some(PopupState {
                                                popup_type: PopupType::FileBrowser(Box::new(
                                                    browser,
                                                )),
                                                input_text: String::new(),
                                                cursor_pos: 0,
                                                view_offset: 0,
                                                error_message: None,
                                                focused_element: PopupFocus::Input,
                                            });
                                        }
                                        Err(e) => {
                                            popup_state.error_message =
                                                Some(format!("Failed to create folder: {}", e));
                                        }
                                    }
                                    return true;
                                }
                            }
                        }
                    }
                    _ => {
                        // Fall through to regular text input handling
                    }
                }
            }

            // Special handling for preset list navigation
            if let PopupType::PresetList {
                presets,
                selected_index,
            } = &mut popup_state.popup_type
            {
                match key.code {
                    KeyCode::Up => {
                        if *selected_index > 0 {
                            *selected_index -= 1;
                        }
                        return true;
                    }
                    KeyCode::Down => {
                        if *selected_index < presets.len() - 1 {
                            *selected_index += 1;
                        }
                        return true;
                    }
                    KeyCode::Esc => {
                        self.popup_state = None;
                        return true;
                    }
                    KeyCode::Enter => {
                        // Load the selected preset
                        if let Some(preset_name) = presets.get(*selected_index) {
                            match PresetManager::new() {
                                Ok(manager) => match manager.load_preset(preset_name) {
                                    Ok(preset) => {
                                        self.load_preset(&preset);
                                        self.popup_state = None;
                                    }
                                    Err(e) => {
                                        if let Ok(mut file) = OpenOptions::new()
                                            .create(true)
                                            .append(true)
                                            .open("wizard_areas.log")
                                        {
                                            let _ = writeln!(file, "Failed to load preset: {}", e);
                                        }
                                    }
                                },
                                Err(e) => {
                                    if let Ok(mut file) = OpenOptions::new()
                                        .create(true)
                                        .append(true)
                                        .open("wizard_areas.log")
                                    {
                                        let _ = writeln!(
                                            file,
                                            "Failed to create preset manager: {}",
                                            e
                                        );
                                    }
                                }
                            }
                        }
                        return true;
                    }
                    _ => {}
                }
            }

            // Regular text input handling
            match key.code {
                KeyCode::Esc => {
                    self.popup_state = None;
                    true
                }
                KeyCode::Enter => {
                    // Handle Enter based on what's focused
                    match popup_state.focused_element {
                        PopupFocus::CancelButton => {
                            // Cancel button is focused, close popup
                            self.popup_state = None;
                            return true;
                        }
                        PopupFocus::Input | PopupFocus::OkButton => {
                            // Input field or OK button focused, process normally
                        }
                    }

                    // Same as clicking OK
                    let popup_type = popup_state.popup_type.clone();
                    let input_text = popup_state.input_text.clone();

                    match popup_type {
                        PopupType::PresetName => {
                            // Check if preset already exists
                            match PresetManager::new() {
                                Ok(manager) => {
                                    if manager.preset_exists(&input_text) {
                                        // Show overwrite confirmation
                                        self.popup_state = Some(PopupState {
                                            popup_type: PopupType::OverwriteConfirm {
                                                preset_name: input_text,
                                            },
                                            input_text: String::new(),
                                            cursor_pos: 0,
                                            view_offset: 0,
                                            error_message: None,
                                            focused_element: PopupFocus::Input,
                                        });
                                        return true;
                                    }

                                    // Preset doesn't exist, save it
                                    let mut preset = ConversionPreset::from(&*self);
                                    preset.name = input_text;

                                    match manager.save_preset(&preset) {
                                        Ok(_) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ = writeln!(
                                                    file,
                                                    "Successfully saved preset: {}",
                                                    preset.name
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ =
                                                    writeln!(file, "Failed to save preset: {}", e);
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    if let Ok(mut file) = OpenOptions::new()
                                        .create(true)
                                        .append(true)
                                        .open("wizard_areas.log")
                                    {
                                        let _ = writeln!(
                                            file,
                                            "Failed to create preset manager: {}",
                                            e
                                        );
                                    }
                                }
                            }
                        }
                        PopupType::OverwriteConfirm { preset_name } => {
                            // User confirmed overwrite
                            match PresetManager::new() {
                                Ok(manager) => {
                                    let mut preset = ConversionPreset::from(&*self);
                                    preset.name = preset_name.clone();

                                    match manager.save_preset(&preset) {
                                        Ok(_) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ = writeln!(
                                                    file,
                                                    "Successfully overwrote preset: {}",
                                                    preset_name
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ = writeln!(
                                                    file,
                                                    "Failed to overwrite preset: {}",
                                                    e
                                                );
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    if let Ok(mut file) = OpenOptions::new()
                                        .create(true)
                                        .append(true)
                                        .open("wizard_areas.log")
                                    {
                                        let _ = writeln!(
                                            file,
                                            "Failed to create preset manager: {}",
                                            e
                                        );
                                    }
                                }
                            }
                        }
                        PopupType::TextInput { field } => {
                            // Apply the text input to the appropriate field
                            match field {
                                EditingField::CopyFiles => {
                                    self.copy_files_extensions = input_text;
                                }
                                EditingField::CopySubdirectories => {
                                    self.copy_subdirectories = input_text;
                                }
                                EditingField::CustomDestination => {
                                    // Validate the path
                                    let input_path = input_text.trim();
                                    if input_path.is_empty() {
                                        // Empty path is allowed (will use default)
                                        if let DestinationMode::Custom(ref mut path) =
                                            self.destination_mode
                                        {
                                            *path = String::new();
                                        } else {
                                            self.destination_mode =
                                                DestinationMode::Custom(String::new());
                                        }
                                    } else {
                                        // Check if path is valid
                                        let path = std::path::Path::new(input_path);

                                        if path.exists() && path.is_dir() {
                                            // Path exists and is a directory - valid
                                            if let DestinationMode::Custom(ref mut dest_path) =
                                                self.destination_mode
                                            {
                                                *dest_path = input_path.to_string();
                                            } else {
                                                self.destination_mode =
                                                    DestinationMode::Custom(input_path.to_string());
                                            }
                                        } else if let Some(parent) = path.parent() {
                                            // Check if parent exists (we can create the final folder)
                                            if parent.exists() && parent.is_dir() {
                                                // Additional check: verify we can actually write to the parent
                                                // For paths like /your-mom, even though / exists, we likely can't write there
                                                let test_path = parent.join(".convert_wizard_test");
                                                match std::fs::File::create(&test_path) {
                                                    Ok(_) => {
                                                        // We can write here, clean up test file
                                                        let _ = std::fs::remove_file(&test_path);

                                                        // Store the path as-is, we'll create the folder during conversion
                                                        if let DestinationMode::Custom(
                                                            ref mut dest_path,
                                                        ) = self.destination_mode
                                                        {
                                                            *dest_path = input_path.to_string();
                                                        } else {
                                                            self.destination_mode =
                                                                DestinationMode::Custom(
                                                                    input_path.to_string(),
                                                                );
                                                        }
                                                    }
                                                    Err(_) => {
                                                        // Can't write to parent directory
                                                        if let Some(ref mut popup) =
                                                            self.popup_state
                                                        {
                                                            popup.error_message = Some("Invalid Path - No write permission".to_string());
                                                        }
                                                        return true; // Don't close popup
                                                    }
                                                }
                                            } else {
                                                // Invalid path - keep popup open with error
                                                if let Some(ref mut popup) = self.popup_state {
                                                    popup.error_message =
                                                        Some("Invalid Path".to_string());
                                                }
                                                return true; // Don't close popup
                                            }
                                        } else {
                                            // Invalid path - keep popup open with error
                                            if let Some(ref mut popup) = self.popup_state {
                                                popup.error_message =
                                                    Some("Invalid Path".to_string());
                                            }
                                            return true; // Don't close popup
                                        }
                                    }
                                }
                            }
                        }
                        PopupType::PresetList {
                            presets,
                            selected_index,
                        } => {
                            // Load the selected preset
                            if let Some(preset_name) = presets.get(selected_index) {
                                match PresetManager::new() {
                                    Ok(manager) => match manager.load_preset(preset_name) {
                                        Ok(preset) => {
                                            self.load_preset(&preset);
                                        }
                                        Err(e) => {
                                            if let Ok(mut file) = OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open("wizard_areas.log")
                                            {
                                                let _ =
                                                    writeln!(file, "Failed to load preset: {}", e);
                                            }
                                        }
                                    },
                                    Err(e) => {
                                        if let Ok(mut file) = OpenOptions::new()
                                            .create(true)
                                            .append(true)
                                            .open("wizard_areas.log")
                                        {
                                            let _ = writeln!(
                                                file,
                                                "Failed to create preset manager: {}",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        PopupType::FileBrowser(_) | PopupType::NewFolder { .. } => {
                            // These are handled elsewhere
                        }
                    }

                    self.popup_state = None;
                    true
                }
                KeyCode::Char(c) => {
                    // Only allow text input for text input popups
                    match &popup_state.popup_type {
                        PopupType::PresetName
                        | PopupType::TextInput { .. }
                        | PopupType::NewFolder { .. } => {
                            popup_state.input_text.insert(popup_state.cursor_pos, c);
                            popup_state.cursor_pos += 1;
                            popup_state.error_message = None; // Clear error on typing
                            self.ensure_popup_cursor_visible();
                        }
                        _ => {}
                    }
                    true
                }
                KeyCode::Backspace => {
                    if popup_state.cursor_pos > 0 {
                        popup_state.cursor_pos -= 1;
                        popup_state.input_text.remove(popup_state.cursor_pos);
                        popup_state.error_message = None; // Clear error on editing
                        self.ensure_popup_cursor_visible();
                    }
                    true
                }
                KeyCode::Delete => {
                    if popup_state.cursor_pos < popup_state.input_text.len() {
                        popup_state.input_text.remove(popup_state.cursor_pos);
                        popup_state.error_message = None; // Clear error on editing
                    }
                    true
                }
                KeyCode::Left => {
                    if popup_state.cursor_pos > 0 {
                        popup_state.cursor_pos -= 1;
                        self.ensure_popup_cursor_visible();
                    }
                    true
                }
                KeyCode::Right => {
                    if popup_state.cursor_pos < popup_state.input_text.len() {
                        popup_state.cursor_pos += 1;
                        self.ensure_popup_cursor_visible();
                    }
                    true
                }
                KeyCode::Home => {
                    popup_state.cursor_pos = 0;
                    popup_state.view_offset = 0;
                    true
                }
                KeyCode::End => {
                    popup_state.cursor_pos = popup_state.input_text.len();
                    self.ensure_popup_cursor_visible();
                    true
                }
                KeyCode::Up => {
                    // Handle navigation for PresetList
                    if let PopupType::PresetList {
                        presets: _,
                        selected_index,
                    } = &mut popup_state.popup_type
                    {
                        if *selected_index > 0 {
                            *selected_index -= 1;
                        }
                    }
                    true
                }
                KeyCode::Down => {
                    // Handle navigation for PresetList
                    if let PopupType::PresetList {
                        presets,
                        selected_index,
                    } = &mut popup_state.popup_type
                    {
                        if *selected_index < presets.len().saturating_sub(1) {
                            *selected_index += 1;
                        }
                    }
                    true
                }
                KeyCode::Tab => {
                    // Tab through popup elements
                    match &popup_state.focused_element {
                        PopupFocus::Input => popup_state.focused_element = PopupFocus::OkButton,
                        PopupFocus::OkButton => {
                            popup_state.focused_element = PopupFocus::CancelButton
                        }
                        PopupFocus::CancelButton => popup_state.focused_element = PopupFocus::Input,
                    }
                    true
                }
                KeyCode::BackTab => {
                    // Shift+Tab to go backwards
                    match &popup_state.focused_element {
                        PopupFocus::Input => popup_state.focused_element = PopupFocus::CancelButton,
                        PopupFocus::OkButton => popup_state.focused_element = PopupFocus::Input,
                        PopupFocus::CancelButton => {
                            popup_state.focused_element = PopupFocus::OkButton
                        }
                    }
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }

    fn ensure_popup_cursor_visible(&mut self) {
        if let Some(popup_state) = &mut self.popup_state {
            // Assume a fixed width for the popup input field (adjust as needed)
            let field_width = 50;

            // Adjust view offset to keep cursor visible
            if popup_state.cursor_pos < popup_state.view_offset {
                popup_state.view_offset = popup_state.cursor_pos;
            } else if popup_state.cursor_pos >= popup_state.view_offset + field_width {
                popup_state.view_offset = popup_state.cursor_pos - field_width + 1;
            }
        }
    }

    fn select_format(&mut self) {
        let formats = vec![
            AudioFormat::Flac,
            AudioFormat::Wav,
            AudioFormat::Aiff,
            AudioFormat::WavPack,
            AudioFormat::Mp3,
            AudioFormat::Aac,
            AudioFormat::Opus,
        ];

        if self.selected_index < formats.len() {
            self.selected_format = Some(formats[self.selected_index]);
            // Reset quality area navigation when changing formats
            self.quality_index = 0;
            self.in_quality_area = false;
        }
    }

    fn select_quality(&mut self) {
        match self.selected_format {
            Some(AudioFormat::Opus) => {
                let qualities = vec!["Low", "Medium", "High", "Very High", "Insane"];
                let content_types = vec![OpusContentType::Music, OpusContentType::Voice];

                if self.quality_index < qualities.len() {
                    self.selected_quality = Some(qualities[self.quality_index].to_string());
                } else if self.quality_index < qualities.len() + content_types.len() {
                    // This is a content type selection
                    let content_idx = self.quality_index - qualities.len();
                    self.opus_content_type = Some(content_types[content_idx]);
                }
            }
            Some(AudioFormat::Aac) => {
                let profiles = vec![AacProfile::LcAac, AacProfile::HeAac, AacProfile::HeAacV2];
                let bitrates = self.get_aac_bitrates();

                if self.quality_index < profiles.len() {
                    // This is a profile selection
                    let old_profile = self.aac_profile;
                    self.aac_profile = Some(profiles[self.quality_index]);

                    // When changing profiles, ensure quality_index is still valid
                    if old_profile != self.aac_profile {
                        let new_bitrates = self.get_aac_bitrates();
                        let max_quality_index = profiles.len() + new_bitrates.len() - 1;
                        if self.quality_index > max_quality_index {
                            self.quality_index = profiles.len(); // Select first bitrate option
                        }
                    }
                } else if self.quality_index < profiles.len() + bitrates.len() {
                    // This is a bitrate selection
                    let bitrate_idx = self.quality_index - profiles.len();
                    self.selected_quality = Some(bitrates[bitrate_idx].to_string());
                }
            }
            _ => {
                let qualities = match self.selected_format {
                    Some(AudioFormat::Mp3) => vec![
                        "320 kbps",
                        "256 kbps",
                        "192 kbps",
                        "128 kbps",
                        "V0 (VBR ~245 kbps)",
                        "V2 (VBR ~190 kbps)",
                    ],
                    Some(AudioFormat::WavPack) => vec![
                        "Fast (Low CPU, larger files)",
                        "High (Balanced)",
                        "Very High (Smaller files)",
                        "Maximum (Best compression)",
                        "Ultra (Very slow)",
                        "Extreme (Slowest, smallest)",
                    ],
                    _ => vec!["Default"],
                };

                if self.quality_index < qualities.len() {
                    self.selected_quality = Some(qualities[self.quality_index].to_string());
                }
            }
        }
    }

    fn select_bit_depth(&mut self) {
        let options = Self::get_bit_depth_options();
        let index = self.selected_index;

        if index < options.len() {
            let new_bit_depth = options[index].0;
            self.bit_depth = Some(new_bit_depth);

            // Update dither type based on bit depth selection
            // Only auto-enable dither when reducing bit depth (16-bit target)
            // For upsampling or maintaining bit depth, default to None
            if new_bit_depth == 16 {
                // For 16-bit, default to Shibata (likely reducing bit depth)
                self.dither_type = Some(DitherType::Shibata);
            } else {
                // For all other cases (24-bit, 32-bit, float, same as source), default to None
                // User can explicitly enable dither in the Dithering section if desired
                self.dither_type = Some(DitherType::None);
            }
        }
    }

    fn select_dither_type(&mut self) {
        let options = self.get_dither_options();
        let index = self.selected_index;

        if index < options.len() {
            self.dither_type = Some(options[index]);
        }
    }

    fn select_sample_rate(&mut self) {
        let options = self.get_sample_rate_options_for_format();
        let index = self.selected_index;

        if index < options.len() {
            self.sample_rate = Some(options[index].0);
        }
    }

    fn select_compression_level(&mut self) {
        let options = Self::get_compression_level_options();
        if self.quality_index < options.len() {
            self.compression_level = Some(options[self.quality_index].0);
        }
    }

    fn select_resample_quality(&mut self) {
        let options = Self::get_resample_quality_options();
        let index = self.selected_index;

        if index < options.len() {
            self.resample_quality = Some(options[index].0);
        }
    }

    fn select_nyquist_transition(&mut self) {
        let options = Self::get_nyquist_transition_options();
        let index = self.selected_index;

        if index < options.len() {
            self.nyquist_transition = Some(options[index]);
        }
    }

    fn toggle_processing_option(&mut self) {
        // Processing options start after 3 compression options
        let processing_index = self.quality_index.saturating_sub(3);
        match processing_index {
            0 => self.verify_encoding = Some(!self.verify_encoding.unwrap_or(false)),
            1 => self.store_md5 = Some(!self.store_md5.unwrap_or(true)),
            2 => {
                // Only toggle if not force-checked by processing options
                if !self.is_reencode_forced() {
                    self.reencode_flac = Some(!self.reencode_flac.unwrap_or(false));
                }
                // If forced, ignore toggle (checkbox is disabled/greyed)
            }
            _ => {}
        }
    }

    fn toggle_additional_option(&mut self) {
        match self.additional_options_index {
            0 => self.replaygain_mode = Some(ReplayGainMode::Album),
            1 => self.replaygain_mode = Some(ReplayGainMode::Track),
            2 => self.replaygain_mode = Some(ReplayGainMode::Both),
            3 => self.replaygain_mode = Some(ReplayGainMode::Off),
            4 | 5 => {} // These are now editable fields
            6 => self.merge_to_single = Some(!self.merge_to_single.unwrap_or(false)),
            7 => self.destination_mode = DestinationMode::AskEveryTime,
            8 => {
                // Switch to custom mode, keeping existing path if any
                if !matches!(self.destination_mode, DestinationMode::Custom(_)) {
                    self.destination_mode = DestinationMode::Custom(String::new());
                }
            }
            _ => {}
        }
    }

    fn handle_text_edit(&mut self, _key: KeyEvent, _field: EditingField) -> bool {
        // All text fields now use popups, not inline editing
        false
    }
}

// File browser event handling
pub fn handle_file_browser_key(
    browser: &mut crate::types::FileBrowser,
    key: KeyEvent,
) -> crate::types::BrowserAction {
    use crate::types::{BrowserAction, BrowserFocus};

    match key.code {
        KeyCode::Esc => BrowserAction::Cancelled,

        KeyCode::Tab => {
            // Cycle through focus areas
            browser.focus = match browser.focus {
                BrowserFocus::List => BrowserFocus::NewButton,
                BrowserFocus::NewButton => BrowserFocus::SelectButton,
                BrowserFocus::SelectButton => BrowserFocus::CancelButton,
                BrowserFocus::CancelButton => BrowserFocus::List,
            };
            BrowserAction::Continue
        }

        KeyCode::Up => {
            if browser.focus == BrowserFocus::List && browser.selected_index > 0 {
                browser.selected_index -= 1;
            }
            BrowserAction::Continue
        }

        KeyCode::Down => {
            if browser.focus == BrowserFocus::List
                && browser.selected_index < browser.entries.len().saturating_sub(1)
            {
                browser.selected_index += 1;
            }
            BrowserAction::Continue
        }

        KeyCode::Enter => match browser.focus {
            BrowserFocus::List => {
                browser.enter_selected();
                BrowserAction::Continue
            }
            BrowserFocus::NewButton => {
                browser.show_new_folder_popup = true;
                BrowserAction::Continue
            }
            BrowserFocus::SelectButton => BrowserAction::Selected(browser.current_path.clone()),
            BrowserFocus::CancelButton => BrowserAction::Cancelled,
        },

        KeyCode::Char(' ') => {
            // Spacebar can also activate buttons
            match browser.focus {
                BrowserFocus::NewButton => {
                    browser.show_new_folder_popup = true;
                    BrowserAction::Continue
                }
                BrowserFocus::SelectButton => BrowserAction::Selected(browser.current_path.clone()),
                BrowserFocus::CancelButton => BrowserAction::Cancelled,
                _ => BrowserAction::Continue,
            }
        }

        KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Toggle hidden files
            browser.show_hidden = !browser.show_hidden;
            browser.refresh_entries();
            BrowserAction::Continue
        }

        _ => BrowserAction::Continue,
    }
}

pub fn handle_file_browser_mouse(
    browser: &mut crate::types::FileBrowser,
    mouse: MouseEvent,
    button_id: crate::ui::ButtonId,
) -> crate::types::BrowserAction {
    use crate::types::{BrowserAction, BrowserFocus};
    use crate::ui::ButtonId;
    use std::time::Instant;

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            match button_id {
                ButtonId::FileItem(idx) => {
                    // Handle double-click detection
                    let now = Instant::now();
                    if let Some((last_idx, last_time)) = browser.last_click {
                        if last_idx == idx && now.duration_since(last_time).as_millis() < 500 {
                            // Double-click detected
                            browser.selected_index = idx;
                            browser.enter_selected();
                            browser.last_click = None;
                            return BrowserAction::Continue;
                        }
                    }

                    // Single click - select item
                    browser.selected_index = idx;
                    browser.focus = BrowserFocus::List;
                    browser.last_click = Some((idx, now));
                    BrowserAction::Continue
                }
                ButtonId::NewFolder => {
                    browser.focus = BrowserFocus::NewButton;
                    browser.show_new_folder_popup = true;
                    BrowserAction::Continue
                }
                ButtonId::FileBrowserSelect => {
                    browser.focus = BrowserFocus::SelectButton;
                    BrowserAction::Selected(browser.current_path.clone())
                }
                ButtonId::FileBrowserCancel => {
                    browser.focus = BrowserFocus::CancelButton;
                    BrowserAction::Cancelled
                }
                _ => BrowserAction::Continue,
            }
        }
        _ => BrowserAction::Continue,
    }
}
