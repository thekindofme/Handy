//! Draws a [`Screen`] with AppKit inside an `NSPopover` on the status item.
//!
//! Everything here runs on the main thread. Views are rebuilt from the
//! screen description on every refresh (a few dozen native views, well under
//! a millisecond), and the whole tree is dropped when the popover closes, so
//! an idle Handy holds no UI beyond the status item.

use super::view::{
    Action, Block, BoolKey, ButtonSpec, ChoiceKey, Control, Phase, Row, Screen, Tab,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::runtime::ProtocolObject;
use objc2::Message;
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSPopoverDelegate;
use objc2_app_kit::{
    NSApplication, NSBox, NSBoxType, NSButton, NSColor, NSControlSize, NSEvent, NSFont, NSImage,
    NSImageView, NSLayoutAttribute, NSLayoutConstraint, NSLineBreakMode, NSPopUpButton, NSPopover,
    NSPopoverBehavior, NSProgressIndicator, NSProgressIndicatorStyle, NSScrollView,
    NSSegmentSwitchTracking, NSSegmentedControl, NSStackView, NSStackViewDistribution, NSSwitch,
    NSTextField, NSTitlePosition, NSUserInterfaceLayoutOrientation, NSView, NSViewController,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSRectEdge,
    NSSize, NSString,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::AppHandle;

const WIDTH: f64 = 380.0;
const MAX_SCROLL: f64 = 470.0;
const PAD: f64 = 14.0;

/// What a control's tag points at.
enum Slot {
    Fixed(Action),
    Switch(BoolKey),
    Popup(ChoiceKey, Vec<String>),
    Tabs,
}

/// One tab's built views. Kept while the popover is open so switching back
/// to a tab is a swap, not a rebuild.
struct Tree {
    /// What it was built from; an equal screen means nothing to redraw.
    screen: Screen,
    outer: Retained<NSStackView>,
    /// The tab bar; a click leaves it showing the clicked tab, so it is
    /// reset whenever this tree comes back on screen.
    tabs: Retained<NSView>,
    scroll: Retained<NSScrollView>,
    progress: ProgressViews,
    slots: Vec<Slot>,
    /// Tags carry this so a control from a replaced tree can't fire.
    generation: isize,
    height: f64,
}

struct Ui {
    app: AppHandle,
    popover: Retained<NSPopover>,
    root: Retained<NSView>,
    target: Retained<Target>,
    trees: HashMap<Tab, Tree>,
    /// The tab on screen.
    shown: Option<Tab>,
    next_generation: isize,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    /// When the popover last closed. A click on the menu bar icon closes a
    /// transient popover before our click handler runs; without this the same
    /// click would open it again.
    static LAST_CLOSED: Cell<Option<Instant>> = const { Cell::new(None) };
}

const GEN_STRIDE: isize = 100_000;

/// Whether the popover is open; read from any thread so closed-popover
/// refreshes cost nothing.
static OPEN: AtomicBool = AtomicBool::new(false);

pub fn is_open() -> bool {
    OPEN.load(Ordering::SeqCst)
}

define_class!(
    /// The single target every control sends `perform:` to.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "HandyMenubarTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(perform:))]
        fn perform(&self, sender: &NSView) {
            on_control(sender);
        }
    }

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSPopoverDelegate for Target {
        #[unsafe(method(popoverDidClose:))]
        fn popover_did_close(&self, _notification: &NSNotification) {
            LAST_CLOSED.with(|c| c.set(Some(Instant::now())));
            OPEN.store(false, Ordering::SeqCst);
            // Frees the views on a later turn, once AppKit is done with them.
            if let Some(app) = UI.with(|cell| cell.try_borrow().ok().and_then(|ui| ui.as_ref().map(|u| u.app.clone()))) {
                std::thread::spawn(move || {
                    let _ = app.run_on_main_thread(|| {
                        if !OPEN.load(Ordering::SeqCst) {
                            release_views();
                        }
                    });
                });
            }
        }
    }
);

define_class!(
    /// Scroll document view that lays out from the top.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "HandyMenubarFlippedView"]
    struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

fn on_control(sender: &NSView) {
    let tag: isize = unsafe { msg_send![sender, tag] };
    let action = UI.with(|cell| {
        let ui = cell.borrow();
        let ui = ui.as_ref()?;
        let tree = ui.trees.get(&ui.shown?)?;
        if tag / GEN_STRIDE != tree.generation {
            return None;
        }
        let slot = tree.slots.get((tag % GEN_STRIDE) as usize)?;
        let action = match slot {
            Slot::Fixed(a) => a.clone(),
            Slot::Switch(key) => {
                let state: isize = unsafe { msg_send![sender, state] };
                Action::SetBool(*key, state != 0)
            }
            Slot::Popup(key, values) => {
                let index: isize = unsafe { msg_send![sender, indexOfSelectedItem] };
                Action::SetChoice(*key, values.get(index as usize)?.clone())
            }
            Slot::Tabs => {
                let index: isize = unsafe { msg_send![sender, selectedSegment] };
                Action::SelectTab(Tab::from_index(index))
            }
        };
        Some((ui.app.clone(), action))
    });
    if let Some((app, action)) = action {
        super::perform(&app, action);
    }
}

// ---------------------------------------------------------------------------
// Popover lifecycle
// ---------------------------------------------------------------------------

fn status_button(app: &AppHandle, mtm: MainThreadMarker) -> Option<Retained<NSView>> {
    use tauri::Manager;
    let tray = app.try_state::<tauri::tray::TrayIcon>()?;
    // The closure runs on this (main) thread; the pointer only crosses the
    // Send bound Tauri puts on it.
    let raw = tray
        .with_inner_tray_icon(|inner| {
            inner
                .ns_status_item()
                .map(|i| Retained::into_raw(i) as usize)
        })
        .ok()
        .flatten()?;
    let item = unsafe { Retained::from_raw(raw as *mut objc2_app_kit::NSStatusItem) }?;
    item.button(mtm).map(|b| upcast(&*b))
}

fn ensure_ui(app: &AppHandle, mtm: MainThreadMarker) {
    UI.with(|cell| {
        if cell.borrow().is_some() {
            return;
        }
        let popover = NSPopover::new(mtm);
        popover.setBehavior(NSPopoverBehavior::Transient);
        popover.setAnimates(true);
        let root = NSView::new(mtm);
        // Once: the root outlives every rebuild.
        activate(&[root.widthAnchor().constraintEqualToConstant(WIDTH)]);
        let vc = NSViewController::new(mtm);
        vc.setView(&root);
        popover.setContentViewController(Some(&vc));
        let target: Retained<Target> = unsafe { msg_send![Target::alloc(mtm), init] };
        popover.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        *cell.borrow_mut() = Some(Ui {
            app: app.clone(),
            popover,
            root,
            target,
            trees: HashMap::new(),
            shown: None,
            next_generation: 1,
        });
    });
}

fn is_shown() -> bool {
    UI.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|ui| ui.popover.isShown())
    })
}

