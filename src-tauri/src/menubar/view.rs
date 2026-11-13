//! What the menu bar popover shows, as plain data.
//!
//! [`build`] turns a [`Snapshot`] of app state into a [`Screen`]: a header, a
//! list of blocks and the actions each control triggers. It knows nothing
//! about AppKit, so every screen can be unit tested; `appkit.rs` only draws
//! what this returns.

use crate::managers::history::HistoryEntry;
use crate::managers::model::ModelInfo;
use crate::settings::{
    AppSettings, AutoSubmitKey, ClipboardHandling, ModelUnloadTimeout, OverlayStyle, PasteMethod,
    RecordingRetentionPeriod, ShortcutActivation, SoundTheme,
};
use std::collections::HashMap;

/// The popover's four tabs, in segmented-control order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Home,
    History,
    Models,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Home, Tab::History, Tab::Models, Tab::Settings];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::History => "History",
            Tab::Models => "Models",
            Tab::Settings => "Settings",
        }
    }

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn from_index(index: isize) -> Tab {
        usize::try_from(index)
            .ok()
            .and_then(|i| Tab::ALL.get(i).copied())
            .unwrap_or(Tab::Home)
    }
}

/// Where a dictation is, as the tray icon shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Recording,
    Transcribing,
}

/// On/off settings the popover exposes as switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoolKey {
    AudioFeedback,
    Autostart,
    TranslateToEnglish,
    AppendTrailingSpace,
    MuteWhileRecording,
    AlwaysOnMicrophone,
    AutoSubmit,
    FillerWordRemoval,
    VoiceActivityDetection,
}

impl BoolKey {
    pub fn get(self, s: &AppSettings) -> bool {
        match self {
            BoolKey::AudioFeedback => s.audio_feedback,
            BoolKey::Autostart => s.autostart_enabled,
            BoolKey::TranslateToEnglish => s.translate_to_english,
            BoolKey::AppendTrailingSpace => s.append_trailing_space,
            BoolKey::MuteWhileRecording => s.mute_while_recording,
            BoolKey::AlwaysOnMicrophone => s.always_on_microphone,
            BoolKey::AutoSubmit => s.auto_submit,
            BoolKey::FillerWordRemoval => s.filler_word_removal_enabled,
            BoolKey::VoiceActivityDetection => s.vad_enabled,
        }
    }

    /// Stable accessibility identifier suffix (also used by the UI tests).
    pub fn id(self) -> &'static str {
        match self {
            BoolKey::AudioFeedback => "audio_feedback",
            BoolKey::Autostart => "autostart",
            BoolKey::TranslateToEnglish => "translate_to_english",
            BoolKey::AppendTrailingSpace => "append_trailing_space",
            BoolKey::MuteWhileRecording => "mute_while_recording",
            BoolKey::AlwaysOnMicrophone => "always_on_microphone",
            BoolKey::AutoSubmit => "auto_submit",
            BoolKey::FillerWordRemoval => "filler_word_removal",
            BoolKey::VoiceActivityDetection => "vad",
        }
    }
}

/// Settings picked from a list, shown as pop-up buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChoiceKey {
    Microphone,
    OutputDevice,
    Language,
    Activation,
    PasteMethod,
    ClipboardHandling,
    SoundTheme,
    UnloadTimeout,
    HistoryLimit,
    RecordingRetention,
    AutoSubmitKey,
    OverlayStyle,
    OverlayPosition,
}

#[cfg(test)]
impl ChoiceKey {
    pub fn id(self) -> &'static str {
        match self {
            ChoiceKey::Microphone => "microphone",
            ChoiceKey::OutputDevice => "output_device",
            ChoiceKey::Language => "language",
            ChoiceKey::Activation => "activation",
            ChoiceKey::PasteMethod => "paste_method",
            ChoiceKey::ClipboardHandling => "clipboard_handling",
            ChoiceKey::SoundTheme => "sound_theme",
            ChoiceKey::UnloadTimeout => "unload_timeout",
            ChoiceKey::HistoryLimit => "history_limit",
            ChoiceKey::RecordingRetention => "recording_retention",
            ChoiceKey::AutoSubmitKey => "auto_submit_key",
            ChoiceKey::OverlayStyle => "overlay_style",
            ChoiceKey::OverlayPosition => "overlay_position",
        }
    }
}

/// Everything a control in the popover can ask for.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    ToggleDictation,
    CancelDictation,
    SelectTab(Tab),
    CopyEntry(i64),
    DeleteEntry(i64),
    DownloadModel(String),
    CancelDownload(String),
    SelectModel(String),
    DeleteModel(String),
    RequestMicrophone,
    RequestAccessibility,
    SetBool(BoolKey, bool),
    SetChoice(ChoiceKey, String),
    StartShortcutCapture,
    CancelShortcutCapture,
    ResetShortcut,
    OpenRecordingsFolder,
    OpenLogs,
    Quit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

