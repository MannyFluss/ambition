//! Text fields through Accessibility.
//!
//! Reading: the app's focused element, its value and its selection. Apps
//! built on Chromium (Chrome, Electron apps such as Discord) build their
//! accessibility tree only when asked: Ambition sets AXManualAccessibility on
//! the app, or AXEnhancedUserInterface where that is not supported (Chrome).
//!
//! Offsets: a field's cursor and selection count UTF-16 units of its text,
//! except where they do not. In Chromium's editable web text made of blocks
//! (Google Docs, Discord), the line breaks in the value are not counted. The
//! field's own text for a range (AXStringForRange) shows which way it counts,
//! and the two are lined up once per read.
//!
//! Writing: only the lines that changed, one piece at a time, last first:
//! select the piece (AXSelectedTextRange), then replace the selection with
//! AXSelectedText. Where that does nothing (Chromium's editable web text),
//! the selection is pasted over from the clipboard, and the clipboard is put
//! back. The whole value is never set: in Discord that desyncs its editor
//! from what it shows. Before each piece, the field must still be the one in
//! front and say what Ambition expects, and your keyboard and mouse are held
//! meanwhile (`input.rs`), so text can never land anywhere else.

use super::ffi::*;
use super::{cfstr, input, units, Cf, Front};
use crate::field::{self, Snapshot, Surface};
use std::path::Path;
use std::time::{Duration, Instant};

fn copy(el: &Cf, name: &str) -> Result<Cf, i32> {
    let mut out: CFTypeRef = std::ptr::null();
    let n = cfstr(name);
    let e = unsafe { AXUIElementCopyAttributeValue(el.0, n.0, &mut out) };
    if e == 0 && !out.is_null() {
        Ok(Cf(out))
    } else {
        Err(e)
    }
}

fn of_type(v: &Cf, id: usize) -> bool {
    unsafe { CFGetTypeID(v.0) == id }
}

fn string(el: &Cf, name: &str) -> Option<String> {
    let v = copy(el, name).ok()?;
    of_type(&v, unsafe { CFStringGetTypeID() }).then(|| String::from_utf16_lossy(&units(v.0)))
}

fn element(el: &Cf, name: &str) -> Option<Cf> {
    let v = copy(el, name).ok()?;
    of_type(&v, unsafe { AXUIElementGetTypeID() }).then_some(v)
}

fn children(el: &Cf) -> Vec<Cf> {
    let Ok(v) = copy(el, "AXChildren") else { return Vec::new() };
    if !of_type(&v, unsafe { CFArrayGetTypeID() }) {
        return Vec::new();
    }
    let n = unsafe { CFArrayGetCount(v.0) };
    (0..n)
        .map(|i| {
            let e = unsafe { CFArrayGetValueAtIndex(v.0, i) };
            unsafe { CFRetain(e) };
            Cf(e)
        })
        .collect()
}

fn set(el: &Cf, name: &str, value: CFTypeRef) -> i32 {
    let n = cfstr(name);
    unsafe { AXUIElementSetAttributeValue(el.0, n.0, value) }
}

fn range(el: &Cf) -> Option<CFRange> {
    let v = copy(el, "AXSelectedTextRange").ok()?;
    let mut r = CFRange::default();
    (unsafe { AXValueGetValue(v.0, kAXValueCFRangeType, &mut r as *mut CFRange as *mut _) } != 0).then_some(r)
}

fn set_range(el: &Cf, at: usize, len: usize) -> bool {
    let r = CFRange { location: at as isize, length: len as isize };
    let v = Cf(unsafe { AXValueCreate(kAXValueCFRangeType, &r as *const CFRange as *const _) });
    set(el, "AXSelectedTextRange", v.0) == 0
}

