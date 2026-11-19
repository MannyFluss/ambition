//! The C and Objective-C calls Ambition makes on macOS, declared by hand:
//! Core Foundation, Accessibility, Core Graphics events and windows, the
//! Carbon hotkey, the Objective-C runtime and libdispatch.

#![allow(non_upper_case_globals, non_snake_case, dead_code, clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void};

pub type CFTypeRef = *const c_void;
pub type Id = *mut c_void;
pub type Sel = *const c_void;
pub type CGEventRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct CFRange {
    pub location: isize,
    pub length: isize,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt64Type: isize = 4;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> u8;
    pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFArrayGetTypeID() -> usize;
    pub fn CFURLGetTypeID() -> usize;
    pub fn CFNumberGetTypeID() -> usize;
    pub fn CFStringCreateWithBytes(alloc: CFTypeRef, bytes: *const u8, len: isize, enc: u32, external: u8) -> CFTypeRef;
    pub fn CFStringGetLength(s: CFTypeRef) -> isize;
    pub fn CFStringGetCharacters(s: CFTypeRef, range: CFRange, buf: *mut u16);
    pub fn CFArrayGetCount(a: CFTypeRef) -> isize;
    pub fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    pub fn CFURLGetString(u: CFTypeRef) -> CFTypeRef;
    pub fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    pub fn CFDictionaryCreate(alloc: CFTypeRef, keys: *const CFTypeRef, values: *const CFTypeRef, n: isize,
        kcb: *const c_void, vcb: *const c_void) -> CFTypeRef;
    pub fn CFNumberGetValue(n: CFTypeRef, ty: isize, out: *mut c_void) -> u8;
    pub fn CFRunLoopGetCurrent() -> CFTypeRef;
    pub fn CFRunLoopRun();
    pub fn CFRunLoopAddSource(rl: CFTypeRef, src: CFTypeRef, mode: CFTypeRef);
    pub fn CFMachPortCreateRunLoopSource(alloc: CFTypeRef, port: CFTypeRef, order: isize) -> CFTypeRef;
    pub static kCFBooleanTrue: CFTypeRef;
    pub static kCFRunLoopCommonModes: CFTypeRef;
    pub static kCFTypeDictionaryKeyCallBacks: c_void;
    pub static kCFTypeDictionaryValueCallBacks: c_void;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    pub fn AXUIElementGetTypeID() -> usize;
    pub fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFTypeRef, out: *mut CFTypeRef) -> i32;
    pub fn AXUIElementSetAttributeValue(el: CFTypeRef, attr: CFTypeRef, value: CFTypeRef) -> i32;
    pub fn AXUIElementCopyParameterizedAttributeValue(el: CFTypeRef, attr: CFTypeRef, param: CFTypeRef, out: *mut CFTypeRef) -> i32;
    pub fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> i32;
    pub fn AXValueCreate(ty: u32, ptr: *const c_void) -> CFTypeRef;
    pub fn AXValueGetValue(v: CFTypeRef, ty: u32, ptr: *mut c_void) -> u8;
    pub fn AXValueGetTypeID() -> usize;
    pub fn AXIsProcessTrusted() -> u8;
    pub fn AXIsProcessTrustedWithOptions(opts: CFTypeRef) -> u8;
    pub static kAXTrustedCheckOptionPrompt: CFTypeRef;
}

pub const kAXValueCFRangeType: u32 = 4;
pub const kAXErrorAttributeUnsupported: i32 = -25205;
pub const kAXErrorNoValue: i32 = -25212;
pub const kAXErrorParameterizedAttributeUnsupported: i32 = -25213;

pub type TapCallback = extern "C" fn(proxy: *mut c_void, ty: u32, event: CGEventRef, user: *mut c_void) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGEventCreateKeyboardEvent(src: CFTypeRef, key: u16, down: bool) -> CGEventRef;
    pub fn CGEventSetFlags(e: CGEventRef, flags: u64);
    pub fn CGEventPost(tap: u32, e: CGEventRef);
    pub fn CGEventSetIntegerValueField(e: CGEventRef, field: u32, value: i64);
    pub fn CGEventGetIntegerValueField(e: CGEventRef, field: u32) -> i64;
    pub fn CGEventCreateCopy(e: CGEventRef) -> CGEventRef;
    pub fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, cb: TapCallback, user: *mut c_void) -> CFTypeRef;
    pub fn CGEventTapEnable(tap: CFTypeRef, enable: bool);
    pub fn CGWindowListCopyWindowInfo(option: u32, relative: u32) -> CFTypeRef;
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGRequestScreenCaptureAccess() -> bool;
    pub fn CGSessionCopyCurrentDictionary() -> CFTypeRef;
    pub fn CGMainDisplayID() -> u32;
    pub fn CGDisplayBounds(display: u32) -> CGRect;
    pub fn CGColorCreateGenericGray(gray: f64, alpha: f64) -> CFTypeRef;
    pub static kCGWindowOwnerPID: CFTypeRef;
    pub static kCGWindowNumber: CFTypeRef;
    pub static kCGWindowLayer: CFTypeRef;
}

pub const kCGHIDEventTap: u32 = 0;
pub const kCGSessionEventTap: u32 = 1;
pub const kCGHeadInsertEventTap: u32 = 0;
pub const kCGEventTapOptionDefault: u32 = 0;
pub const kCGEventSourceUserData: u32 = 42;
pub const kCGEventFlagMaskCommand: u64 = 1 << 20;
pub const kCGWindowListOptionOnScreenOnly: u32 = 1;
pub const kCGWindowListExcludeDesktopElements: u32 = 16;

#[repr(C)]
pub struct EventTypeSpec {
    pub class: u32,
    pub kind: u32,
}

#[repr(C)]
pub struct EventHotKeyID {
    pub signature: u32,
    pub id: u32,
}

pub type EventHandler = extern "C" fn(call: *mut c_void, event: *mut c_void, user: *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    pub fn GetApplicationEventTarget() -> *mut c_void;
    pub fn InstallEventHandler(target: *mut c_void, handler: EventHandler, n: usize, list: *const EventTypeSpec,
        user: *mut c_void, out: *mut *mut c_void) -> i32;
    pub fn RegisterEventHotKey(code: u32, mods: u32, id: EventHotKeyID, target: *mut c_void, options: u32,
        out: *mut *mut c_void) -> i32;
    pub fn UnregisterEventHotKey(r: *mut c_void) -> i32;
}

pub const kEventClassKeyboard: u32 = u32::from_be_bytes(*b"keyb");
pub const kEventHotKeyPressed: u32 = 5;

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc")]
extern "C" {
    pub fn objc_getClass(name: *const c_char) -> Id;
    pub fn sel_registerName(name: *const c_char) -> Sel;
    pub fn objc_msgSend();
    pub fn objc_allocateClassPair(sup: Id, name: *const c_char, extra: usize) -> Id;
    pub fn class_addMethod(cls: Id, sel: Sel, imp: *const c_void, types: *const c_char) -> bool;
    pub fn objc_registerClassPair(cls: Id);
    pub fn objc_autoreleasePoolPush() -> *mut c_void;
    pub fn objc_autoreleasePoolPop(p: *mut c_void);
}

extern "C" {
    pub static _dispatch_main_q: c_void;
    pub fn dispatch_sync_f(q: *const c_void, ctx: *mut c_void, work: extern "C" fn(*mut c_void));
    pub fn pthread_main_np() -> i32;
}
