//! Ambition on a Mac with a screen: the running Ambition is an app. Its main
//! thread runs the app's loop, which carries
//!
//! - the global key (⌃⌥↩, `hotkey =` in the config), registered with Carbon.
//!   It needs no permission, and the app under it never sees the chord. It
//!   steps aside while an app on the `editor` list is in front (Zed, by
//!   default), so that app's own binding runs `ambition fire` instead;
//! - the one dialog (may a press run here?), and notices at the top of the
//!   screen that take no focus.
//!
//! A press of the key reads the focused text field through Accessibility
//! (`ax.rs`) and hands it to the app as a field press (`app.rs`). Writing
//! back holds your keyboard and mouse for the moment it takes (`input.rs`).

#![allow(clippy::missing_safety_doc)]

macro_rules! msg {
    ($recv:expr, $sel:expr $(, $arg:expr => $ty:ty)* ; $ret:ty) => {{
        #[allow(clippy::missing_transmute_annotations)]
        let f: unsafe extern "C" fn($crate::mac::ffi::Id, $crate::mac::ffi::Sel $(, $ty)*) -> $ret =
            std::mem::transmute($crate::mac::ffi::objc_msgSend as unsafe extern "C" fn());
        f($recv as $crate::mac::ffi::Id, $crate::mac::sel($sel) $(, $arg)*)
    }};
}

pub mod ax;
pub mod ffi;
pub mod input;

use crate::app::{log, App};
use crate::config;
use crate::ui::{self, Answer};
use ffi::*;
use std::ffi::{c_void, CString};
use std::ptr::null_mut;
use std::sync::{Arc, Mutex, OnceLock};

pub fn sel(name: &str) -> Sel {
    let c = CString::new(name).unwrap();
    unsafe { sel_registerName(c.as_ptr()) }
}

pub fn class(name: &str) -> Id {
    let c = CString::new(name).unwrap();
    unsafe { objc_getClass(c.as_ptr()) }
}

/// A Core Foundation object Ambition owns: released when dropped.
pub struct Cf(pub CFTypeRef);

unsafe impl Send for Cf {}
unsafe impl Sync for Cf {}

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

impl Clone for Cf {
    fn clone(&self) -> Cf {
        if !self.0.is_null() {
            unsafe { CFRetain(self.0) };
        }
        Cf(self.0)
    }
}

impl Cf {
    pub fn same(&self, other: &Cf) -> bool {
        !self.0.is_null() && !other.0.is_null() && unsafe { CFEqual(self.0, other.0) } != 0
    }
}

/// A CFString (an NSString too).
pub fn cfstr(s: &str) -> Cf {
    Cf(unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, kCFStringEncodingUTF8, 0) })
}

/// A CFString's UTF-16 units. `s` must be a CFString (an NSString).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn units(s: CFTypeRef) -> Vec<u16> {
    unsafe {
        let n = CFStringGetLength(s);
        let mut buf = vec![0u16; n.max(0) as usize];
        if n > 0 {
            CFStringGetCharacters(s, CFRange { location: 0, length: n }, buf.as_mut_ptr());
        }
        buf
    }
}

/// An NSString's text, or None for nil.
pub fn rstr(s: Id) -> Option<String> {
    (!s.is_null()).then(|| String::from_utf16_lossy(&units(s)))
}

/// Run on the main thread, inside an autorelease pool, and wait for it.
pub fn on_main<R, F: FnOnce() -> R>(f: F) -> R {
    unsafe {
        if pthread_main_np() != 0 {
            return pool(f);
        }
        struct Ctx<F, R> {
            f: Option<F>,
            r: Option<std::thread::Result<R>>,
        }
        extern "C" fn tramp<F: FnOnce() -> R, R>(p: *mut c_void) {
            let c = unsafe { &mut *(p as *mut Ctx<F, R>) };
            let f = c.f.take().unwrap();
            c.r = Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pool(f))));
        }
        let mut c = Ctx { f: Some(f), r: None };
        dispatch_sync_f(&_dispatch_main_q as *const c_void, &mut c as *mut Ctx<F, R> as *mut c_void, tramp::<F, R>);
        match c.r.take().unwrap() {
            Ok(r) => r,
            Err(e) => std::panic::resume_unwind(e),
        }
    }
}