pub fn toggle(app: &AppHandle) {
    let since_close = LAST_CLOSED.with(|c| c.get()).map(|t| t.elapsed());
    let just_closed = since_close.is_some_and(|t| t < Duration::from_millis(400));
    let shown = is_shown();
    let visible = popover_window().is_some_and(|w| w.isVisible());
    let app_ns = MainThreadMarker::new().map(NSApplication::sharedApplication);
    log::debug!(
        "Menu bar click: shown={shown} visible={visible} since_close={since_close:?} \
         app_hidden={:?} app_active={:?}",
        app_ns.as_ref().map(|a| a.isHidden()),
        app_ns.as_ref().map(|a| a.isActive()),
    );
    if shown && visible {
        close();
    } else if shown {
        // AppKit says the popover is open but nothing is on screen (seen
        // under heavy load, and when the app was hidden). The click is
        // meant to open it, so reopen instead of "closing" an invisible one.
        log::info!("Menu bar: popover was open but not on screen; reopening");
        if let Some(p) = popover() {
            // Without animation, so it's fully closed before we reopen.
            p.setAnimates(false);
            p.close();
            p.setAnimates(true);
        }
        show(app);
    } else if !just_closed {
        show(app);
    }
}

fn popover_window() -> Option<Retained<objc2_app_kit::NSWindow>> {
    popover()
        .and_then(|p| p.contentViewController())
        .and_then(|vc| vc.view().window())
}

fn popover() -> Option<Retained<NSPopover>> {
    UI.with(|cell| cell.borrow().as_ref().map(|ui| ui.popover.clone()))
}

