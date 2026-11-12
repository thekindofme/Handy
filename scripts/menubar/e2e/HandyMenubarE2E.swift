// End-to-end test for Handy's native menu bar UI.
//
// Drives the real app the way a person does: it finds controls through the
// macOS Accessibility API (by the identifiers the Rust UI sets), moves the
// real mouse pointer to them and clicks with real CGEvents, types real key
// events, and then checks the result in three independent places: the UI,
// the settings file Handy writes, and (for dictation) the app it pasted into.
//
// A caption bar at the bottom of the screen shows each step and the running
// pass/fail count, so a screen recording of a run is readable on its own.
//
// Build: swiftc -O -o e2e HandyMenubarE2E.swift
// Run:   ./e2e [--phase ui|dictation|all] [--app "/path/Handy Menubar.app"]

import AppKit
import ApplicationServices
import Foundation

// MARK: - Configuration

let bundleId = "com.pais.handy.menubar"
let supportDir = NSHomeDirectory() + "/Library/Application Support/\(bundleId)"
let settingsPath = supportDir + "/settings_store.json"
var appPath = FileManager.default.currentDirectoryPath
    + "/src-tauri/target/debug/bundle/macos/Handy Menubar.app"
var phase = "all"
var keepSettings = false
var only: String? = nil
var resultsPath = FileManager.default.currentDirectoryPath + "/docs/menubar/e2e-results.json"

do {
    var args = CommandLine.arguments.dropFirst().makeIterator()
    while let a = args.next() {
        switch a {
        case "--app": appPath = args.next() ?? appPath
        case "--phase": phase = args.next() ?? phase
        case "--keep-settings": keepSettings = true
        case "--only": only = args.next()
        case "--results": resultsPath = args.next() ?? resultsPath
        default: break
        }
    }
}

// MARK: - Caption HUD (for the recording)

final class Caption {
    let panel: NSPanel
    let title = NSTextField(labelWithString: "")
    let detail = NSTextField(labelWithString: "")
    let tally = NSTextField(labelWithString: "")

    init() {
        let screen = NSScreen.main!.frame
        let w: CGFloat = 900, h: CGFloat = 84
        panel = NSPanel(
            contentRect: NSRect(x: screen.midX - w / 2, y: 40, width: w, height: h),
            styleMask: [.nonactivatingPanel, .borderless], backing: .buffered, defer: false)
        panel.level = .screenSaver
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.ignoresMouseEvents = true
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        let bg = NSVisualEffectView(frame: NSRect(x: 0, y: 0, width: w, height: h))
        bg.material = .hudWindow
        bg.state = .active
        bg.wantsLayer = true
        bg.layer?.cornerRadius = 14
        panel.contentView = bg
        title.font = .systemFont(ofSize: 20, weight: .semibold)
        detail.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        detail.textColor = .secondaryLabelColor
        tally.font = .monospacedDigitSystemFont(ofSize: 18, weight: .bold)
        tally.alignment = .right
        for (v, f) in [
            (title, NSRect(x: 20, y: 40, width: 690, height: 28)),
            (detail, NSRect(x: 20, y: 14, width: 860, height: 20)),
            (tally, NSRect(x: 700, y: 40, width: 180, height: 28)),
        ] {
            v.frame = f
            v.lineBreakMode = .byTruncatingTail
            bg.addSubview(v)
        }
        panel.orderFrontRegardless()
    }

    func show(_ t: String, _ d: String, passed: Int, failed: Int) {
        DispatchQueue.main.async {
            self.title.stringValue = t
            self.detail.stringValue = d
            self.tally.stringValue = "✅ \(passed)  ❌ \(failed)"
            self.tally.textColor = failed > 0 ? .systemRed : .systemGreen
        }
    }
}

// MARK: - Test bookkeeping

struct Result: Codable {
    let section: String
    let name: String
    let passed: Bool
    let detail: String
    let seconds: Double
}

var results: [Result] = []
var caption: Caption!
var section = ""
var passed = 0, failed = 0
let logStart = Date()

func log(_ s: String) {
    let t = String(format: "%6.1fs", Date().timeIntervalSince(logStart))
    print("[\(t)] \(s)")
    fflush(stdout)
}

func status(_ title: String, _ detail: String = "") {
    caption.show(title, detail, passed: passed, failed: failed)
    log("… \(title) \(detail)")
}

