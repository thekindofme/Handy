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
    partial: String,
    flash: Option<(String, Instant)>,
    microphones: Vec<String>,
    outputs: Vec<String>,
    last_progress_redraw: Option<Instant>,
}

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

/// Left click on the menu bar icon.
pub fn on_tray_click(app: &AppHandle) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || appkit::toggle(&app2));
}

/// Shows the popover (used wherever upstream shows the settings window).
pub fn open(app: &AppHandle) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || appkit::show(&app2));
}

/// Redraws the popover if it's open. Cheap to call often: calls made while a
/// redraw is pending are merged into it.
pub fn request_refresh(app: &AppHandle) {
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
        let _ = app2.run_on_main_thread(move || {
            if let Some(s) = state(&app3) {
                s.refresh_pending.store(false, Ordering::SeqCst);
            }
            appkit::refresh(&app3);
        });
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
    let history = app
        .try_state::<Arc<HistoryManager>>()
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
                inner.capture.is_some(),
                inner.partial.clone(),
                inner.flash.as_ref().map(|(m, _)| m.clone()),
                inner.microphones.clone(),
                inner.outputs.clone(),
            )
        }
        None => Default::default(),
    };

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
    let app = app.clone();
    std::thread::spawn(move || {
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

fn after_setup_step(app: &AppHandle) {
    if view::setup_rows(&snapshot(app)).is_empty() {
        mark_onboarded(app);
        flash(app, "Handy is ready. Press your shortcut to dictate.");
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
        BoolKey::UpdateChecks => sc::change_update_checks_setting(a, on),
        BoolKey::VoiceActivityDetection => sc::change_vad_enabled_setting(a, on),
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
    });
}

fn start_capture(app: &AppHandle) -> Result<(), String> {
    // The key listener starts once Accessibility is granted; if that just
    // happened, start it now rather than failing.
    if !permissions().1 {
        return Err("Allow Accessibility access first, then try again.".into());
    }
    start_input(app);
    crate::shortcut::handy_keys::start_handy_keys_recording(app.clone(), "transcribe".into())
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
            stop_capture(app);
            match crate::shortcut::change_binding(app.clone(), "transcribe".into(), binding.clone())
            {
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
            let entry = snapshot(app).history.into_iter().find(|e| e.id == id);
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
        Action::SetChoice(key @ (ChoiceKey::Microphone | ChoiceKey::OutputDevice), value) => {
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
        Action::StartShortcutCapture => start_capture(app),
        Action::CancelShortcutCapture => {
            stop_capture(app);
            Ok(())
        }
        Action::ResetShortcut => {
            crate::shortcut::reset_binding(app.clone(), "transcribe".into()).map(|_| ())
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
