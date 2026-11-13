//! Native menu bar UI for macOS.
//!
//! Handy normally keeps a WebView settings window around. With the menu bar
//! UI (the default on macOS, see [`enabled`]) there is no WebView at all:
//! left-clicking the menu bar icon opens an AppKit popover drawn from Rust,
//! and right-clicking keeps the upstream quick menu from `tray.rs`.
//!
//! Layout of this module:
//! - [`view`]: pure data. App state in, a description of the screen out.
//! - [`capture`]: pure shortcut-capture state machine.
//! - this file: gathers state, runs actions, listens for backend events.
//! - `appkit`: draws a [`view::Screen`] with AppKit and owns the popover.
//!
//! The rest of Handy only calls [`enabled`], [`install`], [`on_tray_click`],
//! [`open`] and [`request_refresh`], which keeps the diff against upstream
//! small.

mod appkit;
pub mod capture;
pub mod hud;
pub mod view;

use crate::managers::history::{HistoryManager, HistoryUpdatePayload};
use crate::managers::model::{DownloadProgress, ModelManager};
use crate::settings;
use crate::tray::{self, TrayIconState};
use capture::{Capture, KeyEvent, Step};
use log::{error, info, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Listener, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_specta::Event;
use view::{Action, BoolKey, ChoiceKey, Languages, Phase, Snapshot, Tab};

/// Set `HANDY_WEBVIEW_UI=1` to get the upstream WebView window back.
pub fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        !matches!(
            std::env::var("HANDY_WEBVIEW_UI").as_deref(),
            Ok("1") | Ok("true")
        )
    })
}

const HISTORY_SHOWN: usize = 30;
const FLASH_FOR: Duration = Duration::from_secs(4);

#[derive(Default)]
struct Inner {
    tab: Option<Tab>,
    progress: HashMap<String, f64>,
    capture: Option<Capture>,
    /// The binding `capture` is recording for.
    capture_binding: String,
    partial: String,
    flash: Option<(String, Instant)>,
    microphones: Vec<String>,
    outputs: Vec<String>,
    last_progress_redraw: Option<Instant>,
    /// Models tab view options (not settings: they reset each launch).
    model_filter: Option<String>,
    model_sort: Option<String>,
    /// A Delete clicked once, and when; a second click within
    /// `CONFIRM_FOR` deletes.
    confirm: Option<(String, Instant)>,
}

const CONFIRM_FOR: Duration = Duration::from_secs(4);

/// Tauri-managed state shared between the main thread and event listeners.
#[derive(Default)]
pub struct MenubarState {
    inner: Mutex<Inner>,
    refresh_pending: AtomicBool,
}

impl MenubarState {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn state(app: &AppHandle) -> Option<tauri::State<'_, MenubarState>> {
    app.try_state::<MenubarState>()
}

fn flash(app: &AppHandle, message: impl Into<String>) {
    if let Some(s) = state(app) {
        s.lock().flash = Some((message.into(), Instant::now()));
    }
    request_refresh(app);
    // Clear it again once it has been read.
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FLASH_FOR + Duration::from_millis(100));
        request_refresh(&app);
    });
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Sets up state and listeners. Call once from `setup`, after the managers.
pub fn install(app: &AppHandle) {
    app.manage(MenubarState::default());
    listen(app);
    refresh_devices(app);

    // What the WebView did once onboarding finished: the keyboard and
    // shortcuts need Accessibility, so only start them once it's granted.
    let (mic, ax) = permissions();
    if ax {
        start_input(app);
    }
    let snapshot = snapshot(app);
    let needs_setup = !view::setup_rows(&snapshot).is_empty();
    if !needs_setup {
        mark_onboarded(app);
    }
    info!("Menu bar UI ready (microphone={mic}, accessibility={ax}, setup needed={needs_setup})");
    if needs_setup {
        // First run: open the popover so setup is the first thing seen. The
        // status item needs a moment to get a position on screen.
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            open(&app);
        });
    }
}