pub fn pool<R>(f: impl FnOnce() -> R) -> R {
    unsafe {
        let p = objc_autoreleasePoolPush();
        let r = f();
        objc_autoreleasePoolPop(p);
        r
    }
}

/// Whether this process can reach a screen (it cannot over ssh).
pub fn gui() -> bool {
    let d = unsafe { CGSessionCopyCurrentDictionary() };
    if d.is_null() {
        return false;
    }
    unsafe { CFRelease(d) };
    true
}

/// The app in front.
#[derive(Clone, Debug)]
pub struct Front {
    pub pid: i32,
    pub bundle: String,
    pub name: String,
}

pub fn front() -> Option<Front> {
    on_main(|| unsafe {
        let ws = msg!(class("NSWorkspace"), "sharedWorkspace"; Id);
        let a = msg!(ws, "frontmostApplication"; Id);
        (!a.is_null()).then(|| Front {
            pid: msg!(a, "processIdentifier"; i32),
            bundle: rstr(msg!(a, "bundleIdentifier"; Id)).unwrap_or_default(),
            name: rstr(msg!(a, "localizedName"; Id)).unwrap_or_default(),
        })
    })
}

static APP: OnceLock<Arc<App>> = OnceLock::new();

/// The registered hotkey, if any, and the chord it is.
struct Hot {
    reg: usize,
    chord: Option<config::Chord>,
}

static HOT: Mutex<Hot> = Mutex::new(Hot { reg: 0, chord: None });

/// Register the hotkey as the config says, or step aside for an editor in
/// front. Read on each app switch, so a changed config counts from the next.
fn refresh(front_bundle: &str) {
    let chord = match config::chord(&config::hotkey_words()) {
        Ok(c) => c,
        Err(e) => {
            log(&format!("{e} Using {}.", config::DEFAULT_HOTKEY));
            config::chord(config::DEFAULT_HOTKEY).ok().flatten()
        }
    };
    let aside = config::editors().iter().any(|e| e == front_bundle);
    let want = if aside { None } else { chord };
    let mut h = HOT.lock().unwrap();
    if h.chord == want && (want.is_none() || h.reg != 0) {
        return;
    }
    unsafe {
        if h.reg != 0 {
            UnregisterEventHotKey(h.reg as *mut c_void);
            h.reg = 0;
        }
        h.chord = None;
        if let Some(c) = want {
            let mut r: *mut c_void = null_mut();
            let id = EventHotKeyID { signature: u32::from_be_bytes(*b"Ambi"), id: 1 };
            let err = RegisterEventHotKey(c.key, c.mods, id, GetApplicationEventTarget(), 0, &mut r);
            if err == 0 {
                h.reg = r as usize;
                h.chord = want;
            } else {
                log(&format!("the hotkey {} could not be registered (error {err}): another app may hold it.", config::hotkey_words()));
            }
        }
    }
}

extern "C" fn on_hotkey(_call: *mut c_void, _event: *mut c_void, _user: *mut c_void) -> i32 {
    let Some(f) = front() else { return 0 };
    if f.pid == std::process::id() as i32 || config::editors().contains(&f.bundle) {
        return 0;
    }
    if let Some(app) = APP.get().cloned() {
        std::thread::spawn(move || press(app, f));
    }
    0
}

/// One press of the key, off the main thread.
fn press(app: Arc<App>, f: Front) {
    let r = pool(|| ax::snapshot(&f)).and_then(|(snap, surface)| app.field_press(snap, Arc::new(surface)));
    match r {
        Ok(t) => log(&format!("a press in {}: {t}", f.name)),
        Err(e) => ui::tell(&e),
    }
}

extern "C" fn activated(_this: Id, _cmd: Sel, note: Id) {
    unsafe {
        let info = msg!(note, "userInfo"; Id);
        let key = cfstr("NSWorkspaceApplicationKey");
        let a = msg!(info, "objectForKey:", key.0 as Id => Id; Id);
        let b = if a.is_null() { None } else { rstr(msg!(a, "bundleIdentifier"; Id)) };
        refresh(&b.unwrap_or_default());
    }
}

