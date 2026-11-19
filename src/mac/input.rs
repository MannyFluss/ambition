//! Your keyboard and mouse, held for the moment Ambition writes into a field,
//! and the keys and clipboard Ambition uses to write.
//!
//! One event tap, on its own thread, is switched on only when needed:
//!
//! - hold: every key, click and scroll you make waits, and is played back
//!   when the write is done, so nothing you do can move the cursor or the
//!   focus halfway through. A hold lets go by itself after three seconds;
//! - watch: while a change is highlighted, your first key puts the cursor at
//!   the end of it before the key goes on, so it never types over the
//!   selection.
//!
//! Ambition's own key events carry a mark, and pass.

use super::ffi::*;
use super::{class, on_main, Id};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const OURS: i64 = 0x416d_6269; // "Ambi"
const OFF: u8 = 0;
const HOLD: u8 = 1;
const WATCH: u8 = 2;

static MODE: AtomicU8 = AtomicU8::new(OFF);
static UNTIL: AtomicU64 = AtomicU64::new(0);
/// What you did while a change was highlighted: nothing, a key, a click.
static ACTED: AtomicU8 = AtomicU8::new(0);
static TAP: Mutex<usize> = Mutex::new(0);
static HELD: Mutex<Vec<usize>> = Mutex::new(Vec::new());
#[allow(clippy::type_complexity)]
static ON_KEY: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

fn ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

const KEY_DOWN: u32 = 10;
const TAP_OFF_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_OFF_USER: u32 = 0xFFFF_FFFF;

extern "C" fn tapped(_proxy: *mut c_void, ty: u32, ev: CGEventRef, _user: *mut c_void) -> CGEventRef {
    unsafe {
        if ty == TAP_OFF_TIMEOUT || ty == TAP_OFF_USER {
            if MODE.load(Ordering::SeqCst) != OFF {
                let t = *TAP.lock().unwrap();
                if t != 0 {
                    CGEventTapEnable(t as CFTypeRef, true);
                }
            }
            return ev;
        }
        if CGEventGetIntegerValueField(ev, kCGEventSourceUserData) == OURS {
            return ev;
        }
        match MODE.load(Ordering::SeqCst) {
            HOLD if ms() < UNTIL.load(Ordering::SeqCst) => {
                HELD.lock().unwrap().push(CGEventCreateCopy(ev) as usize);
                std::ptr::null_mut()
            }
            WATCH if ty == KEY_DOWN => {
                if let Some(f) = ON_KEY.lock().unwrap().take() {
                    f();
                }
                ACTED.store(1, Ordering::SeqCst);
                ev
            }
            WATCH if matches!(ty, 1 | 3 | 25) => {
                ACTED.store(2, Ordering::SeqCst);
                ev
            }
            _ => ev,
        }
    }
}

/// The tap, made the first time it is needed. None without Accessibility.
fn tap() -> Option<CFTypeRef> {
    let mut t = TAP.lock().unwrap();
    if *t == 0 {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || unsafe {
            // keys, flags, every mouse button down, up and drag, and the wheel
            let mask: u64 = [10u32, 11, 12, 1, 2, 3, 4, 6, 7, 22, 25, 26, 27].iter().map(|k| 1u64 << k).sum();
            let port = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault, mask, tapped, std::ptr::null_mut());
            if port.is_null() {
                let _ = tx.send(0usize);
                return;
            }
            CGEventTapEnable(port, false);
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopCommonModes);
            let _ = tx.send(port as usize);
            CFRunLoopRun();
        });
        *t = rx.recv_timeout(Duration::from_secs(2)).unwrap_or(0);
        if *t == 0 {
            crate::app::log("Ambition could not hold the keyboard and mouse while it writes (no event tap).");
        }
    }
    (*t != 0).then_some(*t as CFTypeRef)
}

fn switch(mode: u8) {
    if let Some(t) = tap() {
        MODE.store(mode, Ordering::SeqCst);
        unsafe { CGEventTapEnable(t, mode != OFF) };
    }
}

/// Your input, held until this is dropped or released.
pub struct Hold {
    done: bool,
}

pub fn hold() -> Hold {
    UNTIL.store(ms() + 3000, Ordering::SeqCst);
    switch(HOLD);
    Hold { done: false }
}

impl Hold {
    /// Let go, and play back what was held. How many events that was.
    pub fn release(mut self) -> usize {
        self.done = true;
        let_go()
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if !self.done {
            let_go();
        }
    }
}