/// The field's own text for a range of its offsets.
fn text_for(el: &Cf, at: usize, len: usize) -> Result<Vec<u16>, i32> {
    let r = CFRange { location: at as isize, length: len as isize };
    let p = Cf(unsafe { AXValueCreate(kAXValueCFRangeType, &r as *const CFRange as *const _) });
    let n = cfstr("AXStringForRange");
    let mut out: CFTypeRef = std::ptr::null();
    let e = unsafe { AXUIElementCopyParameterizedAttributeValue(el.0, n.0, p.0, &mut out) };
    if e != 0 || out.is_null() {
        return Err(e);
    }
    let v = Cf(out);
    if of_type(&v, unsafe { CFStringGetTypeID() }) {
        Ok(units(v.0))
    } else {
        Err(-1)
    }
}

/// For each UTF-16 index of `text` (0 to its length), the field's offset.
/// None when the two cannot be lined up.
fn offsets(el: &Cf, text: &[u16]) -> Option<Vec<usize>> {
    let n = text.len();
    let same = || Some((0..=n).collect::<Vec<usize>>());
    match text_for(el, 0, n) {
        Ok(s) if s == text => return same(),
        Err(e) if e == kAXErrorParameterizedAttributeUnsupported || e == kAXErrorAttributeUnsupported => return same(),
        _ => {}
    }
    // The longest range the field will give, then line the two up: the
    // field's text is the value with some line breaks left out.
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if text_for(el, 0, mid).is_ok() {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let s = text_for(el, 0, lo).unwrap_or_default();
    let mut map = vec![0; n + 1];
    let (mut i, mut j) = (0, 0);
    while i < n {
        if j < s.len() && text[i] == s[j] {
            map[i] = j;
            j += 1;
        } else if text[i] == b'\n' as u16 {
            map[i] = j;
        } else {
            return None;
        }
        i += 1;
    }
    map[n] = j;
    (j == s.len()).then_some(map)
}

pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

/// Ask macOS to show its own Accessibility prompt for Ambition.
pub fn ask_trust() {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let d = Cf(CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), values.as_ptr(), 1,
            &kCFTypeDictionaryKeyCallBacks as *const _, &kCFTypeDictionaryValueCallBacks as *const _));
        AXIsProcessTrustedWithOptions(d.0);
    }
}

fn focused(app: &Cf) -> Option<Cf> {
    element(app, "AXFocusedUIElement")
}

/// The first page address above an element, when it is in a web page.
fn web_url(el: &Cf) -> Option<String> {
    let mut cur = el.clone();
    for _ in 0..80 {
        if string(&cur, "AXRole").as_deref() == Some("AXWebArea") {
            if let Ok(u) = copy(&cur, "AXURL") {
                if of_type(&u, unsafe { CFURLGetTypeID() }) {
                    let s = String::from_utf16_lossy(&units(unsafe { CFURLGetString(u.0) }));
                    if s.starts_with("http") || s.starts_with("file:") {
                        return Some(s);
                    }
                }
            }
        }
        cur = element(&cur, "AXParent")?;
    }
    None
}

/// Read the focused field of the app in front.
pub fn snapshot(f: &Front) -> Result<(Snapshot, AxField), String> {
    if !trusted() {
        ask_trust();
        return Err("Ambition needs Accessibility to read text fields. In System Settings › Privacy & Security › \
            Accessibility, turn on Ambition, then press again."
            .into());
    }
    let app = Cf(unsafe { AXUIElementCreateApplication(f.pid) });
    unsafe { AXUIElementSetMessagingTimeout(app.0, 2.0) };
    let mut el = focused(&app);
    if el.is_none() {
        // Chromium builds its tree only when asked.
        let yes = unsafe { kCFBooleanTrue };
        if set(&app, "AXManualAccessibility", yes) != 0 {
            set(&app, "AXEnhancedUserInterface", yes);
        }
        let end = Instant::now() + Duration::from_millis(1500);
        while el.is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(60));
            el = focused(&app);
        }
    }
    let el = el.ok_or(format!("nothing in {} has the keyboard focus that Ambition can read.", f.name))?;
    let value = string(&el, "AXValue").ok_or(format!("the focused thing in {} is not a text field Ambition can read.", f.name))?;
    let text = field::normalize(&value);
    let t16: Vec<u16> = text.encode_utf16().collect();
    let at = range(&el).map(|r| r.location.max(0) as usize).unwrap_or(t16.len());
    let index = match offsets(&el, &t16) {
        Some(map) => map.iter().position(|&o| o >= at).unwrap_or(t16.len()),
        None => at.min(t16.len()),
    };
    let window = element(&el, "AXWindow").or_else(|| element(&app, "AXFocusedWindow"));
    let title = window.as_ref().and_then(|w| string(w, "AXTitle")).unwrap_or_default();
    let ident = string(&el, "AXIdentifier").or_else(|| string(&el, "AXDOMIdentifier")).filter(|s| !s.is_empty());
    let snap = Snapshot {
        app: f.name.clone(),
        bundle: f.bundle.clone(),
        title,
        url: web_url(&el),
        ident,
        line: field::line_at(&text, index),
        text,
    };
    Ok((snap, AxField { pid: f.pid, name: f.name.clone(), app, el, window }))
}

