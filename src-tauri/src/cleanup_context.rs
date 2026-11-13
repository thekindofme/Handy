//! Handy Bar: context for AI cleanup, so names and terms on screen come out
//! spelled right.
//!
//! - App hints (default on): the front app's name and its window title, read
//!   through Accessibility. A few milliseconds.
//! - Screen text (opt-in): the front window is captured with `screencapture`
//!   and read with Apple Vision's text recognition, on device.
//!
//! Both start when the cleanup shortcut is pressed, on a background thread,
//! so they run while the user speaks; cleanup waits for them only briefly.
//! Nothing here leaves the Mac except as part of the cleanup prompt the user
//! has chosen to send.

use log::{debug, warn};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Screen text is cut to this many characters (Ghost Pepper's limit too).
const MAX_SCREEN_TEXT: usize = 4000;
/// How long cleanup waits for a capture still in progress.
const WAIT_FOR_CAPTURE: Duration = Duration::from_millis(1500);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    pub app: Option<String>,
    pub window: Option<String>,
    pub screen_text: Option<String>,
}

impl Context {
    pub fn is_empty(&self) -> bool {
        self.app.is_none() && self.window.is_none() && self.screen_text.is_none()
    }

    /// The block added to the cleanup prompt.
    pub fn prompt_block(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut out = String::from(
            "\n\n<context>\nWhere the user is dictating. Use it only to spell names, \
             commands and terms correctly; never add it to your reply.\n",
        );
        match (&self.app, &self.window) {
            (Some(app), Some(window)) => out.push_str(&format!("App: {app} (window: {window})\n")),
            (Some(app), None) => out.push_str(&format!("App: {app}\n")),
            (None, Some(window)) => out.push_str(&format!("Window: {window}\n")),
            (None, None) => {}
        }
        if let Some(text) = &self.screen_text {
            out.push_str("Text on screen:\n");
            out.push_str(text);
            out.push('\n');
        }
        out.push_str("</context>");
        Some(out)
    }
}

static PENDING: Mutex<Option<(Instant, Receiver<Context>)>> = Mutex::new(None);

/// Starts gathering context for the dictation that's starting now.
pub fn start(app_hints: bool, screen_text: bool, custom_words: Vec<String>) {
    if !app_hints && !screen_text {
        if let Ok(mut p) = PENDING.lock() {
            *p = None;
        }
        return;
    }
    let (tx, rx) = channel();
    if let Ok(mut p) = PENDING.lock() {
        *p = Some((Instant::now(), rx));
    }
    std::thread::spawn(move || {
        let started = Instant::now();
        let ctx = imp::gather(app_hints, screen_text, &custom_words);
        debug!(
            "Cleanup context gathered in {:?} (app={}, window={}, screen text={} chars)",
            started.elapsed(),
            ctx.app.is_some(),
            ctx.window.is_some(),
            ctx.screen_text.as_ref().map_or(0, |t| t.len())
        );
        let _ = tx.send(ctx);
    });
}

/// The context gathered for this dictation, waiting briefly if it's still
/// being read. Empty when none was started (or it's stale).
pub fn take() -> Context {
    let pending = PENDING.lock().ok().and_then(|mut p| p.take());
    let Some((started, rx)) = pending else {
        return Context::default();
    };
    if started.elapsed() > Duration::from_secs(600) {
        return Context::default();
    }
    match rx.recv_timeout(WAIT_FOR_CAPTURE) {
        Ok(ctx) => ctx,
        Err(_) => {
            warn!("Cleanup context wasn't ready in time; cleaning up without it");
            Context::default()
        }
    }
}

/// Whether macOS lets Handy Bar capture other apps' windows.
pub fn screen_recording_allowed() -> bool {
    imp::screen_recording_allowed()
}

/// Asks macOS for Screen Recording access (shows the system prompt once).
pub fn request_screen_recording() -> bool {
    imp::request_screen_recording()
}