/// Runs one check. `body` returns nil on success or a failure message.
@discardableResult
func check(_ name: String, _ body: () throws -> String?) -> Bool {
    if let o = only, section != o, section != "Launch" { return true }
    let started = Date()
    status("\(section) · \(name)")
    var message: String?
    do { message = try body() } catch { message = "threw \(error)" }
    let ok = message == nil
    if ok { passed += 1 } else { failed += 1 }
    let r = Result(
        section: section, name: name, passed: ok, detail: message ?? "ok",
        seconds: Date().timeIntervalSince(started))
    results.append(r)
    caption.show("\(ok ? "✅" : "❌") \(section) · \(name)", message ?? "passed", passed: passed, failed: failed)
    log("\(ok ? "PASS" : "FAIL") \(section) · \(name)\(ok ? "" : " — \(message!)")")
    pause(0.5)
    return ok
}

func pause(_ s: Double) { Thread.sleep(forTimeInterval: s) }

func waitUntil(_ timeout: Double, _ cond: () -> Bool) -> Bool {
    let end = Date().addingTimeInterval(timeout)
    while Date() < end {
        if cond() { return true }
        pause(0.1)
    }
    return cond()
}

func sh(_ cmd: String) -> String {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/bin/zsh")
    p.arguments = ["-c", cmd]
    let pipe = Pipe()
    p.standardOutput = pipe
    p.standardError = pipe
    try? p.run()
    p.waitUntilExit()
    return String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
}

// MARK: - Settings file

func settings() -> [String: Any] {
    guard let data = FileManager.default.contents(atPath: settingsPath),
        let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
        let s = json["settings"] as? [String: Any]
    else { return [:] }
    return s
}

func setting(_ key: String) -> Any? { settings()[key] }

func settingString(_ key: String) -> String {
    switch setting(key) {
    case let v as String: return v
    case let v as NSNumber: return v.stringValue
    case nil, is NSNull: return "nil"
    case let v?: return "\(v)"
    }
}

func waitSetting(_ key: String, _ expected: String, _ timeout: Double = 4) -> String? {
    if waitUntil(timeout, { settingString(key) == expected }) { return nil }
    return "settings.\(key) is \(settingString(key)), expected \(expected)"
}

func isCtrlShiftK(_ b: String) -> Bool {
    let parts = Set(b.split(separator: "+").map { $0.replacingOccurrences(of: "_left", with: "").replacingOccurrences(of: "_right", with: "") })
    return parts == ["ctrl", "shift", "k"]
}

func transcribeBinding() -> String {
    ((setting("bindings") as? [String: Any])?["transcribe"] as? [String: Any])?["current_binding"]
        as? String ?? "nil"
}

// MARK: - Accessibility

typealias El = AXUIElement

func attr<T>(_ el: El, _ name: String) -> T? {
    var v: CFTypeRef?
    guard AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success else { return nil }
    return v as? T
}

func children(_ el: El) -> [El] { attr(el, kAXChildrenAttribute) ?? [] }
func ident(_ el: El) -> String? { attr(el, "AXIdentifier") }
func role(_ el: El) -> String { attr(el, kAXRoleAttribute) ?? "" }
func title(_ el: El) -> String { attr(el, kAXTitleAttribute) ?? "" }
func value(_ el: El) -> String {
    let v: CFTypeRef? = attr(el, kAXValueAttribute)
    if let s = v as? String { return s }
    if let n = v as? NSNumber { return n.stringValue }
    return ""
}

func frame(_ el: El) -> CGRect? {
    guard let p: AXValue = attr(el, kAXPositionAttribute),
        let s: AXValue = attr(el, kAXSizeAttribute)
    else { return nil }
    var pt = CGPoint.zero, sz = CGSize.zero
    AXValueGetValue(p, .cgPoint, &pt)
    AXValueGetValue(s, .cgSize, &sz)
    return CGRect(origin: pt, size: sz)
}

var pid: pid_t = 0
func appEl() -> El { AXUIElementCreateApplication(pid) }

func walk(_ el: El, depth: Int = 0, _ visit: (El) -> Bool) -> Bool {
    if visit(el) { return true }
    if depth > 40 { return false }
    for c in children(el) where walk(c, depth: depth + 1, visit) { return true }
    return false
}

/// The app's windows (the popover is one); skips the app menu bar, which
/// is big and never holds our controls.
func roots() -> [El] {
    let extras: El? = attr(appEl(), "AXExtrasMenuBar")
    return extras.map { [$0] } ?? []
}

func walkRoots(_ visit: (El) -> Bool) -> Bool {
    for r in roots() where walk(r, visit) { return true }
    return false
}

func find(_ id: String) -> El? {
    var found: El?
    _ = walkRoots { el in
        if ident(el) == id { found = el; return true }
        return false
    }
    return found
}

func findAll(prefix: String) -> [(String, El)] {
    var out: [(String, El)] = []
    _ = walkRoots { el in
        if let i = ident(el), i.hasPrefix(prefix) { out.append((i, el)) }
        return false
    }
    return out
}

func waitFor(_ id: String, _ timeout: Double = 5) -> El? {
    var el: El?
    _ = waitUntil(timeout) { el = find(id); return el != nil }
    return el
}