pub fn show(app: &AppHandle) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    ensure_ui(app, mtm);
    let Some(button) = status_button(app, mtm) else {
        log::warn!("Menu bar: no status item to anchor the popover to");
        return;
    };
    super::refresh_devices(app);
    rebuild(app, mtm);
    let app_ns = NSApplication::sharedApplication(mtm);
    // A dictation started from the popover hides the app to hand focus back;
    // a hidden app's popover is "shown" but never drawn.
    if app_ns.isHidden() {
        app_ns.unhideWithoutActivation();
    }
    #[allow(deprecated)]
    app_ns.activateIgnoringOtherApps(true);
    // Called outside the UI borrow: showing and closing call our delegate.
    if let Some(popover) = popover() {
        if !popover.isShown() {
            popover.showRelativeToRect_ofView_preferredEdge(
                button.bounds(),
                &button,
                NSRectEdge::MinY,
            );
        }
        if let Some(window) = popover_window() {
            // macOS may refuse to activate us (cooperative activation), which
            // can leave the popover's window ordered out. Put it up anyway.
            window.orderFrontRegardless();
            window.makeKeyWindow();
        }
        OPEN.store(popover.isShown(), Ordering::SeqCst);
    }
    prebuild_other_tabs(app);
}

/// Views are released by the delegate once the close animation is done, so
/// the popover doesn't blank while it fades.
fn close() {
    if let Some(popover) = popover() {
        popover.close();
    }
}

/// Drops the view tree so a closed popover holds no memory beyond itself.
fn release_views() {
    UI.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return;
        };
        if let Some(ui) = guard.as_mut() {
            for sub in ui.root.subviews().iter() {
                sub.removeFromSuperview();
            }
            ui.trees.clear();
            ui.shown = None;
        }
    });
}

/// Closes the popover and gives focus back to the app the user came from,
/// so a dictation started here pastes into it.
pub fn close_and_yield(_app: &AppHandle) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    close();
    NSApplication::sharedApplication(mtm).hide(None);
}

/// Redraws if the popover is open; drops stale views if it closed by itself.
pub fn refresh(app: &AppHandle) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if !is_shown() {
        return;
    }
    if interacting() {
        // A click or open menu is in progress; replacing its control now
        // would drop the action. Try again shortly.
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            super::request_refresh(&app);
        });
        return;
    }
    rebuild(app, mtm);
}

/// A mouse button is down, or AppKit is tracking a menu or control.
fn interacting() -> bool {
    if NSEvent::pressedMouseButtons() != 0 {
        return true;
    }
    let mode = objc2_foundation::NSRunLoop::currentRunLoop().currentMode();
    let tracking = unsafe { objc2_app_kit::NSEventTrackingRunLoopMode };
    mode.is_some_and(|m| m.isEqualToString(tracking))
}

// ---------------------------------------------------------------------------
// Building views
// ---------------------------------------------------------------------------

fn upcast<T: AsRef<NSView> + ?Sized>(x: &T) -> Retained<NSView> {
    x.as_ref().retain()
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn set_identifier(view: &NSView, id: &str) {
    let id = ns(id);
    let _: () = unsafe { msg_send![view, setAccessibilityIdentifier: &*id] };
}

fn set_label(view: &NSView, label: &str) {
    let label = ns(label);
    let _: () = unsafe { msg_send![view, setAccessibilityLabel: &*label] };
}

fn no_autoresize(view: &NSView) {
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
}

fn activate(constraints: &[Retained<NSLayoutConstraint>]) {
    let refs: Vec<&NSLayoutConstraint> = constraints.iter().map(|c| &**c).collect();
    NSLayoutConstraint::activateConstraints(&NSArray::from_slice(&refs));
}

fn pin_horizontal(child: &NSView, parent: &NSView, inset: f64) {
    activate(&[
        child
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&parent.leadingAnchor(), inset),
        child
            .trailingAnchor()
            .constraintEqualToAnchor_constant(&parent.trailingAnchor(), -inset),
    ]);
}

fn pin_all(child: &NSView, parent: &NSView) {
    pin_horizontal(child, parent, 0.0);
    activate(&[
        child
            .topAnchor()
            .constraintEqualToAnchor(&parent.topAnchor()),
        child
            .bottomAnchor()
            .constraintEqualToAnchor(&parent.bottomAnchor()),
    ]);
}

