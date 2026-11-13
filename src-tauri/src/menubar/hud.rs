//! Native recording indicator: replaces the WebView overlay when the menu
//! bar UI is on.
//!
//! A small borderless panel that never takes focus or clicks, joins every
//! Space and sits over full-screen apps. It shows live mic levels while
//! recording (plus live text with the Live style), a spinner while
//! transcribing, and notices. It is created once and kept hidden, so a
//! session costs a few redraws, not a WebKit process.
//!
//! `overlay.rs` calls in here from any thread; everything is forwarded to
//! the main thread.

use crate::settings::{self, OverlayPosition, OverlayStyle};
use objc2::rc::Retained;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSBox, NSBoxType, NSColor, NSEvent, NSFont, NSImage,
    NSImageView, NSLineBreakMode, NSPanel, NSProgressIndicator, NSProgressIndicatorStyle, NSScreen,
    NSStackView, NSStatusWindowLevel, NSTextField, NSTitlePosition,
    NSUserInterfaceLayoutOrientation, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::AppHandle;

const HEIGHT: f64 = 36.0;
const MARGIN: f64 = 46.0;
const BARS: usize = 16;

/// Latest mic levels (0..1), written from the audio thread.
static LEVELS: Mutex<Vec<f32>> = Mutex::new(Vec::new());
static LEVELS_REDRAW_PENDING: AtomicBool = AtomicBool::new(false);
/// Bumped by every show/hide; a delayed hide only runs if nothing newer came.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Listening,
    Working,
    Notice,
}

struct Hud {
    panel: Retained<NSPanel>,
    card: Retained<NSBox>,
    icon: Retained<NSImageView>,
    levels: Retained<LevelsView>,
    spinner: Retained<NSProgressIndicator>,
    label: Retained<NSTextField>,
    mode: Mode,
}

thread_local! {
    static HUD: RefCell<Option<Hud>> = const { RefCell::new(None) };
}

define_class!(
    /// Draws the level bars from `LEVELS`.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "HandyHudLevels"]
    struct LevelsView;

    impl LevelsView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let levels = LEVELS.lock().map(|l| l.clone()).unwrap_or_default();
            let gap = 2.0;
            let width = (bounds.size.width - gap * (BARS as f64 - 1.0)) / BARS as f64;
            NSColor::whiteColor().set();
            for i in 0..BARS {
                let level = levels.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0) as f64;
                let h = (bounds.size.height * (0.12 + 0.88 * level)).max(3.0);
                let rect = NSRect::new(
                    NSPoint::new(i as f64 * (width + gap), (bounds.size.height - h) / 2.0),
                    NSSize::new(width, h),
                );
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, width / 2.0, width / 2.0)
                    .fill();
            }
        }
    }
);

fn on_main(app: &AppHandle, f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    let _ = app.run_on_main_thread(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            f(mtm)
        }
    });
}