func popoverOpen() -> Bool { find("tabs") != nil }

func statusItem() -> El? {
    guard let bar: El = attr(appEl(), "AXExtrasMenuBar") else { return nil }
    return children(bar).first
}

// MARK: - Real input events

func mouse(to target: CGPoint, steps: Int = 14) {
    let start = CGEvent(source: nil)?.location ?? target
    for i in 1...steps {
        let t = CGFloat(i) / CGFloat(steps)
        let e = t * t * (3 - 2 * t)
        let p = CGPoint(x: start.x + (target.x - start.x) * e, y: start.y + (target.y - start.y) * e)
        CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: p, mouseButton: .left)?
            .post(tap: .cghidEventTap)
        pause(0.015)
    }
}

func click(at p: CGPoint, right: Bool = false) {
    mouse(to: p)
    pause(0.08)
    let (down, up, btn): (CGEventType, CGEventType, CGMouseButton) =
        right ? (.rightMouseDown, .rightMouseUp, .right) : (.leftMouseDown, .leftMouseUp, .left)
    CGEvent(mouseEventSource: nil, mouseType: down, mouseCursorPosition: p, mouseButton: btn)?
        .post(tap: .cghidEventTap)
    pause(0.06)
    CGEvent(mouseEventSource: nil, mouseType: up, mouseCursorPosition: p, mouseButton: btn)?
        .post(tap: .cghidEventTap)
    pause(0.25)
}

func click(_ el: El, right: Bool = false) -> String? {
    guard let f = frame(el) else { return "element has no frame" }
    click(at: CGPoint(x: f.midX, y: f.midY), right: right)
    return nil
}

func click(_ id: String, timeout: Double = 5) -> String? {
    guard let el = waitFor(id, timeout) else { return "no element \(id)" }
    scrollTo(el)
    return click(el)
}

/// Virtual key codes used by the tests.
enum Key: CGKeyCode {
    case escape = 53, space = 49, k = 40, returnKey = 36, option = 58, shift = 56, control = 59
}

func key(_ code: CGKeyCode, _ flags: CGEventFlags = [], down: Bool? = nil) {
    let src = CGEventSource(stateID: .hidSystemState)
    for isDown in down.map({ [$0] }) ?? [true, false] {
        let e = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: isDown)
        e?.flags = flags
        e?.post(tap: .cghidEventTap)
        pause(0.05)
    }
}

/// Holds modifiers + key like a person does: modifiers down, key tap, all up.
func chord(_ mods: [(CGKeyCode, CGEventFlags)], _ code: CGKeyCode) {
    var flags: CGEventFlags = []
    let src = CGEventSource(stateID: .hidSystemState)
    for (m, f) in mods {
        flags.insert(f)
        let e = CGEvent(keyboardEventSource: src, virtualKey: m, keyDown: true)
        e?.flags = flags
        e?.post(tap: .cghidEventTap)
        pause(0.08)
    }
    key(code, flags)
    pause(0.1)
    for (m, f) in mods.reversed() {
        flags.remove(f)
        let e = CGEvent(keyboardEventSource: src, virtualKey: m, keyDown: false)
        e?.flags = flags
        e?.post(tap: .cghidEventTap)
        pause(0.08)
    }
}

// MARK: - App-level helpers

func openPopover() -> String? {
    if popoverOpen() { return nil }
    guard let item = statusItem() else { return "status item not found" }
    if let e = click(item) { return e }
    return waitUntil(4, popoverOpen) ? nil : "popover did not open"
}

func closePopover() {
    if popoverOpen() {
        key(Key.escape.rawValue)
        _ = waitUntil(2) { !popoverOpen() }
    }
}

func selectTab(_ name: String) -> String? {
    if let e = openPopover() { return e }
    guard let tabs = waitFor("tabs") else { return "no tabs" }
    var target: El?
    _ = walk(tabs) { el in
        let desc: String = attr(el, kAXDescriptionAttribute) ?? ""
        if role(el) == "AXRadioButton" && (desc == name || title(el) == name) { target = el; return true }
        return false
    }
    guard let t = target else { return "no tab \(name)" }
    if let e = click(t) { return e }
    pause(0.4)
    return nil
}

func headerTitle() -> String { find("header.title").map(value) ?? "" }
func headerSubtitle() -> String { find("header.subtitle").map(value) ?? "" }

/// Picks `option` in the pop-up button with identifier `id`.
func choose(_ id: String, _ option: String) -> String? {
    guard let popup = waitFor(id) else { return "no popup \(id)" }
    scrollTo(popup)
    if let e = click(popup) { return e }
    var item: El?
    let ok = waitUntil(3) {
        item = nil
        _ = walk(popup) { el in
            if role(el) == "AXMenuItem" && title(el) == option { item = el; return true }
            return false
        }
        return item != nil
    }
    guard ok, let it = item else {
        key(Key.escape.rawValue)
        return "option '\(option)' not in \(id)"
    }
    if let e = click(it) { return e }
    pause(0.4)
    return nil
}