fn stack(
    mtm: MainThreadMarker,
    vertical: bool,
    spacing: f64,
    views: &[Retained<NSView>],
) -> Retained<NSStackView> {
    let refs: Vec<&NSView> = views.iter().map(|v| &**v).collect();
    let s = NSStackView::stackViewWithViews(&NSArray::from_slice(&refs), mtm);
    s.setOrientation(if vertical {
        NSUserInterfaceLayoutOrientation::Vertical
    } else {
        NSUserInterfaceLayoutOrientation::Horizontal
    });
    s.setSpacing(spacing);
    if vertical {
        s.setAlignment(NSLayoutAttribute::Leading);
    } else {
        s.setAlignment(NSLayoutAttribute::CenterY);
        s.setDistribution(NSStackViewDistribution::Fill);
    }
    no_autoresize(&s);
    s
}

/// A vertical stack whose children span its full width.
fn column(
    mtm: MainThreadMarker,
    spacing: f64,
    inset: f64,
    views: &[Retained<NSView>],
) -> Retained<NSStackView> {
    let s = stack(mtm, true, spacing, views);
    for v in views {
        pin_horizontal(v, &s, inset);
    }
    s
}

fn percent(fraction: f64) -> String {
    format!("{:.0}%", fraction * 100.0)
}

fn label(
    mtm: MainThreadMarker,
    text: &str,
    size: f64,
    bold: bool,
    secondary: bool,
) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&ns(text), mtm);
    let font = if bold {
        NSFont::boldSystemFontOfSize(size)
    } else {
        NSFont::systemFontOfSize(size)
    };
    l.setFont(Some(&font));
    if secondary {
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    l.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    l.setContentCompressionResistancePriority_forOrientation(
        250.0,
        objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
    );
    no_autoresize(&l);
    l
}

fn wrapping(
    mtm: MainThreadMarker,
    text: &str,
    size: f64,
    secondary: bool,
) -> Retained<NSTextField> {
    let l = NSTextField::wrappingLabelWithString(&ns(text), mtm);
    l.setFont(Some(&NSFont::systemFontOfSize(size)));
    if secondary {
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    l.setPreferredMaxLayoutWidth(WIDTH - 2.0 * PAD - 24.0);
    l.setSelectable(false);
    no_autoresize(&l);
    l
}

fn symbol(
    mtm: MainThreadMarker,
    name: &str,
    tint: Option<Retained<NSColor>>,
    size: f64,
) -> Retained<NSView> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(name), None);
    let view = match image {
        Some(img) => NSImageView::imageViewWithImage(&img, mtm),
        None => NSImageView::new(mtm),
    };
    if let Some(t) = tint {
        view.setContentTintColor(Some(&t));
    }
    no_autoresize(&view);
    activate(&[
        view.widthAnchor().constraintEqualToConstant(size),
        view.heightAnchor().constraintEqualToConstant(size),
    ]);
    upcast(&*view)
}

fn spacer(mtm: MainThreadMarker) -> Retained<NSView> {
    let v = NSView::new(mtm);
    no_autoresize(&v);
    v.setContentHuggingPriority_forOrientation(
        1.0,
        objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
    );
    v
}

struct Builder<'a> {
    mtm: MainThreadMarker,
    target: &'a Target,
    slots: Vec<Slot>,
    generation: isize,
    progress: ProgressViews,
}

/// Download bars and their percentage labels, by row id.
type ProgressViews = HashMap<String, (Retained<NSProgressIndicator>, Retained<NSTextField>)>;