/// Be the app until the process ends. `lock` keeps this the one Ambition.
pub fn run(app: Arc<App>, lock: std::fs::File) -> ! {
    let _ = APP.set(app);
    ui::install(Box::new(MacUi));
    unsafe {
        pool(|| {
            let nsapp = msg!(class("NSApplication"), "sharedApplication"; Id);
            // An accessory: no Dock icon, no menu bar.
            msg!(nsapp, "setActivationPolicy:", 1isize => isize; bool);
            let spec = EventTypeSpec { class: kEventClassKeyboard, kind: kEventHotKeyPressed };
            InstallEventHandler(GetApplicationEventTarget(), on_hotkey, 1, &spec, null_mut(), null_mut());
            let cls = objc_allocateClassPair(class("NSObject"), c"AmbitionObserver".as_ptr(), 0);
            if !cls.is_null() {
                class_addMethod(cls, sel("activated:"), activated as *const c_void, c"v@:@".as_ptr());
                objc_registerClassPair(cls);
                let obs = msg!(msg!(cls, "alloc"; Id), "init"; Id);
                let ws = msg!(class("NSWorkspace"), "sharedWorkspace"; Id);
                let nc = msg!(ws, "notificationCenter"; Id);
                let name = cfstr("NSWorkspaceDidActivateApplicationNotification");
                msg!(nc, "addObserver:selector:name:object:", obs => Id, sel("activated:") => Sel, name.0 as Id => Id,
                    null_mut::<c_void>() => Id; ());
            }
            refresh(&front().map(|f| f.bundle).unwrap_or_default());
            match HOT.lock().unwrap().chord {
                Some(_) => log(&format!("The key {} presses in any text field.", config::hotkey_words())),
                None => log("The global key is off or stepped aside."),
            }
        });
        let nsapp = msg!(class("NSApplication"), "sharedApplication"; Id);
        msg!(nsapp, "run"; ());
    }
    drop(lock);
    std::process::exit(0)
}

struct MacUi;

impl ui::Ui for MacUi {
    fn ask(&self, title: &str, text: &str) -> Answer {
        on_main(|| unsafe { alert(title, text) })
    }

    fn tell(&self, text: &str) {
        on_main(|| unsafe { notice(text) })
    }
}

/// The dialog: Allow once (Return), Always, Deny (Escape). It gives up after
/// two minutes. The app that was in front gets the focus back.
unsafe fn alert(title: &str, text: &str) -> Answer {
    let nsapp = msg!(class("NSApplication"), "sharedApplication"; Id);
    let ws = msg!(class("NSWorkspace"), "sharedWorkspace"; Id);
    let prev = msg!(ws, "frontmostApplication"; Id);
    if !prev.is_null() {
        msg!(prev, "retain"; Id);
    }
    let a = msg!(msg!(class("NSAlert"), "alloc"; Id), "init"; Id);
    let (t, x) = (cfstr(title), cfstr(text));
    msg!(a, "setMessageText:", t.0 as Id => Id; ());
    msg!(a, "setInformativeText:", x.0 as Id => Id; ());
    for (k, b) in ["Allow once", "Always", "Deny"].iter().enumerate() {
        let s = cfstr(b);
        let button = msg!(a, "addButtonWithTitle:", s.0 as Id => Id; Id);
        if k == 2 {
            let esc = cfstr("\u{1b}");
            msg!(button, "setKeyEquivalent:", esc.0 as Id => Id; ());
        }
    }
    msg!(nsapp, "activateIgnoringOtherApps:", true => bool; ());
    let window = msg!(a, "window"; Id);
    msg!(window, "setLevel:", 8isize => isize; ());
    let mode = cfstr("NSModalPanelRunLoopMode");
    let modes = msg!(class("NSArray"), "arrayWithObject:", mode.0 as Id => Id; Id);
    msg!(nsapp, "performSelector:withObject:afterDelay:inModes:", sel("abortModal") => Sel, null_mut::<c_void>() => Id,
        120.0f64 => f64, modes => Id; ());
    let r = msg!(a, "runModal"; isize);
    msg!(class("NSObject"), "cancelPreviousPerformRequestsWithTarget:selector:object:", nsapp => Id, sel("abortModal") => Sel,
        null_mut::<c_void>() => Id; ());
    msg!(a, "release"; ());
    if !prev.is_null() {
        msg!(prev, "activateWithOptions:", 0usize => usize; bool);
        msg!(prev, "release"; ());
    }
    match r {
        1000 => Answer::Once,
        1001 => Answer::Always,
        1002 => Answer::Deny,
        _ => Answer::Unanswered,
    }
}