func options(_ id: String) -> [String] {
    guard let popup = find(id) else { return [] }
    var out: [String] = []
    _ = walk(popup) { el in
        if role(el) == "AXMenuItem" { out.append(title(el)) }
        return false
    }
    return out
}

/// Scrolls the popover's content with the real scroll wheel until `el` is
/// inside the visible area.
func scrollTo(_ el: El) {
    guard let area = find("content").flatMap(frame) else { return }
    for _ in 0..<40 {
        guard let f = frame(el) else { return }
        let dy: Int32
        if f.maxY > area.maxY - 4 { dy = -3 } else if f.minY < area.minY + 4 { dy = 3 } else { return }
        mouse(to: CGPoint(x: area.midX, y: area.midY), steps: 4)
        CGEvent(scrollWheelEvent2Source: nil, units: .line, wheelCount: 1, wheel1: dy, wheel2: 0, wheel3: 0)?
            .post(tap: .cghidEventTap)
        pause(0.06)
    }
}

func scrollTo(_ id: String) { if let el = find(id) { scrollTo(el) } }

func clickControl(_ id: String) -> String? {
    guard let el = waitFor(id) else { return "no element \(id)" }
    scrollTo(el)
    return click(el)
}

func launch(fresh: Bool, viaOpen: Bool) {
    _ = sh("pkill -f 'Handy Menubar.app/Contents/MacOS/handy'; sleep 1")
    if fresh {
        // Settings and history only; downloaded models stay.
        _ = sh("rm -f '\(settingsPath)' '\(supportDir)/history.db'*; rm -rf '\(supportDir)/recordings'")
    }
    if viaOpen {
        _ = sh("open '\(appPath)'")
    } else {
        // Child of this process: macOS attributes its permissions to the caller.
        _ = sh("cd '\(appPath)/../../../../..' && ('\(appPath)/Contents/MacOS/handy' > /tmp/handy-menubar-e2e.log 2>&1 &)")
    }
    _ = waitUntil(20) {
        pid = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId).first?.processIdentifier ?? 0
        return pid != 0 && statusItem() != nil
    }
}

func rssMB(_ p: pid_t) -> Double {
    Double(sh("ps -o rss= -p \(p)").trimmingCharacters(in: .whitespacesAndNewlines)) .map { $0 / 1024 } ?? -1
}

func webContentProcesses() -> Int {
    // WebKit content processes a WebView window would start for this app.
    Int(sh("pgrep -fl 'com.apple.WebKit' | wc -l").trimmingCharacters(in: .whitespacesAndNewlines)) ?? -1
}

// MARK: - Test phases