impl Builder<'_> {
    fn tag(&mut self, slot: Slot) -> isize {
        self.slots.push(slot);
        self.generation * GEN_STRIDE + (self.slots.len() as isize - 1)
    }

    fn wire(&mut self, control: &NSView, slot: Slot) {
        let tag = self.tag(slot);
        let target: &AnyObject = self.target.as_ref();
        unsafe {
            let _: () = msg_send![control, setTag: tag];
            let _: () = msg_send![control, setTarget: target];
            let _: () = msg_send![control, setAction: sel!(perform:)];
        }
    }

    fn button(&mut self, spec: &ButtonSpec) -> Retained<NSView> {
        let b = NSButton::new(self.mtm);
        b.setTitle(&ns(&spec.title));
        b.setBezelStyle(objc2_app_kit::NSBezelStyle::Push);
        b.setControlSize(NSControlSize::Small);
        b.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        if spec.prominent {
            b.setBezelColor(Some(&NSColor::controlAccentColor()));
        }
        if matches!(spec.action, Action::DeleteEntry(_) | Action::DeleteModel(_)) {
            b.setHasDestructiveAction(true);
        }
        if matches!(spec.action, Action::Quit) {
            b.setKeyEquivalent(&ns("q"));
            b.setKeyEquivalentModifierMask(objc2_app_kit::NSEventModifierFlags::Command);
        }
        no_autoresize(&b);
        b.setContentHuggingPriority_forOrientation(
            750.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );
        b.setContentCompressionResistancePriority_forOrientation(
            760.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );
        let v: Retained<NSView> = upcast(&*b);
        self.wire(&v, Slot::Fixed(spec.action.clone()));
        set_identifier(&v, &spec.id);
        v
    }

    fn control(&mut self, row: &Row) -> Option<Retained<NSView>> {
        let mtm = self.mtm;
        let view: Retained<NSView> = match &row.control {
            Control::None => return None,
            Control::Switch { key, on } => {
                let s = NSSwitch::new(mtm);
                s.setState(if *on { 1 } else { 0 });
                s.setControlSize(NSControlSize::Small);
                no_autoresize(&s);
                let v: Retained<NSView> = upcast(&*s);
                self.wire(&v, Slot::Switch(*key));
                v
            }
            Control::Popup {
                key,
                options,
                selected,
            } => {
                let p = NSPopUpButton::new(mtm);
                p.setPullsDown(false);
                p.setControlSize(NSControlSize::Small);
                p.setFont(Some(&NSFont::systemFontOfSize(12.0)));
                if let Some(menu) = p.menu() {
                    for o in options {
                        let item = objc2_app_kit::NSMenuItem::new(mtm);
                        item.setTitle(&ns(&o.label));
                        menu.addItem(&item);
                    }
                }
                p.selectItemAtIndex(*selected as isize);
                no_autoresize(&p);
                p.setContentCompressionResistancePriority_forOrientation(
                    760.0,
                    objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
                );
                activate(&[p.widthAnchor().constraintLessThanOrEqualToConstant(190.0)]);
                let v: Retained<NSView> = upcast(&*p);
                self.wire(
                    &v,
                    Slot::Popup(*key, options.iter().map(|o| o.value.clone()).collect()),
                );
                v
            }
            Control::Buttons(buttons) => {
                let views: Vec<_> = buttons.iter().map(|b| self.button(b)).collect();
                if views.len() == 1 {
                    let v = views.into_iter().next()?;
                    set_label(&v, &format!("{} {}", row.title, buttons[0].title));
                    return Some(v);
                }
                upcast(&*stack(mtm, false, 6.0, &views))
            }
            Control::Progress { fraction, cancel } => {
                let bar = NSProgressIndicator::new(mtm);
                bar.setStyle(NSProgressIndicatorStyle::Bar);
                bar.setIndeterminate(false);
                bar.setMinValue(0.0);
                bar.setMaxValue(1.0);
                bar.setDoubleValue(*fraction);
                no_autoresize(&bar);
                activate(&[bar.widthAnchor().constraintEqualToConstant(80.0)]);
                let pct_label = label(mtm, &percent(*fraction), 11.0, false, true);
                self.progress
                    .insert(row.id.clone(), (bar.clone(), pct_label.clone()));
                let bar: Retained<NSView> = upcast(&*bar);
                set_identifier(&bar, &format!("{}.progress", row.id));
                let pct: Retained<NSView> = upcast(&*pct_label);
                let cancel = self.button(cancel);
                upcast(&*stack(mtm, false, 6.0, &[bar, pct, cancel]))
            }
            Control::Done(text) => {
                let check = symbol(
                    mtm,
                    "checkmark.circle.fill",
                    Some(NSColor::systemGreenColor()),
                    14.0,
                );
                let l: Retained<NSView> = upcast(&*label(mtm, text, 12.0, false, true));
                // Stack views aren't accessibility elements; tag the label.
                set_identifier(&l, &row.id);
                let s = stack(mtm, false, 4.0, &[check, l]);
                upcast(&*s)
            }
        };
        if !matches!(row.control, Control::Buttons(_)) {
            set_identifier(&view, &row.id);
            set_label(&view, &row.title);
        }
        Some(view)
    }

    fn row(&mut self, row: &Row) -> Retained<NSView> {
        let mtm = self.mtm;
        let title = label(mtm, &row.title, 13.0, false, false);
        let mut text: Vec<Retained<NSView>> = vec![upcast(&*title)];
        if let Some(detail) = &row.detail {
            let d = label(mtm, detail, 11.0, false, true);
            set_identifier(&d, &format!("{}.detail", row.id));
            text.push(upcast(&*d));
        }
        let texts = stack(mtm, true, 1.0, &text);
        texts.setContentHuggingPriority_forOrientation(
            1.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );
        texts.setContentCompressionResistancePriority_forOrientation(
            250.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );
        let mut parts: Vec<Retained<NSView>> = vec![upcast(&*texts)];
        if let Some(c) = self.control(row) {
            parts.push(c);
        }
        let h = stack(mtm, false, 8.0, &parts);
        h.setEdgeInsets(NSEdgeInsets {
            top: 7.0,
            left: 10.0,
            bottom: 7.0,
            right: 10.0,
        });
        set_identifier(&h, &format!("{}.row", row.id));
        upcast(&*h)
    }

    fn group(&mut self, title: Option<&str>, rows: &[Row]) -> Retained<NSView> {
        let mtm = self.mtm;
        let mut items: Vec<Retained<NSView>> = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            if i > 0 {
                let sep = NSBox::new(mtm);
                sep.setBoxType(NSBoxType::Separator);
                no_autoresize(&sep);
                items.push(upcast(&*sep));
            }
            items.push(self.row(r));
        }
        let body = column(mtm, 0.0, 0.0, &items);
        let card = NSBox::new(mtm);
        card.setBoxType(NSBoxType::Custom);
        card.setTitlePosition(NSTitlePosition::NoTitle);
        card.setCornerRadius(8.0);
        card.setBorderWidth(0.5);
        card.setBorderColor(&NSColor::separatorColor());
        card.setFillColor(&NSColor::controlBackgroundColor());
        card.setContentViewMargins(NSSize::new(0.0, 0.0));
        card.setContentView(Some(&body));
        no_autoresize(&card);
        // NSBox doesn't size an Auto Layout content view by itself.
        pin_all(&body, &card);
        let card: Retained<NSView> = upcast(&*card);
        match title {
            Some(t) => {
                let heading = label(mtm, &t.to_uppercase(), 10.5, true, true);
                let heading: Retained<NSView> = upcast(&*heading);
                upcast(&*column(mtm, 4.0, 0.0, &[heading, card]))
            }
            None => card,
        }
    }

    fn block(&mut self, block: &Block) -> Retained<NSView> {
        let mtm = self.mtm;
        match block {
            Block::Group { title, rows } => self.group(title.as_deref(), rows),
            Block::Note(text) => {
                let l = wrapping(mtm, text, 12.0, true);
                set_identifier(&l, "note");
                upcast(&*l)
            }
            Block::Alert(text) => {
                let icon = symbol(
                    mtm,
                    "exclamationmark.triangle.fill",
                    Some(NSColor::systemOrangeColor()),
                    14.0,
                );
                let l = wrapping(mtm, text, 12.0, false);
                set_identifier(&l, "alert");
                let l: Retained<NSView> = upcast(&*l);
                upcast(&*stack(mtm, false, 6.0, &[icon, l]))
            }
        }
    }

    fn header(&mut self, screen: &Screen) -> Retained<NSView> {
        let mtm = self.mtm;
        let (icon, tint) = match screen.header.phase {
            Phase::Recording => ("mic.fill", NSColor::systemRedColor()),
            Phase::Transcribing => ("waveform", NSColor::controlAccentColor()),
            Phase::Idle if screen.header.buttons.is_empty() => {
                ("exclamationmark.circle.fill", NSColor::systemOrangeColor())
            }
            Phase::Idle => ("hand.raised.fill", NSColor::labelColor()),
        };
        let icon = symbol(mtm, icon, Some(tint), 26.0);
        let title = label(mtm, &screen.header.title, 14.0, true, false);
        set_identifier(&title, "header.title");
        let subtitle = label(mtm, &screen.header.subtitle, 11.5, false, true);
        set_identifier(&subtitle, "header.subtitle");
        let texts = stack(mtm, true, 1.0, &[upcast(&*title), upcast(&*subtitle)]);
        texts.setContentHuggingPriority_forOrientation(
            1.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );
        let mut parts: Vec<Retained<NSView>> = vec![icon, upcast(&*texts)];
        for b in &screen.header.buttons {
            let v = self.button(b);
            parts.push(v);
        }
        upcast(&*stack(mtm, false, 10.0, &parts))
    }

    fn tabs(&mut self, screen: &Screen) -> Retained<NSView> {
        let mtm = self.mtm;
        let labels: Vec<Retained<NSString>> = Tab::ALL.iter().map(|t| ns(t.title())).collect();
        let refs: Vec<&NSString> = labels.iter().map(|l| &**l).collect();
        let seg = unsafe {
            NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &NSArray::from_slice(&refs),
                NSSegmentSwitchTracking::SelectOne,
                None,
                None,
                mtm,
            )
        };
        seg.setSelectedSegment(screen.tab.index() as isize);
        seg.setControlSize(NSControlSize::Small);
        no_autoresize(&seg);
        let v: Retained<NSView> = upcast(&*seg);
        self.wire(&v, Slot::Tabs);
        set_identifier(&v, "tabs");
        set_label(&v, "Section");
        v
    }
}