fn choice(value: &str, label: &str) -> Choice {
    Choice {
        value: value.to_string(),
        label: label.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ButtonSpec {
    pub id: String,
    pub title: String,
    pub action: Action,
    pub prominent: bool,
}

fn button(id: impl Into<String>, title: &str, action: Action) -> ButtonSpec {
    ButtonSpec {
        id: id.into(),
        title: title.to_string(),
        action,
        prominent: false,
    }
}

fn prominent(id: impl Into<String>, title: &str, action: Action) -> ButtonSpec {
    ButtonSpec {
        prominent: true,
        ..button(id, title, action)
    }
}

/// The control at the trailing edge of a row.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    None,
    Switch {
        key: BoolKey,
        on: bool,
    },
    Popup {
        key: ChoiceKey,
        options: Vec<Choice>,
        selected: usize,
    },
    Buttons(Vec<ButtonSpec>),
    /// A download in flight: fraction 0..=1 and its cancel button.
    Progress {
        fraction: f64,
        cancel: ButtonSpec,
    },
    /// A green check with a short label ("Allowed", "In use").
    Done(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Accessibility identifier, `<tab>.<name>`.
    pub id: String,
    pub title: String,
    pub detail: Option<String>,
    pub control: Control,
}

impl Row {
    fn new(id: impl Into<String>, title: impl Into<String>, control: Control) -> Self {
        Row {
            id: id.into(),
            title: title.into(),
            detail: None,
            control,
        }
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// A rounded group of rows with an optional small-caps heading.
    Group {
        title: Option<String>,
        rows: Vec<Row>,
    },
    /// Secondary text: an empty state or an explanation.
    Note(String),
    /// A warning line, such as an active error notice.
    Alert(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub phase: Phase,
    pub title: String,
    pub subtitle: String,
    pub buttons: Vec<ButtonSpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    pub tab: Tab,
    pub header: Header,
    pub blocks: Vec<Block>,
    pub footer: String,
}

/// The language picker for the selected model, as the tray computes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Languages {
    /// `auto` or a language code.
    pub active: String,
    pub auto_enabled: bool,
    /// `(code, name)`: spoken languages first, then the rest by name.
    pub options: Vec<(String, String)>,
}

/// Everything [`build`] needs, gathered once per render.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    pub settings: AppSettings,
    pub models: Vec<ModelInfo>,
    /// Download progress by model id, 0..=1.
    pub progress: HashMap<String, f64>,
    pub history: Vec<HistoryEntry>,
    pub microphones: Vec<String>,
    pub outputs: Vec<String>,
    pub languages: Option<Languages>,
    pub mic_permission: bool,
    pub accessibility_permission: bool,
    pub capturing_shortcut: bool,
    /// What's held so far while capturing, e.g. `option+`.
    pub partial_shortcut: String,
    /// One-line message such as "Copied" or an error from the last action.
    pub flash: Option<String>,
    /// The headline of the active error notice, if any.
    pub notice: Option<String>,
    pub version: String,
    /// Unix seconds, for relative history times.
    pub now: i64,
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// `option+space` → `⌥ Space`, `ctrl+shift+d` → `⌃ ⇧ D`.
/// The download fractions on a screen, by row id.
pub fn progress_of(screen: &Screen) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for block in &screen.blocks {
        if let Block::Group { rows, .. } = block {
            for row in rows {
                if let Control::Progress { fraction, .. } = &row.control {
                    out.push((row.id.clone(), *fraction));
                }
            }
        }
    }
    out
}

/// True when two screens differ only in download fractions, so the views
/// can be updated in place instead of rebuilt.
pub fn same_but_progress(a: &Screen, b: &Screen) -> bool {
    fn zeroed(s: &Screen) -> Screen {
        let mut s = s.clone();
        for block in &mut s.blocks {
            if let Block::Group { rows, .. } = block {
                for row in rows {
                    if let Control::Progress { fraction, .. } = &mut row.control {
                        *fraction = 0.0;
                    }
                }
            }
        }
        s
    }
    zeroed(a) == zeroed(b)
}

pub fn pretty_shortcut(binding: &str) -> String {
    if binding.trim().is_empty() {
        return "Not set".to_string();
    }
    let parts: Vec<&str> = binding.split('+').filter(|p| !p.is_empty()).collect();
    let alone = parts.len() == 1;
    parts
        .iter()
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            // handy-keys can name the side: "ctrl_left", "command_right".
            // Say "Right" whenever it matters; say "Left" only for a lone
            // modifier, where the side is the whole shortcut.
            let (key, side) = if let Some(k) = lower.strip_suffix("_left") {
                (k.to_string(), if alone { "Left " } else { "" })
            } else if let Some(k) = lower.strip_suffix("_right") {
                (k.to_string(), "Right ")
            } else {
                (lower.clone(), "")
            };
            let symbol = match key.as_str() {
                "option" | "alt" => "⌥".to_string(),
                "command" | "cmd" | "super" | "meta" => "⌘".to_string(),
                "ctrl" | "control" => "⌃".to_string(),
                "shift" => "⇧".to_string(),
                "fn" | "function" => "fn".to_string(),
                "space" => "Space".to_string(),
                "enter" | "return" => "↩".to_string(),
                "escape" | "esc" => "⎋".to_string(),
                "tab" => "⇥".to_string(),
                "backspace" | "delete" => "⌫".to_string(),
                other => {
                    let mut c = other.chars();
                    match c.next() {
                        Some(first) => first.to_uppercase().chain(c).collect(),
                        None => String::new(),
                    }
                }
            };
            format!("{side}{symbol}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A transcript on one line, cut to `max` characters with an ellipsis.
pub fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// "Just now", "5 min ago", "3 h ago", "2 d ago".
pub fn relative_time(timestamp: i64, now: i64) -> String {
    let secs = (now - timestamp).max(0);
    match secs {
        0..=59 => "Just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// "1.2 GB" / "480 MB".
pub fn size_label(size_mb: u64) -> String {
    if size_mb >= 1000 {
        format!("{:.1} GB", size_mb as f64 / 1000.0)
    } else {
        format!("{size_mb} MB")
    }
}

fn entry_text(entry: &HistoryEntry) -> &str {
    entry
        .post_processed_text
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(&entry.transcription_text)
}

fn serde_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn popup(key: ChoiceKey, options: Vec<Choice>, current: &str) -> Control {
    let selected = options.iter().position(|o| o.value == current).unwrap_or(0);
    Control::Popup {
        key,
        options,
        selected,
    }
}

// ---------------------------------------------------------------------------
// Pieces shared by several tabs
// ---------------------------------------------------------------------------

/// The model transcription will use, if it's on disk.
pub fn active_model(s: &Snapshot) -> Option<&ModelInfo> {
    s.models
        .iter()
        .find(|m| m.id == s.settings.selected_model && m.is_downloaded)
}

/// The model the setup step offers: the recommended one, else the first.
pub fn recommended_model(s: &Snapshot) -> Option<&ModelInfo> {
    s.models
        .iter()
        .find(|m| m.is_recommended)
        .or_else(|| s.models.first())
}

/// Setup steps still to do. Empty once Handy can dictate.
pub fn setup_rows(s: &Snapshot) -> Vec<Row> {
    let mut rows = Vec::new();
    if !s.mic_permission {
        rows.push(
            Row::new(
                "setup.microphone",
                "Microphone access",
                Control::Buttons(vec![prominent(
                    "setup.microphone.allow",
                    "Allow",
                    Action::RequestMicrophone,
                )]),
            )
            .detail("Needed to hear you"),
        );
    }
    if !s.accessibility_permission {
        rows.push(
            Row::new(
                "setup.accessibility",
                "Accessibility access",
                Control::Buttons(vec![prominent(
                    "setup.accessibility.allow",
                    "Allow…",
                    Action::RequestAccessibility,
                )]),
            )
            .detail("Needed for the shortcut and pasting"),
        );
    }
    if active_model(s).is_none() {
        let downloaded = s.models.iter().find(|m| m.is_downloaded);
        let row = match (downloaded, recommended_model(s)) {
            (Some(model), _) => Row::new(
                "setup.model",
                "Speech model",
                Control::Buttons(vec![prominent(
                    "setup.model.use",
                    "Use",
                    Action::SelectModel(model.id.clone()),
                )]),
            )
            .detail(format!("{} is downloaded", model.name)),
            (None, Some(model)) => Row::new(
                "setup.model",
                "Speech model",
                model_progress_control(s, model).unwrap_or_else(|| {
                    Control::Buttons(vec![prominent(
                        "setup.model.download",
                        "Download",
                        Action::DownloadModel(model.id.clone()),
                    )])
                }),
            )
            .detail(format!("{} · {}", model.name, size_label(model.size_mb))),
            (None, None) => {
                Row::new("setup.model", "Speech model", Control::None).detail("No models available")
            }
        };
        rows.push(row);
    }
    rows
}

fn model_progress_control(s: &Snapshot, model: &ModelInfo) -> Option<Control> {
    if !model.is_downloading && !s.progress.contains_key(&model.id) {
        return None;
    }
    let fraction = s.progress.get(&model.id).copied().unwrap_or_else(|| {
        if model.size_mb > 0 {
            model.partial_size as f64 / (model.size_mb as f64 * 1_000_000.0)
        } else {
            0.0
        }
    });
    Some(Control::Progress {
        fraction: fraction.clamp(0.0, 1.0),
        cancel: button(
            format!("models.{}.cancel", model.id),
            "Cancel",
            Action::CancelDownload(model.id.clone()),
        ),
    })
}

fn transcribe_binding(s: &Snapshot) -> String {
    s.settings
        .bindings
        .get("transcribe")
        .map(|b| b.current_binding.clone())
        .unwrap_or_default()
}

fn activation_verb(activation: ShortcutActivation) -> &'static str {
    match activation {
        ShortcutActivation::Toggle => "press",
        ShortcutActivation::PushToTalk => "hold",
        ShortcutActivation::HoldOrToggle => "hold or tap",
    }
}

fn header(s: &Snapshot) -> Header {
    let shortcut = pretty_shortcut(&transcribe_binding(s));
    match s.phase {
        Phase::Recording => Header {
            phase: s.phase,
            title: "Listening…".to_string(),
            subtitle: format!("Press {shortcut} or Stop when you're done"),
            buttons: vec![
                button("header.cancel", "Cancel", Action::CancelDictation),
                prominent("header.stop", "Stop", Action::ToggleDictation),
            ],
        },
        Phase::Transcribing => Header {
            phase: s.phase,
            title: "Transcribing…".to_string(),
            subtitle: active_model(s).map(|m| m.name.clone()).unwrap_or_default(),
            buttons: vec![button("header.cancel", "Cancel", Action::CancelDictation)],
        },
        Phase::Idle => {
            let steps = setup_rows(s).len();
            if steps > 0 {
                Header {
                    phase: s.phase,
                    title: "Finish setting up".to_string(),
                    subtitle: if steps == 1 {
                        "1 step left".to_string()
                    } else {
                        format!("{steps} steps left")
                    },
                    buttons: vec![],
                }
            } else {
                let model = active_model(s).map(|m| m.name.as_str()).unwrap_or("");
                Header {
                    phase: s.phase,
                    title: "Ready".to_string(),
                    subtitle: format!(
                        "{model} · {} {shortcut}",
                        activation_verb(s.settings.shortcut_activation)
                    ),
                    buttons: vec![prominent(
                        "header.dictate",
                        "Dictate",
                        Action::ToggleDictation,
                    )],
                }
            }
        }
    }
}

fn shortcut_row(s: &Snapshot, tab: &str) -> Row {
    let id = format!("{tab}.shortcut");
    if s.capturing_shortcut {
        let held = if s.partial_shortcut.is_empty() {
            "Type a shortcut…".to_string()
        } else {
            pretty_shortcut(&s.partial_shortcut)
        };
        Row::new(
            id.clone(),
            "Shortcut",
            Control::Buttons(vec![button(
                format!("{id}.cancel"),
                "Cancel",
                Action::CancelShortcutCapture,
            )]),
        )
        .detail(held)
    } else if !s.accessibility_permission {
        // The global key listener needs Accessibility; until then the
        // shortcut can't fire or be changed, so the button leads to the fix.
        Row::new(
            id.clone(),
            "Shortcut",
            Control::Buttons(vec![button(
                format!("{id}.change"),
                &pretty_shortcut(&transcribe_binding(s)),
                Action::RequestAccessibility,
            )]),
        )
        .detail("Needs Accessibility access")
    } else {
        let binding = transcribe_binding(s);
        let is_default = s
            .settings
            .bindings
            .get("transcribe")
            .is_some_and(|b| b.current_binding == b.default_binding);
        let mut buttons = vec![button(
            format!("{id}.change"),
            &pretty_shortcut(&binding),
            Action::StartShortcutCapture,
        )];
        if !is_default {
            buttons.push(button(
                format!("{id}.reset"),
                "Reset",
                Action::ResetShortcut,
            ));
        }
        Row::new(id, "Shortcut", Control::Buttons(buttons))
    }
}

fn activation_row(s: &Snapshot, tab: &str) -> Row {
    Row::new(
        format!("{tab}.activation"),
        "Shortcut mode",
        popup(
            ChoiceKey::Activation,
            vec![
                choice("hold_or_toggle", "Hold or tap"),
                choice("push_to_talk", "Push to talk"),
                choice("toggle", "Toggle"),
            ],
            &serde_name(&s.settings.shortcut_activation),
        ),
    )
}

fn device_options(devices: &[String], current: Option<&str>) -> Vec<Choice> {
    let mut options = vec![choice("default", "System default")];
    for d in devices {
        if d != "default" && d != "Default" {
            options.push(choice(d, d));
        }
    }
    if let Some(cur) = current {
        if cur != "default" && !options.iter().any(|o| o.value == cur) {
            options.push(choice(cur, &format!("{cur} (not connected)")));
        }
    }
    options
}

fn microphone_row(s: &Snapshot, tab: &str) -> Row {
    let current = s.settings.selected_microphone.as_deref();
    Row::new(
        format!("{tab}.microphone"),
        "Microphone",
        popup(
            ChoiceKey::Microphone,
            device_options(&s.microphones, current),
            current.unwrap_or("default"),
        ),
    )
}

fn language_row(s: &Snapshot, tab: &str) -> Option<Row> {
    let langs = s.languages.as_ref()?;
    let mut options = Vec::new();
    if langs.auto_enabled {
        options.push(choice("auto", "Auto-detect"));
    }
    for (code, name) in &langs.options {
        options.push(choice(code, name));
    }
    if options.len() < 2 {
        return None;
    }
    Some(Row::new(
        format!("{tab}.language"),
        "Language",
        popup(ChoiceKey::Language, options, &langs.active),
    ))
}

fn switch_row(tab: &str, key: BoolKey, title: &str, s: &Snapshot) -> Row {
    Row::new(
        format!("{tab}.{}", key.id()),
        title,
        Control::Switch {
            key,
            on: key.get(&s.settings),
        },
    )
}

fn history_row(entry: &HistoryEntry, tab: &str, s: &Snapshot, max: usize) -> Row {
    let text = entry_text(entry);
    let title = if text.trim().is_empty() {
        "(no speech)".to_string()
    } else {
        one_line(text, max)
    };
    let mut detail = relative_time(entry.timestamp, s.now);
    if let Some(source) = &entry.source_name {
        detail = format!("{detail} · {source}");
    }
    let mut buttons = vec![button(
        format!("{tab}.entry.{}.copy", entry.id),
        "Copy",
        Action::CopyEntry(entry.id),
    )];
    if tab == "history" {
        buttons.push(button(
            format!("{tab}.entry.{}.delete", entry.id),
            "Delete",
            Action::DeleteEntry(entry.id),
        ));
    }
    Row::new(
        format!("{tab}.entry.{}", entry.id),
        title,
        Control::Buttons(buttons),
    )
    .detail(detail)
}

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

fn home(s: &Snapshot) -> Vec<Block> {
    let mut blocks = Vec::new();
    let setup = setup_rows(s);
    if !setup.is_empty() {
        blocks.push(Block::Group {
            title: Some("Set up".to_string()),
            rows: setup,
        });
    }

    let mut quick = vec![microphone_row(s, "home")];
    if let Some(row) = language_row(s, "home") {
        quick.push(row);
    }
    quick.push(shortcut_row(s, "home"));
    quick.push(activation_row(s, "home"));
    quick.push(switch_row(
        "home",
        BoolKey::AudioFeedback,
        "Sound effects",
        s,
    ));
    blocks.push(Block::Group {
        title: None,
        rows: quick,
    });

    let recent: Vec<Row> = s
        .history
        .iter()
        .take(3)
        .map(|e| history_row(e, "home", s, 70))
        .collect();
    if recent.is_empty() {
        blocks.push(Block::Note(
            "Your transcripts will appear here.".to_string(),
        ));
    } else {
        blocks.push(Block::Group {
            title: Some("Latest".to_string()),
            rows: recent,
        });
    }
    blocks
}

fn history(s: &Snapshot) -> Vec<Block> {
    if s.history.is_empty() {
        return vec![Block::Note(
            "No transcripts yet. Press your shortcut and start talking.".to_string(),
        )];
    }
    vec![
        Block::Group {
            title: Some(format!("{} recent", s.history.len())),
            rows: s
                .history
                .iter()
                .map(|e| history_row(e, "history", s, 90))
                .collect(),
        },
        Block::Group {
            title: None,
            rows: vec![Row::new(
                "history.open_folder",
                "Recordings",
                Control::Buttons(vec![button(
                    "history.open_folder.button",
                    "Show in Finder",
                    Action::OpenRecordingsFolder,
                )]),
            )],
        },
    ]
}

fn model_row(s: &Snapshot, m: &ModelInfo) -> Row {
    let id = format!("models.{}", m.id);
    let mut detail = vec![size_label(m.size_mb)];
    if m.is_recommended {
        detail.push("Recommended".to_string());
    }
    detail.push(format!(
        "accuracy {:.0}% · speed {:.0}%",
        m.accuracy_score * 100.0,
        m.speed_score * 100.0
    ));
    let control = if let Some(progress) = model_progress_control(s, m) {
        progress
    } else if m.is_downloaded && m.id == s.settings.selected_model {
        Control::Done("In use".to_string())
    } else if m.is_downloaded {
        Control::Buttons(vec![
            button(
                format!("{id}.delete"),
                "Delete",
                Action::DeleteModel(m.id.clone()),
            ),
            prominent(
                format!("{id}.use"),
                "Use",
                Action::SelectModel(m.id.clone()),
            ),
        ])
    } else {
        Control::Buttons(vec![button(
            format!("{id}.download"),
            "Download",
            Action::DownloadModel(m.id.clone()),
        )])
    };
    Row::new(id, m.name.clone(), control).detail(detail.join(" · "))
}

fn models(s: &Snapshot) -> Vec<Block> {
    let mut installed: Vec<&ModelInfo> = s.models.iter().filter(|m| m.is_downloaded).collect();
    let mut available: Vec<&ModelInfo> = s.models.iter().filter(|m| !m.is_downloaded).collect();
    installed.sort_by(|a, b| a.name.cmp(&b.name));
    available.sort_by(|a, b| {
        b.is_recommended
            .cmp(&a.is_recommended)
            .then(a.size_mb.cmp(&b.size_mb))
    });
    let mut blocks = Vec::new();
    if installed.is_empty() {
        blocks.push(Block::Note(
            "No models downloaded yet. Pick one below.".to_string(),
        ));
    } else {
        blocks.push(Block::Group {
            title: Some("Installed".to_string()),
            rows: installed.into_iter().map(|m| model_row(s, m)).collect(),
        });
    }
    if !available.is_empty() {
        blocks.push(Block::Group {
            title: Some("Available".to_string()),
            rows: available.into_iter().map(|m| model_row(s, m)).collect(),
        });
    }
    blocks
}

fn settings(s: &Snapshot) -> Vec<Block> {
    let st = &s.settings;
    let mut dictation = vec![
        shortcut_row(s, "settings"),
        activation_row(s, "settings"),
        Row::new(
            "settings.overlay_style",
            "Recording indicator",
            popup(
                ChoiceKey::OverlayStyle,
                vec![
                    choice("live", "Levels + live text"),
                    choice("minimal", "Levels only"),
                    choice("none", "Off"),
                ],
                &serde_name(&st.overlay_style),
            ),
        ),
    ];
    if st.overlay_style != OverlayStyle::None {
        dictation.push(Row::new(
            "settings.overlay_position",
            "Indicator position",
            popup(
                ChoiceKey::OverlayPosition,
                vec![choice("bottom", "Bottom"), choice("top", "Top")],
                &serde_name(&st.overlay_position),
            ),
        ));
    }
    if let Some(row) = language_row(s, "settings") {
        dictation.push(row);
    }
    if active_model(s).is_some_and(|m| m.supports_translation) {
        dictation.push(switch_row(
            "settings",
            BoolKey::TranslateToEnglish,
            "Translate to English",
            s,
        ));
    }

    let output_current = st.selected_output_device.as_deref();
    let mut sound = vec![
        microphone_row(s, "settings"),
        switch_row("settings", BoolKey::AudioFeedback, "Sound effects", s),
    ];
    if st.audio_feedback {
        sound.push(Row::new(
            "settings.sound_theme",
            "Sound",
            popup(
                ChoiceKey::SoundTheme,
                vec![choice("marimba", "Marimba"), choice("pop", "Pop")]
                    .into_iter()
                    .chain(
                        (st.sound_theme == SoundTheme::Custom).then(|| choice("custom", "Custom")),
                    )
                    .collect(),
                &serde_name(&st.sound_theme),
            ),
        ));
        sound.push(Row::new(
            "settings.output_device",
            "Play sounds on",
            popup(
                ChoiceKey::OutputDevice,
                device_options(&s.outputs, output_current),
                output_current.unwrap_or("default"),
            ),
        ));
    }
    sound.push(switch_row(
        "settings",
        BoolKey::MuteWhileRecording,
        "Mute other audio while recording",
        s,
    ));
    sound.push(switch_row(
        "settings",
        BoolKey::AlwaysOnMicrophone,
        "Keep microphone open",
        s,
    ));
    sound.push(switch_row(
        "settings",
        BoolKey::VoiceActivityDetection,
        "Trim silence",
        s,
    ));

    let mut output = vec![
        Row::new(
            "settings.paste_method",
            "Paste with",
            popup(
                ChoiceKey::PasteMethod,
                vec![
                    choice("ctrl_v", "⌘V (clipboard)"),
                    choice("direct", "Typing"),
                    choice("none", "Don't paste"),
                ]
                .into_iter()
                .chain(
                    (!matches!(
                        st.paste_method,
                        PasteMethod::CtrlV | PasteMethod::Direct | PasteMethod::None
                    ))
                    .then(|| choice(&serde_name(&st.paste_method), "Custom")),
                )
                .collect(),
                &serde_name(&st.paste_method),
            ),
        ),
        Row::new(
            "settings.clipboard_handling",
            "Clipboard",
            popup(
                ChoiceKey::ClipboardHandling,
                vec![
                    choice(&serde_name(&ClipboardHandling::DontModify), "Restore it"),
                    choice(
                        &serde_name(&ClipboardHandling::CopyToClipboard),
                        "Keep the transcript",
                    ),
                ],
                &serde_name(&st.clipboard_handling),
            ),
        ),
        switch_row(
            "settings",
            BoolKey::AppendTrailingSpace,
            "Add a space after",
            s,
        ),
        switch_row(
            "settings",
            BoolKey::FillerWordRemoval,
            "Remove filler words",
            s,
        ),
        switch_row(
            "settings",
            BoolKey::AutoSubmit,
            "Press a key after pasting",
            s,
        ),
    ];
    if st.auto_submit {
        output.push(Row::new(
            "settings.auto_submit_key",
            "Key",
            popup(
                ChoiceKey::AutoSubmitKey,
                vec![
                    choice(&serde_name(&AutoSubmitKey::Enter), "↩ Return"),
                    choice(&serde_name(&AutoSubmitKey::CtrlEnter), "⌃↩"),
                    choice(&serde_name(&AutoSubmitKey::CmdEnter), "⌘↩"),
                ],
                &serde_name(&st.auto_submit_key),
            ),
        ));
    }

    let system = vec![
        switch_row("settings", BoolKey::Autostart, "Open at login", s),
        Row::new(
            "settings.unload_timeout",
            "Free model memory",
            popup(
                ChoiceKey::UnloadTimeout,
                vec![
                    choice(&serde_name(&ModelUnloadTimeout::Immediately), "Right away"),
                    choice(&serde_name(&ModelUnloadTimeout::Min2), "After 2 min"),
                    choice(&serde_name(&ModelUnloadTimeout::Min5), "After 5 min"),
                    choice(&serde_name(&ModelUnloadTimeout::Min15), "After 15 min"),
                    choice(&serde_name(&ModelUnloadTimeout::Hour1), "After 1 hour"),
                    choice(&serde_name(&ModelUnloadTimeout::Never), "Never"),
                ],
                &serde_name(&st.model_unload_timeout),
            ),
        ),
        Row::new(
            "settings.history_limit",
            "Keep transcripts",
            popup(
                ChoiceKey::HistoryLimit,
                [5usize, 10, 25, 50, 100, 500]
                    .iter()
                    .map(|n| choice(&n.to_string(), &format!("Last {n}")))
                    .chain(
                        (![5usize, 10, 25, 50, 100, 500].contains(&st.history_limit)).then(|| {
                            choice(
                                &st.history_limit.to_string(),
                                &format!("Last {}", st.history_limit),
                            )
                        }),
                    )
                    .collect(),
                &st.history_limit.to_string(),
            ),
        ),
        Row::new(
            "settings.recording_retention",
            "Keep recordings",
            popup(
                ChoiceKey::RecordingRetention,
                vec![
                    choice(&serde_name(&RecordingRetentionPeriod::Never), "Don't keep"),
                    choice(
                        &serde_name(&RecordingRetentionPeriod::PreserveLimit),
                        "With transcripts",
                    ),
                    choice(&serde_name(&RecordingRetentionPeriod::Days3), "3 days"),
                    choice(&serde_name(&RecordingRetentionPeriod::Weeks2), "2 weeks"),
                    choice(&serde_name(&RecordingRetentionPeriod::Months3), "3 months"),
                ],
                &serde_name(&st.recording_retention_period),
            ),
        ),
        Row::new(
            "settings.logs",
            "Logs",
            Control::Buttons(vec![button(
                "settings.logs.open",
                "Show in Finder",
                Action::OpenLogs,
            )]),
        ),
    ];

    vec![
        Block::Group {
            title: Some("Dictation".to_string()),
            rows: dictation,
        },
        Block::Group {
            title: Some("Sound".to_string()),
            rows: sound,
        },
        Block::Group {
            title: Some("Output".to_string()),
            rows: output,
        },
        Block::Group {
            title: Some("System".to_string()),
            rows: system,
        },
    ]
}

/// The whole popover for `tab`.
pub fn build(s: &Snapshot, tab: Tab) -> Screen {
    let mut blocks = Vec::new();
    if let Some(notice) = &s.notice {
        blocks.push(Block::Alert(notice.clone()));
    }
    if let Some(flash) = &s.flash {
        blocks.push(Block::Note(flash.clone()));
    }
    blocks.extend(match tab {
        Tab::Home => home(s),
        Tab::History => history(s),
        Tab::Models => models(s),
        Tab::Settings => settings(s),
    });
    Screen {
        tab,
        header: header(s),
        blocks,
        footer: format!("Handy Bar {} · unofficial fork of Handy", s.version),
    }
}

/// Every action reachable from a screen.
#[cfg(test)]
pub fn actions(screen: &Screen) -> Vec<Action> {
    let mut out: Vec<Action> = screen
        .header
        .buttons
        .iter()
        .map(|b| b.action.clone())
        .collect();
    for block in &screen.blocks {
        if let Block::Group { rows, .. } = block {
            for row in rows {
                match &row.control {
                    Control::Buttons(bs) => out.extend(bs.iter().map(|b| b.action.clone())),
                    Control::Progress { cancel, .. } => out.push(cancel.action.clone()),
                    Control::Switch { key, on } => out.push(Action::SetBool(*key, !on)),
                    Control::Popup { key, options, .. } => out.extend(
                        options
                            .iter()
                            .map(|o| Action::SetChoice(*key, o.value.clone())),
                    ),
                    Control::Done(_) | Control::None => {}
                }
            }
        }
    }
    out
}

/// All rows of a screen, flattened.
#[cfg(test)]
pub fn rows(screen: &Screen) -> Vec<&Row> {
    screen
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Group { rows, .. } => Some(rows.iter()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::model::ModelSource;
    use crate::settings::get_default_settings;

    fn model(id: &str, downloaded: bool, recommended: bool) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            name: format!("Model {id}"),
            description: String::new(),
            filename: format!("{id}.gguf"),
            source: ModelSource::Url {
                url: String::new(),
                sha256: None,
            },
            size_mb: 480,
            is_downloaded: downloaded,
            is_downloading: false,
            partial_size: 0,
            accuracy_score: 0.8,
            speed_score: 0.9,
            supports_translation: true,
            is_recommended: recommended,
            supported_languages: vec!["en".into(), "de".into()],
            supports_language_selection: true,
            is_custom: false,
            supports_streaming: false,
            supports_language_detection: true,
        }
    }

    fn entry(id: i64, text: &str, ts: i64) -> HistoryEntry {
        HistoryEntry {
            id,
            file_name: format!("{id}.wav"),
            timestamp: ts,
            saved: false,
            title: String::new(),
            transcription_text: text.to_string(),
            post_processed_text: None,
            post_process_prompt: None,
            post_process_requested: false,
            source_name: None,
            language: None,
            language_source: None,
        }
    }

    /// A fully set-up app: permissions granted, model `a` downloaded and selected.
    fn ready() -> Snapshot {
        let mut settings = get_default_settings();
        settings.selected_model = "a".into();
        Snapshot {
            phase: Phase::Idle,
            settings,
            models: vec![model("a", true, false), model("b", false, true)],
            progress: HashMap::new(),
            history: vec![],
            microphones: vec!["MacBook Air Microphone".into()],
            outputs: vec!["MacBook Air Speakers".into()],
            languages: Some(Languages {
                active: "auto".into(),
                auto_enabled: true,
                options: vec![
                    ("en".into(), "English".into()),
                    ("de".into(), "German".into()),
                ],
            }),
            mic_permission: true,
            accessibility_permission: true,
            capturing_shortcut: false,
            partial_shortcut: String::new(),
            flash: None,
            notice: None,
            version: "0.10.0".into(),
            now: 1_000_000,
        }
    }

    fn row<'a>(screen: &'a Screen, id: &str) -> &'a Row {
        rows(screen)
            .into_iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("no row {id}"))
    }

    fn has_row(screen: &Screen, id: &str) -> bool {
        rows(screen).iter().any(|r| r.id == id)
    }

    #[test]
    fn tabs_round_trip_through_their_index() {
        for tab in Tab::ALL {
            assert_eq!(Tab::from_index(tab.index() as isize), tab);
        }
        assert_eq!(Tab::from_index(-1), Tab::Home);
        assert_eq!(Tab::from_index(99), Tab::Home);
    }

    #[test]
    fn shortcuts_render_as_mac_symbols() {
        assert_eq!(pretty_shortcut("option+space"), "⌥ Space");
        assert_eq!(pretty_shortcut("ctrl+shift+d"), "⌃ ⇧ D");
        assert_eq!(pretty_shortcut("command+f13"), "⌘ F13");
        assert_eq!(pretty_shortcut("fn"), "fn");
        assert_eq!(pretty_shortcut("ctrl_left+shift_left+k"), "⌃ ⇧ K");
        assert_eq!(pretty_shortcut("command_right"), "Right ⌘");
        assert_eq!(pretty_shortcut("option_left"), "Left ⌥");
        assert_eq!(pretty_shortcut("ctrl_right+k"), "Right ⌃ K");
        assert_eq!(pretty_shortcut("option_right+space"), "Right ⌥ Space");
        assert_eq!(pretty_shortcut(""), "Not set");
    }

    #[test]
    fn transcripts_are_flattened_and_truncated() {
        assert_eq!(one_line("a\n b   c", 10), "a b c");
        assert_eq!(one_line("abcdefghijkl", 6), "abcde…");
        assert_eq!(one_line("ab", 2), "ab");
    }

    #[test]
    fn relative_times_and_sizes() {
        assert_eq!(relative_time(100, 110), "Just now");
        assert_eq!(relative_time(0, 300), "5 min ago");
        assert_eq!(relative_time(0, 7200), "2 h ago");
        assert_eq!(relative_time(0, 3 * 86_400), "3 d ago");
        assert_eq!(relative_time(500, 100), "Just now");
        assert_eq!(size_label(480), "480 MB");
        assert_eq!(size_label(1600), "1.6 GB");
    }

    #[test]
    fn ready_header_offers_dictation_with_the_shortcut() {
        let screen = build(&ready(), Tab::Home);
        assert_eq!(screen.header.title, "Ready");
        assert!(screen.header.subtitle.contains("Model a"));
        assert!(screen.header.subtitle.contains("⌥ Space"));
        assert_eq!(screen.header.buttons.len(), 1);
        assert_eq!(screen.header.buttons[0].action, Action::ToggleDictation);
        assert_eq!(screen.footer, "Handy Bar 0.10.0 · unofficial fork of Handy");
    }

    #[test]
    fn recording_header_offers_stop_and_cancel() {
        let mut s = ready();
        s.phase = Phase::Recording;
        let h = build(&s, Tab::Home).header;
        assert_eq!(h.title, "Listening…");
        let actions: Vec<_> = h.buttons.iter().map(|b| b.action.clone()).collect();
        assert_eq!(
            actions,
            vec![Action::CancelDictation, Action::ToggleDictation]
        );
        assert!(h.buttons[1].prominent);
    }

    #[test]
    fn transcribing_header_offers_cancel_only() {
        let mut s = ready();
        s.phase = Phase::Transcribing;
        let h = build(&s, Tab::Home).header;
        assert_eq!(h.title, "Transcribing…");
        assert_eq!(h.buttons.len(), 1);
        assert_eq!(h.buttons[0].action, Action::CancelDictation);
    }

    #[test]
    fn a_fresh_install_shows_three_setup_steps() {
        let mut s = ready();
        s.mic_permission = false;
        s.accessibility_permission = false;
        s.settings.selected_model = String::new();
        s.models = vec![model("a", false, false), model("b", false, true)];
        let screen = build(&s, Tab::Home);
        assert_eq!(screen.header.title, "Finish setting up");
        assert_eq!(screen.header.subtitle, "3 steps left");
        assert!(screen.header.buttons.is_empty());
        assert_eq!(
            row(&screen, "setup.microphone").control,
            Control::Buttons(vec![prominent(
                "setup.microphone.allow",
                "Allow",
                Action::RequestMicrophone
            )])
        );
        assert!(has_row(&screen, "setup.accessibility"));
        // The recommended model is the one offered.
        let model_row = row(&screen, "setup.model");
        assert!(actions(&screen).contains(&Action::DownloadModel("b".into())));
        assert!(model_row.detail.as_deref().unwrap().contains("Model b"));
    }

    #[test]
    fn setup_offers_a_downloaded_model_before_downloading_another() {
        let mut s = ready();
        s.settings.selected_model = "missing".into();
        let screen = build(&s, Tab::Home);
        assert!(actions(&screen).contains(&Action::SelectModel("a".into())));
        assert_eq!(screen.header.subtitle, "1 step left");
    }

    #[test]
    fn setup_shows_download_progress() {
        let mut s = ready();
        s.settings.selected_model = String::new();
        s.models = vec![model("b", false, true)];
        s.progress.insert("b".into(), 0.42);
        let screen = build(&s, Tab::Home);
        match &row(&screen, "setup.model").control {
            Control::Progress { fraction, cancel } => {
                assert!((fraction - 0.42).abs() < 1e-9);
                assert_eq!(cancel.action, Action::CancelDownload("b".into()));
            }
            other => panic!("expected progress, got {other:?}"),
        }
    }

    #[test]
    fn setup_disappears_once_done() {
        let screen = build(&ready(), Tab::Home);
        assert!(!has_row(&screen, "setup.microphone"));
        assert!(!has_row(&screen, "setup.model"));
        assert!(!screen
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Group { title: Some(t), .. } if t == "Set up")));
    }

    #[test]
    fn home_has_quick_controls() {
        let screen = build(&ready(), Tab::Home);
        for id in [
            "home.microphone",
            "home.language",
            "home.shortcut",
            "home.activation",
            "home.audio_feedback",
        ] {
            assert!(has_row(&screen, id), "missing {id}");
        }
        assert!(screen
            .blocks
            .contains(&Block::Note("Your transcripts will appear here.".into())));
    }

    #[test]
    fn home_lists_the_latest_three_transcripts() {
        let mut s = ready();
        s.history = (1..=5)
            .rev()
            .map(|i| entry(i, &format!("note {i}"), 1_000_000 - i * 60))
            .collect();
        let screen = build(&s, Tab::Home);
        let latest: Vec<_> = rows(&screen)
            .into_iter()
            .filter(|r| r.id.starts_with("home.entry."))
            .map(|r| r.title.clone())
            .collect();
        assert_eq!(latest, vec!["note 5", "note 4", "note 3"]);
        assert!(actions(&screen).contains(&Action::CopyEntry(5)));
        // Delete lives on the History tab only.
        assert!(!actions(&screen).contains(&Action::DeleteEntry(5)));
    }

    #[test]
    fn history_prefers_post_processed_text_and_offers_delete() {
        let mut s = ready();
        let mut e = entry(7, "raw words", 1_000_000 - 120);
        e.post_processed_text = Some("Polished words.".into());
        s.history = vec![e, entry(6, "", 1_000_000)];
        let screen = build(&s, Tab::History);
        let r = row(&screen, "history.entry.7");
        assert_eq!(r.title, "Polished words.");
        assert_eq!(r.detail.as_deref(), Some("2 min ago"));
        assert_eq!(row(&screen, "history.entry.6").title, "(no speech)");
        let acts = actions(&screen);
        assert!(acts.contains(&Action::DeleteEntry(7)));
        assert!(acts.contains(&Action::OpenRecordingsFolder));
    }

    #[test]
    fn empty_history_explains_itself() {
        let screen = build(&ready(), Tab::History);
        assert!(matches!(&screen.blocks[0], Block::Note(t) if t.starts_with("No transcripts")));
    }

    #[test]
    fn models_split_installed_and_available() {
        let mut s = ready();
        s.models.push(model("c", true, false));
        let screen = build(&s, Tab::Models);
        assert_eq!(
            row(&screen, "models.a").control,
            Control::Done("In use".into())
        );
        let acts = actions(&screen);
        assert!(acts.contains(&Action::SelectModel("c".into())));
        assert!(acts.contains(&Action::DeleteModel("c".into())));
        assert!(acts.contains(&Action::DownloadModel("b".into())));
        // The model in use can't be deleted from here.
        assert!(!acts.contains(&Action::DeleteModel("a".into())));
        let titles: Vec<_> = screen
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Group { title, .. } => title.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(titles, vec!["Installed", "Available"]);
    }

    #[test]
    fn a_downloading_model_shows_progress() {
        let mut s = ready();
        s.models[1].is_downloading = true;
        s.models[1].partial_size = 240_000_000;
        let screen = build(&s, Tab::Models);
        match &row(&screen, "models.b").control {
            Control::Progress { fraction, .. } => assert!((fraction - 0.5).abs() < 1e-9),
            other => panic!("expected progress, got {other:?}"),
        }
    }

    #[test]
    fn settings_switches_reflect_and_flip_their_values() {
        let mut s = ready();
        s.settings.audio_feedback = true;
        s.settings.autostart_enabled = false;
        let screen = build(&s, Tab::Settings);
        assert_eq!(
            row(&screen, "settings.audio_feedback").control,
            Control::Switch {
                key: BoolKey::AudioFeedback,
                on: true
            }
        );
        let acts = actions(&screen);
        assert!(acts.contains(&Action::SetBool(BoolKey::AudioFeedback, false)));
        assert!(acts.contains(&Action::SetBool(BoolKey::Autostart, true)));
        // Sound options only show with sound effects on.
        assert!(has_row(&screen, "settings.sound_theme"));
        s.settings.audio_feedback = false;
        assert!(!has_row(&build(&s, Tab::Settings), "settings.sound_theme"));
    }

    #[test]
    fn settings_popups_select_the_stored_value() {
        let mut s = ready();
        s.settings.paste_method = PasteMethod::Direct;
        s.settings.history_limit = 7;
        s.settings.shortcut_activation = ShortcutActivation::Toggle;
        let screen = build(&s, Tab::Settings);
        let selected = |id: &str| match &row(&screen, id).control {
            Control::Popup {
                options, selected, ..
            } => options[*selected].value.clone(),
            other => panic!("{id} is not a popup: {other:?}"),
        };
        assert_eq!(selected("settings.paste_method"), "direct");
        assert_eq!(selected("settings.history_limit"), "7");
        assert_eq!(selected("settings.activation"), "toggle");
        assert_eq!(selected("settings.language"), "auto");
        assert_eq!(selected("settings.microphone"), "default");
    }

    #[test]
    fn every_bool_key_has_a_unique_id_and_a_settings_row() {
        let mut s = ready();
        s.settings.audio_feedback = true;
        s.settings.auto_submit = true;
        let screen = build(&s, Tab::Settings);
        let keys = [
            BoolKey::AudioFeedback,
            BoolKey::Autostart,
            BoolKey::TranslateToEnglish,
            BoolKey::AppendTrailingSpace,
            BoolKey::MuteWhileRecording,
            BoolKey::AlwaysOnMicrophone,
            BoolKey::AutoSubmit,
            BoolKey::FillerWordRemoval,
            BoolKey::VoiceActivityDetection,
        ];
        let mut ids: Vec<_> = keys.iter().map(|k| k.id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), keys.len());
        for key in keys {
            assert!(
                has_row(&screen, &format!("settings.{}", key.id())),
                "no switch for {key:?}"
            );
        }
        assert!(has_row(&screen, "settings.auto_submit_key"));
    }

    #[test]
    fn progress_changes_alone_are_detected() {
        let mut a = ready();
        a.progress.insert("a".into(), 0.2);
        a.progress.insert("b".into(), 0.2);
        let mut b = a.clone();
        b.progress.insert("b".into(), 0.7);
        let (sa, sb) = (build(&a, Tab::Models), build(&b, Tab::Models));
        assert!(!progress_of(&sa).is_empty());
        assert_ne!(sa, sb);
        assert!(same_but_progress(&sa, &sb));
        assert!(!same_but_progress(&sa, &build(&a, Tab::Settings)));
    }

    #[test]
    fn a_disconnected_microphone_stays_selectable() {
        let mut s = ready();
        s.settings.selected_microphone = Some("USB Mic".into());
        let screen = build(&s, Tab::Settings);
        match &row(&screen, "settings.microphone").control {
            Control::Popup {
                options, selected, ..
            } => {
                assert_eq!(options[*selected].label, "USB Mic (not connected)");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn capturing_a_shortcut_shows_what_is_held() {
        let mut s = ready();
        s.capturing_shortcut = true;
        s.partial_shortcut = "command+shift".into();
        let screen = build(&s, Tab::Settings);
        let r = row(&screen, "settings.shortcut");
        assert_eq!(r.detail.as_deref(), Some("⌘ ⇧"));
        assert!(actions(&screen).contains(&Action::CancelShortcutCapture));
    }

    #[test]
    fn the_shortcut_asks_for_accessibility_first() {
        let mut s = ready();
        s.accessibility_permission = false;
        let screen = build(&s, Tab::Home);
        let r = row(&screen, "home.shortcut");
        assert_eq!(r.detail.as_deref(), Some("Needs Accessibility access"));
        let acts = actions(&screen);
        assert!(acts.contains(&Action::RequestAccessibility));
        assert!(!acts.contains(&Action::StartShortcutCapture));
    }

    #[test]
    fn the_recording_indicator_can_be_styled_placed_or_turned_off() {
        let mut s = ready();
        s.settings.overlay_style = OverlayStyle::Live;
        let screen = build(&s, Tab::Settings);
        assert!(
            actions(&screen).contains(&Action::SetChoice(ChoiceKey::OverlayStyle, "none".into()))
        );
        assert!(
            actions(&screen).contains(&Action::SetChoice(ChoiceKey::OverlayPosition, "top".into()))
        );
        s.settings.overlay_style = OverlayStyle::None;
        let screen = build(&s, Tab::Settings);
        assert!(!rows(&screen)
            .iter()
            .any(|r| r.id == "settings.overlay_position"));
    }

    #[test]
    fn a_custom_shortcut_can_be_reset() {
        let mut s = ready();
        assert!(!actions(&build(&s, Tab::Settings)).contains(&Action::ResetShortcut));
        s.settings
            .bindings
            .get_mut("transcribe")
            .unwrap()
            .current_binding = "f13".into();
        let screen = build(&s, Tab::Settings);
        assert!(actions(&screen).contains(&Action::ResetShortcut));
        assert!(screen.header.subtitle.contains("F13"));
    }

    #[test]
    fn notices_and_flash_messages_lead_the_screen() {
        let mut s = ready();
        s.notice = Some("Transcription Failed".into());
        s.flash = Some("Copied".into());
        let screen = build(&s, Tab::Models);
        assert_eq!(
            screen.blocks[0],
            Block::Alert("Transcription Failed".into())
        );
        assert_eq!(screen.blocks[1], Block::Note("Copied".into()));
    }

    #[test]
    fn row_ids_are_unique_on_every_tab() {
        let mut s = ready();
        s.history = vec![entry(1, "a", 0), entry(2, "b", 0)];
        s.settings.audio_feedback = true;
        for tab in Tab::ALL {
            let screen = build(&s, tab);
            let mut ids: Vec<_> = rows(&screen).iter().map(|r| r.id.clone()).collect();
            let n = ids.len();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), n, "duplicate row id on {tab:?}");
        }
    }
}