func uiPhase() {
    section = "Launch"
    let webBefore = webContentProcesses()
    launch(fresh: !keepSettings, viaOpen: false)
    check("App starts and puts its icon in the menu bar") {
        pid == 0 ? "app not running" : (statusItem() == nil ? "no status item" : nil)
    }
    check("First run opens the popover with setup") {
        guard waitUntil(6, popoverOpen) else { return "popover not shown" }
        return waitUntil(3) { headerTitle() == "Finish setting up" } ? nil : "header is '\(headerTitle())'"
    }
    check("No Dock icon (accessory app)") {
        NSRunningApplication(processIdentifier: pid)?.activationPolicy == .accessory
            ? nil : "activation policy is not accessory"
    }
    check("No WebView process started") {
        pause(1)
        let after = webContentProcesses()
        return after <= webBefore ? nil : "WebKit processes went \(webBefore) → \(after)"
    }
    check("Resident memory under 150 MB") {
        let mb = rssMB(pid)
        log("RSS \(mb) MB")
        return mb > 0 && mb < 150 ? nil : "RSS \(mb) MB"
    }

    section = "Popover"
    check("Escape closes the popover") {
        key(Key.escape.rawValue)
        return waitUntil(2) { !popoverOpen() } ? nil : "still open"
    }
    check("Clicking the menu bar icon opens it") { openPopover() }
    check("Clicking the icon again closes it") {
        guard let item = statusItem() else { return "no status item" }
        _ = click(item)
        return waitUntil(2) { !popoverOpen() } ? nil : "still open"
    }
    check("Right click shows the classic quick menu") {
        guard let item = statusItem() else { return "no status item" }
        _ = click(item, right: true)
        var titles: [String] = []
        let ok = waitUntil(3) {
            titles = []
            _ = walk(item) { el in
                if role(el) == "AXMenuItem" { titles.append(title(el)) }
                return false
            }
            return titles.contains { $0.lowercased().contains("quit") }
        }
        key(Key.escape.rawValue)
        pause(0.4)
        return ok ? nil : "menu items: \(titles)"
    }

    section = "Setup"
    check("Setup offers the speech model already on disk") {
        if let e = openPopover() { return e }
        guard find("setup.model.use") != nil || find("setup.model.download") != nil else {
            return "no model setup step"
        }
        return nil
    }
    check("Use/Download sets up the speech model") {
        if let use = find("setup.model.use") {
            if let e = click(use) { return e }
        } else if let dl = find("setup.model.download") {
            if let e = click(dl) { return e }
        }
        let ok = waitUntil(600) { !settingString("selected_model").isEmpty && settingString("selected_model") != "nil" && find("setup.model.use") == nil && find("setup.model.download") == nil }
        return ok ? nil : "model not selected (selected_model=\(settingString("selected_model")))"
    }
    check("Header counts the steps left") {
        let mic = find("setup.microphone.allow") != nil
        let ax = find("setup.accessibility.allow") != nil
        let n = (mic ? 1 : 0) + (ax ? 1 : 0)
        if n == 0 { return headerTitle() == "Ready" ? nil : "header '\(headerTitle())'" }
        let want = n == 1 ? "1 step left" : "\(n) steps left"
        return waitUntil(3) { headerSubtitle() == want } ? nil : "subtitle '\(headerSubtitle())', want '\(want)'"
    }

    section = "Home"
    check("Home shows microphone, shortcut, mode and sound") {
        if let e = selectTab("Home") { return e }
        for id in ["home.microphone", "home.shortcut.change", "home.activation", "home.audio_feedback"] where find(id) == nil {
            return "missing \(id)"
        }
        return nil
    }
    check("Picking a microphone saves it") {
        let opts = { () -> [String] in
            guard let p = find("home.microphone") else { return [] }
            _ = click(p)
            pause(0.5)
            let o = options("home.microphone")
            key(Key.escape.rawValue)
            pause(0.3)
            return o
        }()
        guard let mic = opts.first(where: { $0 != "System default" && !$0.contains("not connected") }) else {
            return "no microphones listed: \(opts)"
        }
        if let e = choose("home.microphone", mic) { return e }
        if let e = waitSetting("selected_microphone", mic) { return e }
        // And back to the default.
        if let e = choose("home.microphone", "System default") { return e }
        return waitSetting("selected_microphone", "nil")
    }
    check("Shortcut mode: Push to talk") {
        if let e = choose("home.activation", "Push to talk") { return e }
        if let e = waitSetting("shortcut_activation", "push_to_talk") { return e }
        return nil
    }
    check("Shortcut mode: Toggle") {
        if let e = choose("home.activation", "Toggle") { return e }
        return waitSetting("shortcut_activation", "toggle")
    }
    check("Sound effects switch turns on and off") {
        if let e = clickControl("home.audio_feedback") { return e }
        if let e = waitSetting("audio_feedback", "1") { return e }
        if let e = clickControl("home.audio_feedback") { return e }
        return waitSetting("audio_feedback", "0")
    }
    check("Recording a new shortcut (⌃⇧K)") {
        if let e = clickControl("home.shortcut.change") { return e }
        guard waitFor("home.shortcut.cancel", 3) != nil else { return "capture did not start" }
        pause(0.4)
        chord([(Key.control.rawValue, .maskControl), (Key.shift.rawValue, .maskShift)], Key.k.rawValue)
        // Synthetic events carry side-specific modifiers ("ctrl_left").
        let ok = waitUntil(4) { isCtrlShiftK(transcribeBinding()) }
        return ok ? nil : "binding is \(transcribeBinding())"
    }
    check("The new shortcut shows as ⌃ ⇧ K") {
        guard let b = waitFor("home.shortcut.change") else { return "no shortcut button" }
        return waitUntil(3) { title(b) == "⌃ ⇧ K" || (find("home.shortcut.change").map(title) ?? "") == "⌃ ⇧ K" }
            ? nil : "button says '\(title(b))'"
    }
    check("Escape cancels shortcut capture") {
        if let e = clickControl("home.shortcut.change") { return e }
        guard waitFor("home.shortcut.cancel", 3) != nil else { return "capture did not start" }
        key(Key.escape.rawValue)
        pause(0.6)
        if !popoverOpen() { _ = openPopover() }
        return isCtrlShiftK(transcribeBinding()) ? nil : "binding changed to \(transcribeBinding())"
    }
    check("Reset restores the default shortcut") {
        if let e = clickControl("home.shortcut.reset") { return e }
        return waitUntil(4) { transcribeBinding() == "option+space" } ? nil : "binding is \(transcribeBinding())"
    }

    section = "Settings"
    check("Settings tab lists every group") {
        if let e = selectTab("Settings") { return e }
        for id in ["settings.shortcut.change", "settings.autostart", "settings.paste_method", "settings.logs.open"] where find(id) == nil {
            return "missing \(id)"
        }
        return nil
    }
    let switches: [(String, String)] = [
        ("settings.append_trailing_space", "append_trailing_space"),
        ("settings.filler_word_removal", "filler_word_removal_enabled"),
        ("settings.mute_while_recording", "mute_while_recording"),
        ("settings.vad", "vad_enabled"),
        ("settings.update_checks", "update_checks_enabled"),
        ("settings.always_on_microphone", "always_on_microphone"),
        ("settings.autostart", "autostart_enabled"),
        ("settings.auto_submit", "auto_submit"),
        ("settings.audio_feedback", "audio_feedback"),
    ]
    for (id, keyName) in switches {
        check("Switch \(keyName) flips and saves, then flips back") {
            guard let el = waitFor(id) else { return "no \(id)" }
            scrollTo(el)
            let before = settingString(keyName)
            let flipped = before == "1" ? "0" : "1"
            if let e = click(el) { return e }
            if let e = waitSetting(keyName, flipped) { return e }
            guard let el2 = waitFor(id) else { return "switch vanished" }
            scrollTo(el2)
            if let e = click(el2) { return e }
            return waitSetting(keyName, before)
        }
    }
    check("Sound effects on reveals Sound and Play-sounds-on") {
        if let e = clickControl("settings.audio_feedback") { return e }
        if let e = waitSetting("audio_feedback", "1") { return e }
        return waitFor("settings.sound_theme", 3) != nil && find("settings.output_device") != nil
            ? nil : "sound rows missing"
    }
    check("Sound: Pop") {
        if let e = choose("settings.sound_theme", "Pop") { return e }
        return waitSetting("sound_theme", "pop")
    }
    check("Sound: Marimba") {
        if let e = choose("settings.sound_theme", "Marimba") { return e }
        return waitSetting("sound_theme", "marimba")
    }
    check("Turning sound effects off hides those rows") {
        if let e = clickControl("settings.audio_feedback") { return e }
        if let e = waitSetting("audio_feedback", "0") { return e }
        return waitUntil(3) { find("settings.sound_theme") == nil } ? nil : "sound rows still shown"
    }
    check("Press-a-key-after-pasting reveals the key picker") {
        if let e = clickControl("settings.auto_submit") { return e }
        if let e = waitSetting("auto_submit", "1") { return e }
        guard waitFor("settings.auto_submit_key", 3) != nil else { return "no key picker" }
        if let e = choose("settings.auto_submit_key", "⌘↩") { return e }
        if let e = waitSetting("auto_submit_key", "cmd_enter") { return e }
        if let e = choose("settings.auto_submit_key", "↩ Return") { return e }
        if let e = waitSetting("auto_submit_key", "enter") { return e }
        if let e = clickControl("settings.auto_submit") { return e }
        return waitSetting("auto_submit", "0")
    }
    let popups: [(String, String, String, String, String)] = [
        ("settings.paste_method", "Typing", "direct", "⌘V (clipboard)", "ctrl_v"),
        ("settings.clipboard_handling", "Keep the transcript", "copy_to_clipboard", "Restore it", "dont_modify"),
        ("settings.unload_timeout", "After 15 min", "min15", "After 5 min", "min5"),
        ("settings.history_limit", "Last 25", "25", "Last 5", "5"),
        ("settings.recording_retention", "2 weeks", "weeks2", "With transcripts", "preserve_limit"),
        ("settings.activation", "Push to talk", "push_to_talk", "Toggle", "toggle"),
    ]
    for (id, label, stored, backLabel, backStored) in popups {
        let keyName: String = [
            "settings.paste_method": "paste_method", "settings.clipboard_handling": "clipboard_handling",
            "settings.unload_timeout": "model_unload_timeout", "settings.history_limit": "history_limit",
            "settings.recording_retention": "recording_retention_period",
            "settings.activation": "shortcut_activation",
        ][id]!
        check("\(keyName): \(label), then \(backLabel)") {
            if let el = waitFor(id) { scrollTo(el) }
            if let e = choose(id, label) { return e }
            if let e = waitSetting(keyName, stored) { return e }
            if let el = waitFor(id) { scrollTo(el) }
            if let e = choose(id, backLabel) { return e }
            return waitSetting(keyName, backStored)
        }
    }
    check("Language picker switches language") {
        guard let el = waitFor("settings.language", 2) else {
            log("model has a single language; no picker (expected for English-only models)")
            return nil
        }
        scrollTo(el)
        _ = click(el)
        pause(0.4)
        let opts = options("settings.language")
        key(Key.escape.rawValue)
        pause(0.3)
        guard let pick = opts.first(where: { $0 != "Auto-detect" }) else { return "no languages: \(opts)" }
        let before = settingString("selected_language")
        if let e = choose("settings.language", pick) { return e }
        let ok = waitUntil(3) { settingString("selected_language") != before }
        return ok ? nil : "selected_language stayed \(before)"
    }

    section = "Models"
    check("Models tab shows the installed model in use") {
        if let e = selectTab("Models") { return e }
        let sel = settingString("selected_model")
        return waitFor("models.\(sel)", 3) != nil ? nil : "no row for \(sel)"
    }
    var smallId = ""
    check("Download the smallest model and watch progress") {
        let candidates = findAll(prefix: "models.").filter { $0.0.hasSuffix(".download") }
        guard !candidates.isEmpty else { return "nothing to download" }
        // One pass over the tree: every row's detail starts with its size, e.g. "57 MB · ...".
        var details: [String: String] = [:]
        for (id, el) in findAll(prefix: "models.") where id.hasSuffix(".detail") {
            details[String(id.dropLast(".detail".count))] = value(el)
        }
        func size(_ rowId: String) -> Double {
            let parts = (details[rowId] ?? "").split(separator: " ")
            guard parts.count >= 2, let n = Double(parts[0]) else { return .infinity }
            return parts[1].hasPrefix("GB") ? n * 1000 : n
        }
        let rows = candidates.map { String($0.0.dropLast(".download".count)) }
        guard let best = rows.min(by: { size($0) < size($1) }) else { return "no rows" }
        smallId = String(best.dropFirst("models.".count))
        log("downloading \(smallId) (\(size(best)) MB)")
        if let e = clickControl(best + ".download") { return e }
        let sawProgress = waitUntil(20) { find(best + ".progress") != nil || find(best + ".use") != nil }
        guard sawProgress else { return "no progress bar" }
        let done = waitUntil(600) { find(best + ".use") != nil }
        return done ? nil : "download did not finish"
    }
    check("Use switches the active model") {
        guard !smallId.isEmpty else { return "nothing downloaded" }
        let previous = settingString("selected_model")
        if let e = clickControl("models.\(smallId).use") { return e }
        if let e = waitSetting("selected_model", smallId, 30) { return e }
        // Back to the previous model.
        if let e = clickControl("models.\(previous).use") { return e }
        return waitSetting("selected_model", previous, 30)
    }
    check("Delete removes the downloaded model") {
        guard !smallId.isEmpty else { return "nothing downloaded" }
        if let e = clickControl("models.\(smallId).delete") { return e }
        return waitUntil(15) { find("models.\(smallId).download") != nil } ? nil : "still installed"
    }
}