fn rebuild(app: &AppHandle, mtm: MainThreadMarker) {
    let tab = super::current_tab(app);
    prepare(app, mtm, tab, true);
}

/// Builds the other tabs one per main-loop turn after the popover opens, so
/// the first switch to each is instant without delaying the open.
pub fn prebuild_other_tabs(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        for tab in [Tab::Home, Tab::History, Tab::Models, Tab::Settings] {
            std::thread::sleep(Duration::from_millis(30));
            let app2 = app.clone();
            let _ = app.run_on_main_thread(move || {
                let Some(mtm) = MainThreadMarker::new() else {
                    return;
                };
                if is_shown() && super::current_tab(&app2) != tab {
                    prepare(&app2, mtm, tab, false);
                }
            });
        }
    });
}

/// Makes sure `tab`'s views match the current state, rebuilding only if they
/// don't, and puts them on screen when `show` is set.
fn prepare(app: &AppHandle, mtm: MainThreadMarker, tab: Tab, show: bool) {
    let started = Instant::now();
    let snapshot = super::snapshot_for(app, Some(tab));
    let snapshot_took = started.elapsed();
    let screen = super::view::build(&snapshot, tab);

    UI.with(|cell| {
        let mut guard = cell.borrow_mut();
        let Some(ui) = guard.as_mut() else { return };
        let fresh = ui.trees.get(&tab).is_some_and(|t| t.screen == screen);
        let progress_only = !fresh
            && ui
                .trees
                .get(&tab)
                .is_some_and(|t| super::view::same_but_progress(&t.screen, &screen));
        if progress_only {
            // Downloads tick several times a second: move the bars, keep
            // the views (and whatever the user is pointing at).
            if let Some(tree) = ui.trees.get_mut(&tab) {
                for (id, fraction) in super::view::progress_of(&screen) {
                    if let Some((bar, pct)) = tree.progress.get(&id) {
                        bar.setDoubleValue(fraction);
                        pct.setStringValue(&ns(&percent(fraction)));
                    }
                }
                tree.screen = screen;
            }
        } else if !fresh {
            // Keep the scroll position when redrawing the tab on screen.
            let offset = match (ui.shown == Some(tab), ui.trees.get(&tab)) {
                (true, Some(old)) => old.scroll.contentView().bounds().origin.y,
                _ => 0.0,
            };
            let generation = ui.next_generation;
            ui.next_generation += 1;
            let tree = build_tree(mtm, &ui.target, screen, generation);
            let on_screen = ui.shown == Some(tab);
            ui.trees.insert(tab, tree);
            if on_screen {
                ui.shown = None; // force the swap below
                swap_in(ui, tab);
                if offset > 0.0 {
                    if let Some(doc) = ui.trees[&tab].scroll.documentView() {
                        doc.scrollPoint(NSPoint::new(0.0, offset));
                    }
                }
            }
            log::debug!(
                "Menu bar built {tab:?} in {:?} ({:?} reading state)",
                started.elapsed(),
                snapshot_took
            );
        }
        if show && ui.shown != Some(tab) {
            swap_in(ui, tab);
        }
    });
}