/// Any click on the menu bar icon opens the popover.
pub fn configure_tray(
    builder: tauri::tray::TrayIconBuilder<tauri::Wry>,
) -> tauri::tray::TrayIconBuilder<tauri::Wry> {
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
    builder
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            // Either button opens the one popover: everything the old
            // right-click menu did is in it.
            if let TrayIconEvent::Click {
                button: MouseButton::Left | MouseButton::Right,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                on_tray_click(tray.app_handle());
            }
        })
}

/// Left click on the menu bar icon.
pub fn on_tray_click(app: &AppHandle) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || appkit::toggle(&app2));
}

/// Whether the popover is on screen.
pub fn is_open() -> bool {
    appkit::is_open()
}

/// Shows the popover (used wherever upstream shows the settings window).
pub fn open(app: &AppHandle) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || appkit::show(&app2));
}

/// Redraws the popover if it's open. Cheap to call often: calls made while a
/// redraw is pending are merged into it.
pub fn request_refresh(app: &AppHandle) {
    // Nothing on screen to redraw; the next open reads fresh state.
    if !appkit::is_open() {
        return;
    }
    let Some(s) = state(app) else { return };
    if s.refresh_pending.swap(true, Ordering::SeqCst) {
        return;
    }
    // Posted from another thread so it always runs on a later turn of the
    // main loop, never inside the control action that asked for it (which
    // would tear down the control mid-action).
    let app2 = app.clone();
    std::thread::spawn(move || {
        let app3 = app2.clone();
        let posted = app2.run_on_main_thread(move || {
            if let Some(s) = state(&app3) {
                s.refresh_pending.store(false, Ordering::SeqCst);
            }
            appkit::refresh(&app3);
        });
        if posted.is_err() {
            if let Some(s) = state(&app2) {
                s.refresh_pending.store(false, Ordering::SeqCst);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// State gathering
// ---------------------------------------------------------------------------

fn permissions() -> (bool, bool) {
    tauri::async_runtime::block_on(async {
        (
            tauri_plugin_macos_permissions::check_microphone_permission().await,
            tauri_plugin_macos_permissions::check_accessibility_permission().await,
        )
    })
}

fn phase(app: &AppHandle) -> Phase {
    match tray::icon_state(app) {
        TrayIconState::Idle => Phase::Idle,
        TrayIconState::Recording => Phase::Recording,
        TrayIconState::Transcribing => Phase::Transcribing,
    }
}

/// Lists audio devices off the main thread; they show on the next redraw.
fn refresh_devices(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let names = |r: Result<Vec<crate::commands::audio::AudioDevice>, String>| {
            r.map(|ds| ds.into_iter().map(|d| d.name).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        let (mics, outs) = tauri::async_runtime::block_on(async {
            (
                names(crate::commands::audio::get_available_microphones().await),
                names(crate::commands::audio::get_available_output_devices().await),
            )
        });
        if let Some(s) = state(&app) {
            let mut inner = s.lock();
            inner.microphones = mics;
            inner.outputs = outs;
        }
        request_refresh(&app);
    });
}

/// Everything the popover shows, read fresh.
pub(crate) fn snapshot(app: &AppHandle) -> Snapshot {
    snapshot_for(app, None)
}

/// A snapshot for drawing `tab` (`None`: every tab). History is a database
/// read, so it's skipped for tabs that don't show it.
pub(crate) fn snapshot_for(app: &AppHandle, tab: Option<Tab>) -> Snapshot {
    let wants_history = matches!(tab, None | Some(Tab::Home) | Some(Tab::History));
    let settings = settings::get_settings(app);
    let model_manager = app.state::<Arc<ModelManager>>();
    let models = model_manager.get_available_models();
    let languages = model_manager
        .get_model_info(&settings.selected_model)
        .and_then(|m| {
            tray::language_menu(
                &settings,
                &m,
                model_manager.language_detector_path().is_some(),
            )
        })
        .map(|menu| Languages {
            active: menu.active,
            auto_enabled: menu.auto_enabled,
            options: menu.main.into_iter().chain(menu.others).collect(),
        });
    let history = wants_history
        .then(|| app.try_state::<Arc<HistoryManager>>())
        .flatten()
        .and_then(|hm| {
            tauri::async_runtime::block_on(hm.get_history_entries(None, Some(HISTORY_SHOWN))).ok()
        })
        .map(|page| page.entries)
        .unwrap_or_default();
    let (mic_permission, accessibility_permission) = permissions();
    // Accessibility may have been granted in System Settings since launch;
    // start the keyboard and shortcuts as soon as it is (idempotent).
    if accessibility_permission {
        start_input(app);
    }
    let notice = crate::notices::tray_headline(app).map(|n| {
        let strings = crate::tray_i18n::get_tray_translations(Some(settings.app_language.clone()));
        n.code.tray_title(&strings).to_string()
    });

    let (tab_progress, capturing, partial, flash, microphones, outputs) = match state(app) {
        Some(s) => {
            let mut inner = s.lock();
            if inner
                .flash
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() > FLASH_FOR)
            {
                inner.flash = None;
            }
            (
                inner.progress.clone(),
                inner
                    .capture
                    .is_some()
                    .then(|| inner.capture_binding.clone()),
                inner.partial.clone(),
                inner.flash.as_ref().map(|(m, _)| m.clone()),
                inner.microphones.clone(),
                inner.outputs.clone(),
            )
        }
        None => Default::default(),
    };

    let view_state = state(app)
        .map(|s| {
            let inner = s.lock();
            (
                inner.model_filter.clone().unwrap_or_else(|| "all".into()),
                inner
                    .model_sort
                    .clone()
                    .unwrap_or_else(|| "recommended".into()),
                inner
                    .confirm
                    .as_ref()
                    .filter(|(_, at)| at.elapsed() < CONFIRM_FOR)
                    .map(|(k, _)| k.clone()),
            )
        })
        .unwrap_or_else(|| ("all".into(), "recommended".into(), None));

    Snapshot {
        phase: phase(app),
        settings,
        models,
        progress: tab_progress,
        history,
        microphones,
        outputs,
        languages,
        mic_permission,
        accessibility_permission,
        capturing_shortcut: capturing,
        partial_shortcut: partial,
        flash,
        notice,
        version: app.package_info().version.to_string(),
        now: chrono::Utc::now().timestamp(),
        command: crate::cleanup_command::load(app),
        secure_input: crate::secure_input::tray_warning_active(app),
        command_error: crate::cleanup_command::last_error(),
        model_filter: view_state.0,
        model_sort: view_state.1,
        confirm_delete: view_state.2,
        screen_recording: crate::cleanup_context::screen_recording_allowed(),
        model_loaded: app
            .try_state::<Arc<crate::managers::transcription::TranscriptionManager>>()
            .is_some_and(|tm| tm.is_model_loaded()),
    }
}

/// The tab to show: the last one picked, else Home.
pub(crate) fn current_tab(app: &AppHandle) -> Tab {
    state(app).and_then(|s| s.lock().tab).unwrap_or(Tab::Home)
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

fn start_input(app: &AppHandle) {
    if let Err(e) = crate::commands::initialize_enigo(app.clone()) {
        warn!("Menu bar: input not ready: {e}");
    }
    if let Err(e) = crate::commands::initialize_shortcuts(app.clone()) {
        warn!("Menu bar: shortcuts not ready: {e}");
    }
}

fn mark_onboarded(app: &AppHandle) {
    if !settings::get_settings(app).onboarding_completed {
        let _ = crate::commands::complete_onboarding(app.clone());
    }
}

/// After a permission prompt, checks for a while until it's granted, then
/// finishes setup.
fn watch_permissions(app: &AppHandle) {
    // One watcher at a time, however many times Allow is clicked.
    static WATCHING: AtomicBool = AtomicBool::new(false);
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let _done = scopeguard_reset(&WATCHING);
        for _ in 0..120 {
            std::thread::sleep(Duration::from_millis(500));
            let (mic, ax) = permissions();
            request_refresh(&app);
            if ax {
                start_input(&app);
            }
            if mic && ax {
                after_setup_step(&app);
                return;
            }
        }
    });
}

/// Clears `flag` when dropped.
fn scopeguard_reset(flag: &'static AtomicBool) -> impl Drop {
    struct Reset(&'static AtomicBool);
    impl Drop for Reset {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    Reset(flag)
}

fn after_setup_step(app: &AppHandle) {
    if view::setup_rows(&snapshot(app)).is_empty() {
        mark_onboarded(app);
        flash(app, "Handy Bar is ready. Press your shortcut to dictate.");
    }
}

/// Runs a slow setter on a worker thread, then redraws.
fn in_background(
    app: &AppHandle,
    f: impl FnOnce(&AppHandle) -> Result<(), String> + Send + 'static,
) {
    let app = app.clone();
    std::thread::spawn(move || {
        if let Err(e) = f(&app) {
            warn!("Menu bar action failed: {e}");
            flash(&app, e);
        }
        request_refresh(&app);
    });
}

/// `AVAuthorizationStatusNotDetermined`: the system prompt can still be shown.
const MIC_NOT_DETERMINED: isize = 0;

/// `AVCaptureDevice.authorizationStatus(for: .audio)`.
fn microphone_status() -> isize {
    use objc2::{class, msg_send};
    let audio = objc2_foundation::NSString::from_str("soun");
    unsafe { msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: &*audio] }
}

fn open_privacy_pane(anchor: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(format!(
            "x-apple.systempreferences:com.apple.preference.security?{anchor}"
        ))
        .status()
        .map(|_| ())
        .map_err(|e| format!("Couldn't open System Settings: {e}"))
}

fn set_bool(app: &AppHandle, key: BoolKey, on: bool) -> Result<(), String> {
    use crate::shortcut as sc;
    let a = app.clone();
    match key {
        BoolKey::AudioFeedback => sc::change_audio_feedback_setting(a, on),
        BoolKey::Autostart => sc::change_autostart_setting(a, on),
        BoolKey::TranslateToEnglish => sc::change_translate_to_english_setting(a, on),
        BoolKey::AppendTrailingSpace => sc::change_append_trailing_space_setting(a, on),
        BoolKey::MuteWhileRecording => sc::change_mute_while_recording_setting(a, on),
        BoolKey::AlwaysOnMicrophone => {
            tauri::async_runtime::block_on(crate::commands::audio::update_microphone_mode(a, on))
        }
        BoolKey::AutoSubmit => sc::change_auto_submit_setting(a, on),
        BoolKey::FillerWordRemoval => sc::change_filler_word_removal_enabled_setting(a, on),
        BoolKey::VoiceActivityDetection => sc::change_vad_enabled_setting(a, on),
        BoolKey::PostProcess => sc::change_post_process_enabled_setting(a, on),
        BoolKey::CommandPrewarm => {
            update_command(app, |c| c.prewarm = on);
            Ok(())
        }
        BoolKey::AppHints => {
            update_command(app, |c| c.app_hints = on);
            Ok(())
        }
        BoolKey::ScreenText => {
            update_command(app, |c| c.screen_text = on);
            if on && !crate::cleanup_context::screen_recording_allowed() {
                request_screen_recording();
            }
            Ok(())
        }
    }
}

/// Shows the system prompt the first time; after that macOS only lets the
/// user change it in System Settings, so open that pane too.
fn request_screen_recording() {
    if !crate::cleanup_context::request_screen_recording() {
        let _ = open_privacy_pane("Privacy_ScreenCapture");
    }
}

/// Changes Handy Bar's command-line cleanup settings.
fn update_command(app: &AppHandle, f: impl FnOnce(&mut crate::cleanup_command::CommandSettings)) {
    let mut c = crate::cleanup_command::load(app);
    f(&mut c);
    crate::cleanup_command::save(app, &c);
    if c.enabled {
        crate::cleanup_command::warm_up();
    }
}

fn active_provider_id(app: &AppHandle) -> String {
    settings::get_settings(app).post_process_provider_id
}

fn set_text(app: &AppHandle, key: view::TextKey, value: String) -> Result<(), String> {
    use crate::shortcut as sc;
    use view::TextKey;
    let a = app.clone();
    let value = value.trim_end_matches('\n').to_string();
    match key {
        TextKey::Command => {
            update_command(app, |c| c.command = value.trim().to_string());
            Ok(())
        }
        TextKey::ApiKey => {
            sc::change_post_process_api_key_setting(a, active_provider_id(app), value.trim().into())
        }
        TextKey::BaseUrl => sc::change_post_process_base_url_setting(
            a,
            active_provider_id(app),
            value.trim().into(),
        ),
        TextKey::ModelName => {
            sc::change_post_process_model_setting(a, active_provider_id(app), value.trim().into())
        }
        TextKey::Prompt => {
            let st = settings::get_settings(app);
            let id = st
                .post_process_selected_prompt_id
                .clone()
                .or_else(|| st.post_process_prompts.first().map(|p| p.id.clone()))
                .ok_or("No prompt to edit")?;
            let name = st
                .post_process_prompts
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
                .ok_or("That prompt is gone")?;
            if st
                .post_process_prompts
                .iter()
                .any(|p| p.id == id && p.prompt == value)
            {
                return Ok(());
            }
            sc::update_post_process_prompt(a, id, name, value)
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|e| format!("Unknown value '{value}': {e}"))
}

fn set_choice(app: &AppHandle, key: ChoiceKey, value: String) -> Result<(), String> {
    use crate::shortcut as sc;
    let a = app.clone();
    match key {
        ChoiceKey::Microphone => tauri::async_runtime::block_on(
            crate::commands::audio::set_selected_microphone(a, value),
        ),
        ChoiceKey::OutputDevice => crate::commands::audio::set_selected_output_device(a, value),
        ChoiceKey::Language => sc::change_selected_language_setting(a, value),
        ChoiceKey::Activation => sc::change_shortcut_activation_setting(a, parse(&value)?),
        ChoiceKey::PasteMethod => sc::change_paste_method_setting(a, value),
        ChoiceKey::ClipboardHandling => sc::change_clipboard_handling_setting(a, value),
        ChoiceKey::SoundTheme => sc::change_sound_theme_setting(a, value),
        ChoiceKey::AutoSubmitKey => sc::change_auto_submit_key_setting(a, value),
        ChoiceKey::OverlayStyle => sc::change_overlay_style_setting(a, value),
        ChoiceKey::OverlayPosition => sc::change_overlay_position_setting(a, value),
        ChoiceKey::Model => {
            crate::commands::models::switch_active_model(&a, &value)?;
            tray::update_tray_menu(&a);
            Ok(())
        }
        ChoiceKey::CleanupEngine => {
            let command = value == view::COMMAND_ENGINE;
            update_command(app, |c| c.enabled = command);
            if command {
                Ok(())
            } else {
                sc::set_post_process_provider(a, value)
            }
        }
        ChoiceKey::CommandPreset => {
            if let Some((_, _, command, secs)) = crate::cleanup_command::PRESETS
                .iter()
                .find(|(id, ..)| *id == value)
            {
                update_command(app, |c| {
                    c.command = command.to_string();
                    c.timeout_secs = *secs;
                });
            }
            // "Custom": keep the command; the field below is for editing it.
            Ok(())
        }
        ChoiceKey::CommandTimeout => {
            let secs: u64 = value.parse().map_err(|e| format!("{e}"))?;
            update_command(app, |c| c.timeout_secs = secs);
            Ok(())
        }
        ChoiceKey::Prompt => sc::set_post_process_selected_prompt(a, value),
        ChoiceKey::ModelFilter | ChoiceKey::ModelSort => {
            if let Some(s) = state(app) {
                let mut inner = s.lock();
                if key == ChoiceKey::ModelFilter {
                    inner.model_filter = Some(value);
                } else {
                    inner.model_sort = Some(value);
                }
            }
            Ok(())
        }
        ChoiceKey::UnloadTimeout => {
            crate::commands::transcription::set_model_unload_timeout(a, parse(&value)?);
            Ok(())
        }
        ChoiceKey::HistoryLimit => {
            let limit: usize = value.parse().map_err(|e| format!("{e}"))?;
            tauri::async_runtime::block_on(crate::commands::history::update_history_limit(
                a.clone(),
                a.state(),
                limit,
            ))
        }
        ChoiceKey::RecordingRetention => tauri::async_runtime::block_on(
            crate::commands::history::update_recording_retention_period(
                a.clone(),
                a.state(),
                value,
            ),
        ),
    }
}

fn download(app: &AppHandle, model_id: String) {
    if let Some(s) = state(app) {
        s.lock().progress.insert(model_id.clone(), 0.0);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result =
            crate::commands::models::download_model(app.clone(), app.state(), model_id.clone())
                .await;
        if let Some(s) = state(&app) {
            s.lock().progress.remove(&model_id);
        }
        // The rest takes snapshots, which `block_on`; that panics on an async
        // runtime worker, so finish on a plain thread.
        std::thread::spawn(move || finish_download(&app, model_id, result));
    });
}

fn finish_download(app: &AppHandle, model_id: String, result: Result<(), String>) {
    let app = app.clone();
    {
        match result {
            Ok(()) => {
                let settings = settings::get_settings(&app);
                let selected_ok = app
                    .state::<Arc<ModelManager>>()
                    .get_model_info(&settings.selected_model)
                    .is_some_and(|m| m.is_downloaded);
                if !selected_ok {
                    if let Err(e) = crate::commands::models::switch_active_model(&app, &model_id) {
                        error!("Menu bar: couldn't switch to {model_id}: {e}");
                    }
                }
                after_setup_step(&app);
            }
            Err(e) if e.to_lowercase().contains("cancel") => {}
            Err(e) => flash(&app, format!("Download failed: {e}")),
        }
        request_refresh(&app);
    }
}

fn start_capture(app: &AppHandle, binding_id: &str) -> Result<(), String> {
    // The key listener starts once Accessibility is granted; if that just
    // happened, start it now rather than failing.
    if !permissions().1 {
        return Err("Allow Accessibility access first, then try again.".into());
    }
    start_input(app);
    crate::shortcut::handy_keys::start_handy_keys_recording(app.clone(), binding_id.to_string())
        .map_err(|e| {
            if e.contains("secure-input") {
                "Another app is using secure input (a password field), so keys can't be read right now.".to_string()
            } else {
                format!("Couldn't start recording a shortcut: {e}")
            }
        })?;
    if let Some(s) = state(app) {
        let mut inner = s.lock();
        inner.capture = Some(Capture::new());
        inner.capture_binding = binding_id.to_string();
        inner.partial.clear();
    }
    Ok(())
}

fn stop_capture(app: &AppHandle) {
    if let Some(s) = state(app) {
        let mut inner = s.lock();
        inner.capture = None;
        inner.partial.clear();
    }
    let _ = crate::shortcut::handy_keys::stop_handy_keys_recording(app.clone());
}

fn on_key_event(app: &AppHandle, event: KeyEvent) {
    let step = match state(app) {
        Some(s) => match s.lock().capture.as_mut() {
            Some(c) => c.feed(&event),
            None => return,
        },
        None => return,
    };
    match step {
        Step::Partial(held) => {
            if let Some(s) = state(app) {
                s.lock().partial = held;
            }
            request_refresh(app);
        }
        Step::Commit(binding) => {
            let binding_id = state(app)
                .map(|s| s.lock().capture_binding.clone())
                .filter(|b| !b.is_empty())
                .unwrap_or_else(|| "transcribe".into());
            stop_capture(app);
            let settings = settings::get_settings(app);
            let clash = settings
                .bindings
                .iter()
                .filter(|(id, _)| **id != binding_id && id.as_str() != "cancel")
                .map(|(_, b)| b.current_binding.clone())
                .find(|other| view::shortcuts_overlap(&binding, other));
            if let Some(other) = clash {
                flash(
                    app,
                    format!(
                        "{} overlaps {}, so both would start. Try another.",
                        view::pretty_shortcut(&binding),
                        view::pretty_shortcut(&other)
                    ),
                );
                request_refresh(app);
                return;
            }
            match crate::shortcut::change_binding(app.clone(), binding_id, binding.clone()) {
                Ok(r)
                    if serde_json::to_value(&r)
                        .ok()
                        .and_then(|v| v.get("success").and_then(|s| s.as_bool()))
                        .unwrap_or(false) =>
                {
                    flash(
                        app,
                        format!("Shortcut set to {}", view::pretty_shortcut(&binding)),
                    )
                }
                Ok(_) => flash(app, "That shortcut can't be used. Try another."),
                Err(e) => flash(app, format!("That shortcut can't be used: {e}")),
            }
            tray::update_tray_menu(app);
        }
        Step::Cancel => {
            stop_capture(app);
            request_refresh(app);
        }
        Step::Ignore => {}
    }
}

/// Runs a popover action. Called on the main thread; anything slow goes to
/// a worker so the popover never stalls.
pub(crate) fn perform(app: &AppHandle, action: Action) {
    info!("Menu bar action: {action:?}");
    let result: Result<(), String> = match action {
        Action::ToggleDictation => {
            // Give focus back to the app being dictated into first.
            appkit::close_and_yield(app);
            crate::signal_handle::send_transcription_input(app, "transcribe", "menubar");
            Ok(())
        }
        Action::CancelDictation => {
            crate::utils::cancel_current_operation(app);
            Ok(())
        }
        Action::SelectTab(tab) => {
            if let Some(s) = state(app) {
                s.lock().tab = Some(tab);
            }
            if tab == Tab::Home || tab == Tab::Settings {
                refresh_devices(app);
            }
            Ok(())
        }
        Action::CopyEntry(id) => {
            let entry = app.try_state::<Arc<HistoryManager>>().and_then(|hm| {
                tauri::async_runtime::block_on(hm.get_entry_by_id(id))
                    .ok()
                    .flatten()
            });
            match entry {
                Some(e) => {
                    let text = e
                        .post_processed_text
                        .filter(|t| !t.trim().is_empty())
                        .unwrap_or(e.transcription_text);
                    app.clipboard()
                        .write_text(text)
                        .map(|_| flash(app, "Copied to the clipboard"))
                        .map_err(|e| e.to_string())
                }
                None => Err("That transcript is gone".into()),
            }
        }
        Action::DeleteEntry(id) => {
            tauri::async_runtime::block_on(crate::commands::history::delete_history_entry(
                app.clone(),
                app.state(),
                app.state(),
                id,
            ))
        }
        Action::DownloadModel(id) => {
            download(app, id);
            Ok(())
        }
        Action::CancelDownload(id) => {
            if let Some(s) = state(app) {
                s.lock().progress.remove(&id);
            }
            app.state::<Arc<ModelManager>>()
                .cancel_download(&id)
                .map_err(|e| e.to_string())
        }
        Action::SelectModel(id) => {
            let app2 = app.clone();
            std::thread::spawn(move || {
                match crate::commands::models::switch_active_model(&app2, &id) {
                    Ok(()) => after_setup_step(&app2),
                    Err(e) => flash(&app2, format!("Couldn't switch model: {e}")),
                }
                tray::update_tray_menu(&app2);
                request_refresh(&app2);
            });
            Ok(())
        }
        Action::DeleteModel(id) => {
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = crate::commands::models::delete_model(
                    app2.clone(),
                    app2.state(),
                    app2.state(),
                    id,
                )
                .await
                {
                    flash(&app2, format!("Couldn't delete: {e}"));
                }
                request_refresh(&app2);
            });
            Ok(())
        }
        Action::RequestMicrophone => {
            // macOS asks only once. After a "Don't Allow" the request is
            // silently ignored, so send the person to the setting instead.
            let r = if microphone_status() == MIC_NOT_DETERMINED {
                tauri::async_runtime::block_on(
                    tauri_plugin_macos_permissions::request_microphone_permission(),
                )
            } else {
                flash(app, "Turn on Handy in Privacy & Security › Microphone.");
                open_privacy_pane("Privacy_Microphone")
            };
            watch_permissions(app);
            r
        }
        Action::RequestAccessibility => {
            tauri::async_runtime::block_on(
                tauri_plugin_macos_permissions::request_accessibility_permission(),
            );
            watch_permissions(app);
            Ok(())
        }
        // Switching audio devices restarts the audio stream, which can block
        // for a while: keep it off the main thread so the popover stays live.
        Action::SetChoice(
            key @ (ChoiceKey::Microphone | ChoiceKey::OutputDevice | ChoiceKey::Model),
            value,
        ) => {
            in_background(app, move |a| set_choice(a, key, value));
            Ok(())
        }
        Action::SetBool(BoolKey::AlwaysOnMicrophone, on) => {
            in_background(app, move |a| set_bool(a, BoolKey::AlwaysOnMicrophone, on));
            Ok(())
        }
        Action::SetBool(key, on) => set_bool(app, key, on),
        Action::SetChoice(key, value) => {
            let r = set_choice(app, key, value);
            if matches!(key, ChoiceKey::Language) {
                tray::rebuild_tray_menu(app);
            }
            r
        }
        Action::StartShortcutCapture(binding_id) => start_capture(app, &binding_id),
        Action::CancelShortcutCapture => {
            stop_capture(app);
            Ok(())
        }
        Action::ResetShortcut(binding_id) => {
            crate::shortcut::reset_binding(app.clone(), binding_id).map(|_| ())
        }
        Action::SetText(key, value) => set_text(app, key, value),
        Action::ConfirmDelete(key) => {
            if let Some(s) = state(app) {
                s.lock().confirm = Some((key, Instant::now()));
            }
            // Put the plain Delete back if the second click doesn't come.
            let app2 = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(CONFIRM_FOR);
                request_refresh(&app2);
            });
            Ok(())
        }
        Action::OpenUrl(url) => {
            // Only the app's own links reach here (see view.rs).
            std::process::Command::new("/usr/bin/open")
                .arg(&url)
                .spawn()
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        Action::RequestScreenRecording => {
            request_screen_recording();
            watch_permissions(app);
            Ok(())
        }
        Action::UnloadModel => {
            let tm = app.state::<Arc<crate::managers::transcription::TranscriptionManager>>();
            if tm.is_model_loaded() {
                tm.ensure_model_unlocked().map(|()| {
                    tm.request_unload();
                    flash(app, "Model unloaded");
                })
            } else {
                Ok(())
            }
        }
        Action::OpenRecordingsFolder => crate::commands::open_recordings_folder(app.clone()),
        Action::OpenLogs => crate::commands::open_log_dir(app.clone()),
        Action::Quit => {
            app.exit(0);
            Ok(())
        }
    };
    if let Err(e) = result {
        warn!("Menu bar action failed: {e}");
        flash(app, e);
    }
    request_refresh(app);
}

// ---------------------------------------------------------------------------
// Backend events
// ---------------------------------------------------------------------------

fn listen(app: &AppHandle) {
    for event in [
        "model-state-changed",
        "model-download-complete",
        "model-download-failed",
        "model-download-cancelled",
        "model-deleted",
        "models-updated",
        "settings-changed",
        "secure-input-changed",
    ] {
        let a = app.clone();
        app.listen_any(event, move |_| request_refresh(&a));
    }

    let a = app.clone();
    app.listen_any("model-download-progress", move |event| {
        if let Ok(p) = serde_json::from_str::<DownloadProgress>(event.payload()) {
            let redraw = match state(&a) {
                Some(s) => {
                    let mut inner = s.lock();
                    inner
                        .progress
                        .insert(p.model_id, (p.percentage / 100.0).clamp(0.0, 1.0));
                    // At most 4 redraws a second while downloading.
                    let due = inner
                        .last_progress_redraw
                        .is_none_or(|t| t.elapsed() > Duration::from_millis(250));
                    if due {
                        inner.last_progress_redraw = Some(Instant::now());
                    }
                    due
                }
                None => false,
            };
            if redraw {
                request_refresh(&a);
            }
        }
    });

    let a = app.clone();
    app.listen_any("handy-keys-event", move |event| {
        if let Ok(e) = serde_json::from_str::<KeyEvent>(event.payload()) {
            on_key_event(&a, e);
        }
    });

    let a = app.clone();
    HistoryUpdatePayload::listen_any(app, move |_| request_refresh(&a));

    let a = app.clone();
    crate::managers::transcription::StreamTextEvent::listen_any(app, move |e| {
        hud::live_text(&a, &e.payload.committed, &e.payload.tentative)
    });
}