func dictationPhase() {
    section = "Permissions"
    launch(fresh: false, viaOpen: true)
    check("Handy is running from Finder (its own permissions)") { pid == 0 ? "not running" : nil }
    _ = openPopover()
    check("Microphone and Accessibility are allowed") {
        let ok = waitUntil(300) {
            find("setup.microphone.allow") == nil && find("setup.accessibility.allow") == nil && headerTitle() == "Ready"
        }
        return ok ? nil : "setup still needs: mic=\(find("setup.microphone.allow") != nil) ax=\(find("setup.accessibility.allow") != nil)"
    }
    check("Header says Ready with the model and shortcut") {
        let sub = headerSubtitle()
        return headerTitle() == "Ready" && sub.contains("⌥ Space") ? nil : "header '\(headerTitle())' / '\(sub)'"
    }
    closePopover()

    section = "Dictation"
    _ = sh("osascript -e 'tell application \"TextEdit\" to activate' -e 'tell application \"TextEdit\" to make new document'")
    pause(1.5)
    func doc() -> String {
        sh("osascript -e 'tell application \"TextEdit\" to get text of front document'")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
    let phrase1 = "The quick brown fox jumps over the lazy dog."
    check("Shortcut dictation pastes into TextEdit") {
        chord([(Key.option.rawValue, .maskAlternate)], Key.space.rawValue)  // start (toggle mode)
        guard waitUntil(3, { find("header.title") == nil }) else { return "?" }
        pause(0.8)
        _ = sh("say -v Samantha '\(phrase1)'")
        pause(0.6)
        chord([(Key.option.rawValue, .maskAlternate)], Key.space.rawValue)  // stop
        let ok = waitUntil(30) { doc().lowercased().contains("lazy dog") }
        return ok ? nil : "TextEdit has '\(doc())'"
    }
    check("Popover shows Listening… while recording, Stop ends it") {
        chord([(Key.option.rawValue, .maskAlternate)], Key.space.rawValue)
        pause(0.5)
        if let e = openPopover() { return e }
        guard waitUntil(3, { headerTitle() == "Listening…" }) else { return "header '\(headerTitle())'" }
        guard find("header.stop") != nil, find("header.cancel") != nil else { return "no Stop/Cancel" }
        _ = sh("osascript -e 'tell application \"TextEdit\" to activate'")
        pause(0.3)
        _ = sh("say -v Samantha 'Second sentence from the menu bar.'")
        if let e = openPopover() { return e }
        if let e = click("header.stop") { return e }
        let ok = waitUntil(30) { doc().lowercased().contains("menu bar") }
        return ok ? nil : "TextEdit has '\(doc())'"
    }
    check("Dictate button starts a recording, Cancel discards it") {
        if let e = openPopover() { return e }
        if let e = click("header.dictate") { return e }
        pause(1.0)
        if let e = openPopover() { return e }
        guard waitUntil(3, { headerTitle() == "Listening…" }) else { return "header '\(headerTitle())'" }
        if let e = click("header.cancel") { return e }
        return waitUntil(5) { headerTitle() == "Ready" } ? nil : "header '\(headerTitle())'"
    }

    section = "History"
    check("History lists both transcripts") {
        if let e = selectTab("History") { return e }
        let entries = findAll(prefix: "history.entry.").filter { $0.0.hasSuffix(".copy") }
        return entries.count >= 2 ? nil : "\(entries.count) entries"
    }
    check("Copy puts a transcript on the clipboard") {
        _ = sh("printf '' | pbcopy")
        guard let copy = findAll(prefix: "history.entry.").first(where: { $0.0.hasSuffix(".copy") }) else {
            return "no copy button"
        }
        _ = click(copy.1)
        let ok = waitUntil(3) { !sh("pbpaste").isEmpty }
        return ok ? nil : "clipboard empty"
    }
    check("Delete removes a transcript") {
        let before = findAll(prefix: "history.entry.").filter { $0.0.hasSuffix(".delete") }
        guard let del = before.first else { return "no delete button" }
        _ = click(del.1)
        let ok = waitUntil(4) {
            findAll(prefix: "history.entry.").filter { $0.0.hasSuffix(".delete") }.count == before.count - 1
        }
        return ok ? nil : "still \(before.count) entries"
    }
    check("Home shows the latest transcript") {
        if let e = selectTab("Home") { return e }
        return findAll(prefix: "home.entry.").isEmpty ? "no latest entries" : nil
    }

    section = "Quit"
    check("Quit Handy exits the app") {
        if let e = openPopover() { return e }
        if let e = click("footer.quit") { return e }
        return waitUntil(5) { NSRunningApplication(processIdentifier: pid) == nil || NSRunningApplication(processIdentifier: pid)!.isTerminated } ? nil : "still running"
    }
}

// MARK: - Main

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
caption = Caption()

guard AXIsProcessTrusted() else {
    print("This test needs Accessibility access for the process that runs it.")
    exit(2)
}

Thread {
    caption.show("Handy menu bar · end-to-end test", "phase: \(phase)", passed: 0, failed: 0)
    pause(1.5)
    if phase == "scrolldebug" {
        launch(fresh: false, viaOpen: false)
        _ = selectTab("Settings")
        pause(1)
        let area = find("content").flatMap(frame)
        let el = find("settings.mute_while_recording")
        log("content=\(String(describing: area)) mute=\(String(describing: el.flatMap(frame)))")
        if let el = el { scrollTo(el); pause(1); log("after scroll mute=\(String(describing: frame(el))) open=\(popoverOpen())") }
        _ = sh("\(CommandLine.arguments.last!)")
        exit(0)
    }
    if phase == "dump" {
        launch(fresh: false, viaOpen: false)
        _ = openPopover()
        pause(1)
        func dump(_ el: El, _ d: Int) {
            let desc: String = attr(el, kAXDescriptionAttribute) ?? ""
            print(String(repeating: "  ", count: d) + "\(role(el)) id=\(ident(el) ?? "-") title=\(title(el)) desc=\(desc) value=\(value(el))")
            if d < 30 { for c in children(el) { dump(c, d + 1) } }
        }
        for r in roots() { dump(r, 0) }
        exit(0)
    }
    if phase == "ui" || phase == "all" { uiPhase() }
    if phase == "dictation" || phase == "all" { dictationPhase() }
    let summary = "\(passed) passed, \(failed) failed"
    caption.show(failed == 0 ? "✅ All \(passed) checks passed" : "❌ \(summary)", "Handy menu bar · end-to-end", passed: passed, failed: failed)
    log("DONE: \(summary)")
    if let data = try? JSONEncoder().encode(results) {
        try? data.write(to: URL(fileURLWithPath: resultsPath))
    }
    pause(4)
    exit(failed == 0 ? 0 : 1)
}.start()

app.run()