/// A field, reached again through Accessibility.
pub struct AxField {
    pid: i32,
    name: String,
    app: Cf,
    el: Cf,
    window: Option<Cf>,
}

impl AxField {
    /// The same app, window and field still in front, or why not.
    fn here(&self) -> Result<(), String> {
        let f = super::front().ok_or("no app is in front")?;
        if f.pid != self.pid {
            return Err(format!("{} is no longer in front.", self.name));
        }
        match focused(&self.app) {
            Some(e) if e.same(&self.el) => {}
            _ => return Err(format!("the focus moved off the field in {}.", self.name)),
        }
        if let Some(w) = &self.window {
            if !element(&self.app, "AXFocusedWindow").is_some_and(|x| x.same(w)) {
                return Err(format!("another window of {} is in front.", self.name));
            }
        }
        Ok(())
    }

    fn value(&self) -> Result<String, String> {
        string(&self.el, "AXValue").map(|v| field::normalize(&v)).ok_or(format!("the field in {} is gone.", self.name))
    }

    /// Wait for the field to say something other than `before`.
    fn changed_from(&self, before: &str) -> bool {
        let end = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < end {
            if self.value().is_ok_and(|v| v != before) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        false
    }

    /// Select `span` (UTF-16 in `text`, what the field says) for a moment,
    /// then put the cursor at its end. A key pressed meanwhile ends it at
    /// once, so it never types over the selection.
    fn flash(&self, text: &str, span: (usize, usize)) {
        let t16: Vec<u16> = text.encode_utf16().collect();
        let Some(map) = offsets(&self.el, &t16) else { return };
        let (a, b) = (map[span.0.min(t16.len())], map[span.1.min(t16.len())]);
        if !set_range(&self.el, a, b - a) {
            return;
        }
        let el = self.el.clone();
        input::watch(Duration::from_millis(1100), Box::new(move || {
            set_range(&el, b, 0);
        }));
        if range(&self.el) == Some(CFRange { location: a as isize, length: (b - a) as isize }) {
            set_range(&self.el, b, 0);
        }
    }
}

impl Surface for AxField {
    fn read(&self) -> Result<String, String> {
        self.value()
    }