fn build(mtm: MainThreadMarker) -> Hud {
    let panel: Retained<NSPanel> = unsafe {
        msg_send![
            NSPanel::alloc(mtm),
            initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, HEIGHT)),
            styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered,
            defer: true
        ]
    };
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel(NSStatusWindowLevel);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(true);
    panel.setIgnoresMouseEvents(true);
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );

    let card = NSBox::new(mtm);
    card.setBoxType(NSBoxType::Custom);
    card.setTitlePosition(NSTitlePosition::NoTitle);
    card.setCornerRadius(HEIGHT / 2.0);
    card.setBorderWidth(0.0);
    card.setFillColor(&NSColor::colorWithWhite_alpha(0.08, 0.86));
    card.setContentViewMargins(NSSize::new(0.0, 0.0));

    let icon = NSImageView::new(mtm);
    icon.setTranslatesAutoresizingMaskIntoConstraints(false);
    let levels: Retained<LevelsView> =
        unsafe { msg_send![LevelsView::alloc(mtm), initWithFrame: NSRect::ZERO] };
    levels.setTranslatesAutoresizingMaskIntoConstraints(false);
    let spinner = NSProgressIndicator::new(mtm);
    spinner.setStyle(NSProgressIndicatorStyle::Spinning);
    spinner.setControlSize(objc2_app_kit::NSControlSize::Small);
    spinner.setTranslatesAutoresizingMaskIntoConstraints(false);
    // White spinner on the dark pill.
    unsafe {
        let dark =
            objc2_app_kit::NSAppearance::appearanceNamed(objc2_app_kit::NSAppearanceNameDarkAqua);
        if let Some(dark) = dark {
            let _: () = msg_send![&*card, setAppearance: &*dark];
        }
    }
    let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize_weight(12.5, 0.23)));
    label.setTextColor(Some(&NSColor::whiteColor()));
    label.setLineBreakMode(NSLineBreakMode::ByTruncatingHead);
    label.setTranslatesAutoresizingMaskIntoConstraints(false);

    let views: [&NSView; 4] = [&icon, &levels, &spinner, &label];
    let row = NSStackView::stackViewWithViews(&NSArray::from_slice(&views), mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(8.0);
    row.setEdgeInsets(NSEdgeInsets {
        top: 0.0,
        left: 12.0,
        bottom: 0.0,
        right: 14.0,
    });
    row.setTranslatesAutoresizingMaskIntoConstraints(false);
    card.setContentView(Some(&row));
    let constraints = [
        icon.widthAnchor().constraintEqualToConstant(16.0),
        icon.heightAnchor().constraintEqualToConstant(16.0),
        levels
            .widthAnchor()
            .constraintEqualToConstant(BARS as f64 * 5.0),
        levels.heightAnchor().constraintEqualToConstant(18.0),
        label
            .widthAnchor()
            .constraintLessThanOrEqualToConstant(460.0),
        row.leadingAnchor()
            .constraintEqualToAnchor(&card.leadingAnchor()),
        row.trailingAnchor()
            .constraintEqualToAnchor(&card.trailingAnchor()),
        row.topAnchor().constraintEqualToAnchor(&card.topAnchor()),
        row.bottomAnchor()
            .constraintEqualToAnchor(&card.bottomAnchor()),
    ];
    let refs: Vec<&objc2_app_kit::NSLayoutConstraint> = constraints.iter().map(|c| &**c).collect();
    objc2_app_kit::NSLayoutConstraint::activateConstraints(&NSArray::from_slice(&refs));
    panel.setContentView(Some(&card));
    // Accessibility: announce state changes like a status indicator.
    unsafe {
        let _: () = msg_send![&*card, setAccessibilityIdentifier: &*NSString::from_str("hud")];
    }

    Hud {
        panel,
        card,
        icon,
        levels,
        spinner,
        label,
        mode: Mode::Notice,
    }
}

fn with_hud(mtm: MainThreadMarker, f: impl FnOnce(&mut Hud)) {
    HUD.with(|cell| {
        let mut slot = cell.borrow_mut();
        let hud = slot.get_or_insert_with(|| build(mtm));
        f(hud);
    });
}

fn symbol(name: &str, tint: Retained<NSColor>, view: &NSImageView) {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    );
    view.setImage(image.as_deref());
    view.setContentTintColor(Some(&tint));
}

/// Sizes the panel to its content and places it on the screen with the
/// pointer, at the top or bottom as set.
fn place(hud: &Hud, app: &AppHandle) {
    let fit = hud.card.fittingSize();
    let width = fit.width.max(120.0);
    let mouse = NSEvent::mouseLocation();
    let mtm = MainThreadMarker::from(&*hud.panel);
    let screens = NSScreen::screens(mtm);
    let screen = screens
        .iter()
        .find(|s| {
            let f = s.frame();
            mouse.x >= f.origin.x
                && mouse.x <= f.origin.x + f.size.width
                && mouse.y >= f.origin.y
                && mouse.y <= f.origin.y + f.size.height
        })
        .or_else(|| NSScreen::mainScreen(mtm));
    let Some(screen) = screen else { return };
    let visible = screen.visibleFrame();
    let x = visible.origin.x + (visible.size.width - width) / 2.0;
    let y = match settings::get_settings(app).overlay_position {
        OverlayPosition::Top => visible.origin.y + visible.size.height - HEIGHT - 12.0,
        OverlayPosition::Bottom => visible.origin.y + MARGIN,
    };
    hud.panel.setFrame_display(
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, HEIGHT)),
        true,
    );
}