/// Puts a built tab on screen.
fn swap_in(ui: &mut Ui, tab: Tab) {
    let Some(tree) = ui.trees.get(&tab) else {
        return;
    };
    let _: () = unsafe { msg_send![&*tree.tabs, setSelectedSegment: tab.index() as isize] };
    for sub in ui.root.subviews().iter() {
        sub.removeFromSuperview();
    }
    ui.root.addSubview(&tree.outer);
    pin_all(&tree.outer, &ui.root);
    ui.popover.setContentSize(NSSize::new(WIDTH, tree.height));
    ui.root.layoutSubtreeIfNeeded();
    ui.shown = Some(tab);
}

fn build_tree(mtm: MainThreadMarker, target: &Target, screen: Screen, generation: isize) -> Tree {
    let mut b = Builder {
        mtm,
        target,
        slots: Vec::new(),
        generation,
        progress: HashMap::new(),
    };

    let header = b.header(&screen);
    let tabs = b.tabs(&screen);
    let blocks: Vec<Retained<NSView>> = screen.blocks.iter().map(|bl| b.block(bl)).collect();
    let content = column(mtm, 12.0, PAD, &blocks);
    content.setEdgeInsets(NSEdgeInsets {
        top: 2.0,
        left: 0.0,
        bottom: 12.0,
        right: 0.0,
    });
    let footer_label = label(mtm, &screen.footer, 11.0, false, true);
    set_identifier(&footer_label, "footer.version");
    let quit = b.button(&ButtonSpec {
        id: "footer.quit".into(),
        title: "Quit Handy Bar".into(),
        action: Action::Quit,
        prominent: false,
    });
    let footer = stack(
        mtm,
        false,
        8.0,
        &[upcast(&*footer_label), spacer(mtm), quit],
    );
    let slots = std::mem::take(&mut b.slots);
    let progress = std::mem::take(&mut b.progress);

    // Scrollable middle: a flipped document view holding the blocks.
    let doc: Retained<FlippedView> =
        unsafe { msg_send![FlippedView::alloc(mtm), initWithFrame: NSRect::ZERO] };
    no_autoresize(&doc);
    doc.addSubview(&content);
    pin_all(&content, &doc);
    let scroll = NSScrollView::new(mtm);
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setDrawsBackground(false);
    no_autoresize(&scroll);
    scroll.setDocumentView(Some(&doc));
    let clip = scroll.contentView();
    activate(&[
        doc.leadingAnchor()
            .constraintEqualToAnchor(&clip.leadingAnchor()),
        doc.trailingAnchor()
            .constraintEqualToAnchor(&clip.trailingAnchor()),
        doc.topAnchor().constraintEqualToAnchor(&clip.topAnchor()),
    ]);
    set_identifier(&scroll, "content");

    // Measure the blocks at the popover's width to size the scroll area.
    let measure = content.widthAnchor().constraintEqualToConstant(WIDTH);
    measure.setActive(true);
    let content_height = content.fittingSize().height;
    measure.setActive(false);
    let scroll_height = content_height.min(MAX_SCROLL);
    activate(&[scroll
        .heightAnchor()
        .constraintEqualToConstant(scroll_height)]);

    let scroll_view: Retained<NSView> = upcast(&*scroll.clone());
    let outer = column(
        mtm,
        10.0,
        0.0,
        &[
            header.clone(),
            tabs.clone(),
            scroll_view,
            upcast(&*footer.clone()),
        ],
    );
    // Header, tabs and footer get side padding; the scroll area runs edge
    // to edge so its scroller sits at the popover's edge.
    outer.setEdgeInsets(NSEdgeInsets {
        top: PAD,
        left: 0.0,
        bottom: 10.0,
        right: 0.0,
    });
    re_pad(&outer, &[&header, &tabs, &upcast(&*footer)]);
    // Measured on its own at the popover's width, before it is on screen.
    let width = outer.widthAnchor().constraintEqualToConstant(WIDTH);
    width.setActive(true);
    let height = outer.fittingSize().height;
    width.setActive(false);

    Tree {
        screen,
        outer,
        tabs,
        progress,
        scroll,
        slots,
        generation,
        height,
    }
}

/// Swaps the zero-inset width constraints `column` put on `views` for ones
/// with the popover's side padding.
fn re_pad(outer: &NSStackView, views: &[&NSView]) {
    for c in outer.constraints().iter() {
        let first: Option<Retained<AnyObject>> = unsafe { msg_send![&*c, firstItem] };
        let Some(first) = first else { continue };
        if views.iter().any(|v| {
            std::ptr::eq(
                Retained::as_ptr(&first).cast::<NSView>(),
                *v as *const NSView,
            )
        }) {
            let attr = c.firstAttribute();
            if attr == NSLayoutAttribute::Leading {
                c.setConstant(PAD);
            } else if attr == NSLayoutAttribute::Trailing {
                c.setConstant(-PAD);
            }
        }
    }
}