fn let_go() -> usize {
    switch(OFF);
    let held: Vec<usize> = std::mem::take(&mut *HELD.lock().unwrap());
    for e in &held {
        unsafe {
            CGEventPost(kCGHIDEventTap, *e as CGEventRef);
            CFRelease(*e as CFTypeRef);
        }
    }
    held.len()
}

#[derive(Debug, PartialEq)]
pub enum Acted {
    Nothing,
    Key,
    Click,
}

/// For `wait`, or until you press a key (then `on_key` runs first) or click.
pub fn watch(wait: Duration, on_key: Box<dyn FnOnce() + Send>) -> Acted {
    ACTED.store(0, Ordering::SeqCst);
    *ON_KEY.lock().unwrap() = Some(on_key);
    switch(WATCH);
    let end = Instant::now() + wait;
    while Instant::now() < end && ACTED.load(Ordering::SeqCst) == 0 {
        std::thread::sleep(Duration::from_millis(15));
    }
    switch(OFF);
    *ON_KEY.lock().unwrap() = None;
    match ACTED.load(Ordering::SeqCst) {
        1 => Acted::Key,
        2 => Acted::Click,
        _ => Acted::Nothing,
    }
}

/// Press a key, with these modifier flags, as Ambition.
pub fn key(code: u16, flags: u64) {
    for down in [true, false] {
        unsafe {
            let e = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
            CGEventSetFlags(e, flags);
            CGEventSetIntegerValueField(e, kCGEventSourceUserData, OURS);
            CGEventPost(kCGHIDEventTap, e);
            CFRelease(e as CFTypeRef);
        }
    }
}

/// Paste `text` where the selection is: through the clipboard, which the
/// caller has saved (`Clip`).
pub fn paste(text: &str) {
    on_main(|| unsafe {
        let pb = msg!(class("NSPasteboard"), "generalPasteboard"; Id);
        msg!(pb, "clearContents"; isize);
        let s = super::cfstr(text);
        let t = super::cfstr("public.utf8-plain-text");
        // Plain text only: Discord pastes any other type as a block of its own.
        msg!(pb, "setString:forType:", s.0 as Id => Id, t.0 as Id => Id; bool);
    });
    key(9, kCGEventFlagMaskCommand); // ⌘V
}

/// What was on the clipboard: every item, every type. Put back when dropped.
pub struct Clip(Vec<Vec<(usize, usize)>>);

impl Clip {
    pub fn save() -> Clip {
        Clip(on_main(|| unsafe {
            let pb = msg!(class("NSPasteboard"), "generalPasteboard"; Id);
            let items = msg!(pb, "pasteboardItems"; Id);
            let mut out = Vec::new();
            let n = if items.is_null() { 0 } else { msg!(items, "count"; usize) };
            for i in 0..n {
                let item = msg!(items, "objectAtIndex:", i => usize; Id);
                let types = msg!(item, "types"; Id);
                let mut kept = Vec::new();
                for j in 0..msg!(types, "count"; usize) {
                    let t = msg!(types, "objectAtIndex:", j => usize; Id);
                    let d = msg!(item, "dataForType:", t => Id; Id);
                    if !d.is_null() {
                        kept.push((msg!(t, "retain"; Id) as usize, msg!(d, "retain"; Id) as usize));
                    }
                }
                out.push(kept);
            }
            out
        }))
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        let items = std::mem::take(&mut self.0);
        on_main(move || unsafe {
            let pb = msg!(class("NSPasteboard"), "generalPasteboard"; Id);
            msg!(pb, "clearContents"; isize);
            let arr = msg!(class("NSMutableArray"), "array"; Id);
            for kept in &items {
                let item = msg!(msg!(class("NSPasteboardItem"), "alloc"; Id), "init"; Id);
                for (t, d) in kept {
                    msg!(item, "setData:forType:", *d as Id => Id, *t as Id => Id; bool);
                }
                msg!(arr, "addObject:", item => Id; ());
                msg!(item, "release"; ());
            }
            if !items.is_empty() {
                msg!(pb, "writeObjects:", arr => Id; bool);
            }
            for kept in &items {
                for (t, d) in kept {
                    msg!(*t as Id, "release"; ());
                    msg!(*d as Id, "release"; ());
                }
            }
        });
    }
}