fn tidy(text: &str) -> String {
    let mut out = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > MAX_SCREEN_TEXT {
        out = out.chars().take(MAX_SCREEN_TEXT).collect();
    }
    out
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{tidy, Context};
    use log::debug;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{msg_send, ClassType};
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};
    use std::ffi::c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
        fn AXUIElementCopyAttributeValue(
            element: *const c_void,
            attribute: *const c_void,
            value: *mut *const c_void,
        ) -> i32;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *const c_void;
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }
    #[link(name = "Vision", kind = "framework")]
    extern "C" {}

    const ON_SCREEN_ONLY: u32 = 1 << 0;
    const EXCLUDE_DESKTOP: u32 = 1 << 4;

    pub fn screen_recording_allowed() -> bool {
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub fn request_screen_recording() -> bool {
        unsafe { CGRequestScreenCaptureAccess() }
    }

    pub fn gather(app_hints: bool, screen_text: bool, custom_words: &[String]) -> Context {
        let workspace = NSWorkspace::sharedWorkspace();
        let Some(front) = workspace.frontmostApplication() else {
            return Context::default();
        };
        let pid = front.processIdentifier();
        // Our own popover isn't where the user is dictating.
        if pid == std::process::id() as i32 {
            return Context::default();
        }
        let mut ctx = Context::default();
        if app_hints {
            ctx.app = front.localizedName().map(|n| n.to_string());
            ctx.window = window_title(pid).filter(|t| !t.trim().is_empty());
        }
        if screen_text && screen_recording_allowed() {
            ctx.screen_text = front_window_id(pid)
                .and_then(capture)
                .and_then(|path| {
                    let text = recognize(&path, custom_words);
                    let _ = std::fs::remove_file(&path);
                    text
                })
                .map(|t| tidy(&t))
                .filter(|t| !t.is_empty());
        }
        ctx
    }

    /// The focused window's title, through Accessibility.
    fn window_title(pid: i32) -> Option<String> {
        unsafe {
            let app = AXUIElementCreateApplication(pid);
            if app.is_null() {
                return None;
            }
            let focused = NSString::from_str("AXFocusedWindow");
            let mut window: *const c_void = std::ptr::null();
            let ok =
                AXUIElementCopyAttributeValue(app, Retained::as_ptr(&focused).cast(), &mut window);
            CFRelease(app);
            if ok != 0 || window.is_null() {
                return None;
            }
            let title_attr = NSString::from_str("AXTitle");
            let mut title: *const c_void = std::ptr::null();
            let ok = AXUIElementCopyAttributeValue(
                window,
                Retained::as_ptr(&title_attr).cast(),
                &mut title,
            );
            CFRelease(window);
            if ok != 0 || title.is_null() {
                return None;
            }
            // A CFString is an NSString; we own this copy.
            let title: Retained<AnyObject> = Retained::from_raw(title as *mut AnyObject)?;
            let is_string: bool = msg_send![&*title, isKindOfClass: NSString::class()];
            if !is_string {
                return None;
            }
            let s: &NSString = &*(Retained::as_ptr(&title) as *const NSString);
            Some(s.to_string())
        }
    }

    /// The front app's topmost normal window on screen.
    fn front_window_id(pid: i32) -> Option<u32> {
        unsafe {
            let list = CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0);
            let list: Retained<NSArray<NSDictionary<NSString, AnyObject>>> =
                Retained::from_raw(list as *mut _)?;
            let number = |d: &NSDictionary<NSString, AnyObject>, key: &str| -> Option<i64> {
                let v = d.objectForKey(&NSString::from_str(key))?;
                let n: &NSNumber = &*(Retained::as_ptr(&v) as *const NSNumber);
                Some(n.longLongValue())
            };
            list.iter().find_map(|d| {
                let owner = number(&d, "kCGWindowOwnerPID")?;
                let layer = number(&d, "kCGWindowLayer")?;
                (owner == i64::from(pid) && layer == 0)
                    .then(|| number(&d, "kCGWindowNumber"))
                    .flatten()
                    .map(|n| n as u32)
            })
        }
    }

    /// Captures one window to a temporary PNG.
    fn capture(window_id: u32) -> Option<std::path::PathBuf> {
        let path = std::env::temp_dir().join(format!(
            "handybar-context-{}-{window_id}.png",
            std::process::id()
        ));
        let status = std::process::Command::new("/usr/sbin/screencapture")
            .args(["-x", "-o", "-t", "png", &format!("-l{window_id}")])
            .arg(&path)
            .status()
            .ok()?;
        (status.success() && path.exists()).then_some(path)
    }

    /// Apple Vision text recognition, top to bottom.
    fn recognize(path: &std::path::Path, custom_words: &[String]) -> Option<String> {
        let started = std::time::Instant::now();
        unsafe {
            let handler_class = AnyClass::get(c"VNImageRequestHandler")?;
            let request_class = AnyClass::get(c"VNRecognizeTextRequest")?;
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            let options = NSDictionary::<NSString, AnyObject>::new();
            let handler: objc2::rc::Allocated<AnyObject> = msg_send![handler_class, alloc];
            let handler: Option<Retained<AnyObject>> =
                msg_send![handler, initWithURL: &*url, options: &*options];
            let handler = handler?;
            let request: Retained<AnyObject> = msg_send![request_class, new];
            let _: () = msg_send![&*request, setRecognitionLevel: 0isize]; // accurate
            let _: () = msg_send![&*request, setUsesLanguageCorrection: true];
            if !custom_words.is_empty() {
                let words: Vec<Retained<NSString>> =
                    custom_words.iter().map(|w| NSString::from_str(w)).collect();
                let words = NSArray::from_retained_slice(&words);
                let _: () = msg_send![&*request, setCustomWords: &*words];
            }
            let requests = NSArray::from_slice(&[&*request]);
            let ok: bool = msg_send![
                &*handler,
                performRequests: &*requests,
                error: std::ptr::null_mut::<*mut AnyObject>()
            ];
            if !ok {
                return None;
            }
            let results: Option<Retained<NSArray<AnyObject>>> = msg_send![&*request, results];
            let results = results?;
            // Vision's boxes are bottom-left based: sort top to bottom, then
            // left to right.
            let mut lines: Vec<(f64, f64, String)> = Vec::new();
            for obs in results.iter() {
                let candidates: Option<Retained<NSArray<AnyObject>>> =
                    msg_send![&*obs, topCandidates: 1usize];
                let Some(best) = candidates.and_then(|c| c.firstObject()) else {
                    continue;
                };
                let text: Retained<NSString> = msg_send![&*best, string];
                let bbox: objc2_foundation::NSRect = msg_send![&*obs, boundingBox];
                lines.push((
                    -(bbox.origin.y + bbox.size.height),
                    bbox.origin.x,
                    text.to_string(),
                ));
            }
            lines.sort_by(|a, b| {
                a.0.partial_cmp(&b.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            });
            debug!(
                "Screen text: {} lines read in {:?}",
                lines.len(),
                started.elapsed()
            );
            Some(
                lines
                    .into_iter()
                    .map(|(_, _, t)| t)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Context;
    pub fn gather(_: bool, _: bool, _: &[String]) -> Context {
        Context::default()
    }
    pub fn screen_recording_allowed() -> bool {
        false
    }
    pub fn request_screen_recording() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_block_says_what_it_is_for() {
        assert_eq!(Context::default().prompt_block(), None);
        let ctx = Context {
            app: Some("Slack".into()),
            window: Some("#design".into()),
            screen_text: Some("Anouk: ship the Kubernetes fix".into()),
        };
        let block = ctx.prompt_block().unwrap();
        assert!(block.contains("App: Slack (window: #design)"));
        assert!(block.contains("Kubernetes"));
        assert!(block.contains("never add it to your reply"));
    }

    #[test]
    fn screen_text_is_tidied_and_capped() {
        assert_eq!(tidy("a\n\n  b\tc"), "a b c");
        assert_eq!(tidy(&"x ".repeat(5000)).chars().count(), MAX_SCREEN_TEXT);
    }

    #[test]
    fn nothing_started_means_no_context() {
        if let Ok(mut p) = PENDING.lock() {
            *p = None;
        }
        assert!(take().is_empty());
    }
}