/// The notice's panel and its label, made once.
static NOTICE: Mutex<(usize, usize)> = Mutex::new((0, 0));

/// A notice at the top of the screen for a few seconds. It takes no focus
/// and no clicks, so typing goes on where it was.
unsafe fn notice(text: &str) {
    let mut n = NOTICE.lock().unwrap();
    if n.0 == 0 {
        let rect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: 300.0, height: 60.0 } };
        let panel = msg!(msg!(class("NSPanel"), "alloc"; Id), "initWithContentRect:styleMask:backing:defer:",
            rect => CGRect, 1usize << 7 => usize, 2usize => usize, false => bool; Id);
        msg!(panel, "setOpaque:", false => bool; ());
        let clear = msg!(class("NSColor"), "clearColor"; Id);
        msg!(panel, "setBackgroundColor:", clear => Id; ());
        msg!(panel, "setHasShadow:", true => bool; ());
        msg!(panel, "setLevel:", 25isize => isize; ());
        msg!(panel, "setIgnoresMouseEvents:", true => bool; ());
        msg!(panel, "setHidesOnDeactivate:", false => bool; ());
        msg!(panel, "setReleasedWhenClosed:", false => bool; ());
        msg!(panel, "setCollectionBehavior:", 1usize | 16 | 256 => usize; ());
        let view = msg!(panel, "contentView"; Id);
        msg!(view, "setWantsLayer:", true => bool; ());
        let layer = msg!(view, "layer"; Id);
        msg!(layer, "setCornerRadius:", 10.0f64 => f64; ());
        let gray = CGColorCreateGenericGray(0.1, 0.94);
        msg!(layer, "setBackgroundColor:", gray => CFTypeRef; ());
        let empty = cfstr("");
        let label = msg!(class("NSTextField"), "wrappingLabelWithString:", empty.0 as Id => Id; Id);
        msg!(label, "retain"; Id);
        let white = msg!(class("NSColor"), "whiteColor"; Id);
        msg!(label, "setTextColor:", white => Id; ());
        let font = msg!(class("NSFont"), "systemFontOfSize:", 13.0f64 => f64; Id);
        msg!(label, "setFont:", font => Id; ());
        msg!(view, "addSubview:", label => Id; ());
        *n = (panel as usize, label as usize);
    }
    let (panel, label) = (n.0 as Id, n.1 as Id);
    let s = cfstr(&format!("Ambition: {text}"));
    msg!(label, "setStringValue:", s.0 as Id => Id; ());
    msg!(label, "setPreferredMaxLayoutWidth:", 440.0f64 => f64; ());
    let size = msg!(label, "fittingSize"; CGSize);
    let (w, h) = (size.width.min(440.0) + 28.0, size.height + 20.0);
    let screen = CGDisplayBounds(CGMainDisplayID());
    let frame = CGRect {
        origin: CGPoint { x: ((screen.size.width - w) / 2.0).round(), y: (screen.size.height - h - 44.0).round() },
        size: CGSize { width: w, height: h },
    };
    msg!(panel, "setFrame:display:", frame => CGRect, true => bool; ());
    let inner = CGRect { origin: CGPoint { x: 14.0, y: 10.0 }, size: CGSize { width: w - 28.0, height: h - 20.0 } };
    msg!(label, "setFrame:", inner => CGRect; ());
    msg!(panel, "orderFrontRegardless"; ());
    msg!(class("NSObject"), "cancelPreviousPerformRequestsWithTarget:", panel => Id; ());
    let secs = (4.0 + text.chars().count() as f64 / 20.0).min(14.0);
    msg!(panel, "performSelector:withObject:afterDelay:", sel("orderOut:") => Sel, null_mut::<c_void>() => Id, secs => f64; ());
}