    fn write(&self, old: &str, new: &str, mark: bool) -> Result<String, String> {
        let hold = input::hold();
        self.here()?;
        let now = self.value()?;
        if now != old {
            return Err(format!("the field in {} changed since Ambition last read it.", self.name));
        }
        let t16: Vec<u16> = now.encode_utf16().collect();
        let map = offsets(&self.el, &t16).ok_or(format!("Ambition cannot place text exactly in this field of {}.", self.name))?;
        let mut clip: Option<input::Clip> = None;
        for h in field::hunks(old, new).iter().rev() {
            self.here()?;
            let (a, b) = (map[h.at], map[h.at + h.len]);
            if !set_range(&self.el, a, b - a) {
                return Err(format!("Ambition could not select text in the field in {}.", self.name));
            }
            let before = self.value()?;
            let s = cfstr(&h.text);
            if set(&self.el, "AXSelectedText", s.0) == 0 && self.value().is_ok_and(|v| v != before) {
                continue;
            }
            // The clipboard, for fields that take no text through Accessibility.
            if clip.is_none() {
                clip = Some(input::Clip::save());
            }
            self.here()?;
            if h.text.is_empty() {
                input::key(51, 0);
            } else {
                input::paste(&h.text);
            }
            if !self.changed_from(&before) {
                return Err(format!("the field in {} did not take the text.", self.name));
            }
        }
        drop(clip);
        let typed = hold.release();
        let fin = self.value()?;
        if mark && typed == 0 {
            self.flash(&fin, field::changed(&now, &fin));
        }
        Ok(fin)
    }

    fn highlight(&self, text: &str, span: (usize, usize)) -> Result<(), String> {
        self.here()?;
        if self.value()? != text {
            return Err(format!("the field in {} changed.", self.name));
        }
        self.flash(text, span);
        Ok(())
    }

    fn window_text(&self) -> Result<String, String> {
        let w = self.window.clone().or_else(|| element(&self.app, "AXFocusedWindow")).ok_or("the window is gone.")?;
        let mut out: Vec<String> = Vec::new();
        let (mut nodes, mut chars) = (0usize, 0usize);
        fn walk(el: &Cf, out: &mut Vec<String>, nodes: &mut usize, chars: &mut usize) {
            if *nodes > 6000 || *chars > 30000 {
                return;
            }
            *nodes += 1;
            let role = string(el, "AXRole").unwrap_or_default();
            if matches!(role.as_str(), "AXStaticText" | "AXTextArea" | "AXTextField" | "AXHeading") {
                let v = string(el, "AXValue").or_else(|| string(el, "AXTitle")).unwrap_or_default();
                let v = v.trim();
                if !v.is_empty() && !field::blank(v) && out.last().map(String::as_str) != Some(v) {
                    *chars += v.len();
                    out.push(v.to_string());
                }
                if role == "AXStaticText" {
                    return;
                }
            }
            for c in children(el) {
                walk(&c, out, nodes, chars);
            }
        }
        walk(&w, &mut out, &mut nodes, &mut chars);
        Ok(out.join("\n"))
    }

    fn look(&self, out: &Path) -> Result<(), String> {
        unsafe {
            if !CGPreflightScreenCaptureAccess() {
                CGRequestScreenCaptureAccess();
                return Err("Ambition needs Screen Recording for `ambition look`. In System Settings › Privacy & Security › \
                    Screen & System Audio Recording, turn on Ambition."
                    .into());
            }
        }
        let id = window_number(self.pid).ok_or(format!("{} has no window on screen.", self.name))?;
        let o = std::process::Command::new("screencapture").args(["-x", "-o", &format!("-l{id}")]).arg(out).output()
            .map_err(|e| format!("cannot run screencapture: {e}"))?;
        if !o.status.success() || !out.exists() {
            return Err(format!("screencapture failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
        }
        Ok(())
    }
}

/// The frontmost on-screen window of a process, by number.
fn window_number(pid: i32) -> Option<i64> {
    unsafe {
        let list = Cf(CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, 0));
        if list.0.is_null() {
            return None;
        }
        let num = |d: CFTypeRef, k: CFTypeRef| {
            let v = CFDictionaryGetValue(d, k);
            let mut n: i64 = 0;
            (!v.is_null() && CFNumberGetValue(v, kCFNumberSInt64Type, &mut n as *mut i64 as *mut _) != 0).then_some(n)
        };
        for i in 0..CFArrayGetCount(list.0) {
            let d = CFArrayGetValueAtIndex(list.0, i);
            if num(d, kCGWindowOwnerPID) == Some(pid as i64) && num(d, kCGWindowLayer) == Some(0) {
                return num(d, kCGWindowNumber);
            }
        }
        None
    }
}