fn present(app: &AppHandle, mode: Mode, text: String) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    let app2 = app.clone();
    on_main(app, move |mtm| {
        with_hud(mtm, |hud| {
            hud.mode = mode;
            match mode {
                Mode::Listening => {
                    symbol("mic.fill", NSColor::systemRedColor(), &hud.icon);
                    unsafe { hud.spinner.stopAnimation(None) };
                    hud.spinner.setHidden(true);
                    hud.levels.setHidden(false);
                }
                Mode::Working => {
                    hud.icon.setHidden(true);
                    hud.levels.setHidden(true);
                    hud.spinner.setHidden(false);
                    unsafe { hud.spinner.startAnimation(None) };
                }
                Mode::Notice => {
                    symbol(
                        "exclamationmark.triangle.fill",
                        NSColor::systemYellowColor(),
                        &hud.icon,
                    );
                    unsafe { hud.spinner.stopAnimation(None) };
                    hud.spinner.setHidden(true);
                    hud.levels.setHidden(true);
                }
            }
            if mode != Mode::Working {
                hud.icon.setHidden(false);
            }
            hud.label.setStringValue(&NSString::from_str(&text));
            hud.label.setHidden(text.is_empty());
            unsafe {
                let _: () =
                    msg_send![&*hud.card, setAccessibilityLabel: &*NSString::from_str(&text)];
            }
            place(hud, &app2);
            hud.panel.setAlphaValue(1.0);
            hud.panel.orderFrontRegardless();
        });
    });
}

/// `state` is overlay.rs's: recording, streaming, transcribing, processing.
pub fn show(app: &AppHandle, state: &str) {
    if let Ok(mut l) = LEVELS.lock() {
        l.clear();
    }
    match state {
        "recording" | "streaming" => present(app, Mode::Listening, "Listening".into()),
        "processing" => present(app, Mode::Working, "Polishing…".into()),
        _ => present(app, Mode::Working, "Transcribing…".into()),
    }
}

/// Live text for the Live style while listening.
pub fn live_text(app: &AppHandle, committed: &str, tentative: &str) {
    if settings::get_settings(app).overlay_style != OverlayStyle::Live {
        return;
    }
    let text = format!("{committed}{tentative}").trim().to_string();
    if text.is_empty() {
        return;
    }
    let app2 = app.clone();
    on_main(app, move |mtm| {
        with_hud(mtm, |hud| {
            if hud.mode == Mode::Listening && hud.panel.isVisible() {
                hud.label.setStringValue(&NSString::from_str(&text));
                place(hud, &app2);
            }
        });
    });
}

/// Shows a notice for a few seconds, then hides unless something newer came.
pub fn notice(app: &AppHandle, text: String) {
    present(app, Mode::Notice, text);
    let gen = GENERATION.load(Ordering::SeqCst);
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if GENERATION.load(Ordering::SeqCst) == gen {
            hide(&app);
        }
    });
}

pub fn hide(app: &AppHandle) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    on_main(app, |_| {
        HUD.with(|cell| {
            if let Some(hud) = cell.borrow().as_ref() {
                unsafe { hud.spinner.stopAnimation(None) };
                hud.panel.orderOut(None);
            }
        });
    });
}

/// Mic levels from the audio thread (already throttled to ~30 fps).
pub fn levels(app: &AppHandle, levels: &[f32]) {
    if let Ok(mut l) = LEVELS.lock() {
        l.clear();
        l.extend(levels.iter().take(BARS).copied());
    }
    if LEVELS_REDRAW_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    on_main(app, |_| {
        LEVELS_REDRAW_PENDING.store(false, Ordering::SeqCst);
        HUD.with(|cell| {
            if let Some(hud) = cell.borrow().as_ref() {
                if hud.mode == Mode::Listening {
                    hud.levels.setNeedsDisplay(true);
                }
            }
        });
    });
}
