//! Hand-written declarations for the parts of the SANE C API we use, loaded at
//! runtime with `libloading` so the crate builds and runs without libsane.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

use libloading::Library;

pub type Word = c_int;
pub type Status = c_int;
pub type Handle = *mut c_void;

pub const STATUS_GOOD: Status = 0;
pub const STATUS_CANCELLED: Status = 2;
pub const STATUS_DEVICE_BUSY: Status = 3;
pub const STATUS_INVAL: Status = 4;
pub const STATUS_EOF: Status = 5;
pub const STATUS_JAMMED: Status = 6;
pub const STATUS_NO_DOCS: Status = 7;
pub const STATUS_COVER_OPEN: Status = 8;
pub const STATUS_IO_ERROR: Status = 9;

pub const TYPE_BOOL: c_int = 0;
pub const TYPE_INT: c_int = 1;
pub const TYPE_FIXED: c_int = 2;
pub const TYPE_STRING: c_int = 3;

pub const UNIT_PIXEL: c_int = 1;
pub const UNIT_MM: c_int = 3;

pub const CONSTRAINT_RANGE: c_int = 1;
pub const CONSTRAINT_WORD_LIST: c_int = 2;
pub const CONSTRAINT_STRING_LIST: c_int = 3;

pub const CAP_INACTIVE: c_int = 32;

pub const ACTION_GET_VALUE: c_int = 0;
pub const ACTION_SET_VALUE: c_int = 1;

pub const FRAME_GRAY: c_int = 0;
pub const FRAME_RGB: c_int = 1;
pub const FRAME_RED: c_int = 2;
pub const FRAME_GREEN: c_int = 3;
pub const FRAME_BLUE: c_int = 4;

#[repr(C)]
pub struct SaneDevice {
    pub name: *const c_char,
    pub vendor: *const c_char,
    pub model: *const c_char,
    pub kind: *const c_char,
}

#[repr(C)]
pub struct SaneRange {
    pub min: Word,
    pub max: Word,
    pub quant: Word,
}

#[repr(C)]
pub union SaneConstraint {
    pub string_list: *const *const c_char,
    pub word_list: *const Word,
    pub range: *const SaneRange,
}

#[repr(C)]
pub struct SaneOptionDescriptor {
    pub name: *const c_char,
    pub title: *const c_char,
    pub desc: *const c_char,
    pub kind: c_int,
    pub unit: c_int,
    pub size: Word,
    pub cap: Word,
    pub constraint_type: c_int,
    pub constraint: SaneConstraint,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct SaneParameters {
    pub format: c_int,
    pub last_frame: Word,
    pub bytes_per_line: Word,
    pub pixels_per_line: Word,
    pub lines: Word,
    pub depth: Word,
}

type AuthCallback = Option<unsafe extern "C" fn(*const c_char, *mut c_char, *mut c_char)>;

pub struct Sane {
    _lib: Library,
    pub get_devices: unsafe extern "C" fn(*mut *const *const SaneDevice, Word) -> Status,
    pub open: unsafe extern "C" fn(*const c_char, *mut Handle) -> Status,
    pub close: unsafe extern "C" fn(Handle),
    pub get_option_descriptor: unsafe extern "C" fn(Handle, Word) -> *const SaneOptionDescriptor,
    pub control_option: unsafe extern "C" fn(Handle, Word, c_int, *mut c_void, *mut Word) -> Status,
    pub start: unsafe extern "C" fn(Handle) -> Status,
    pub get_parameters: unsafe extern "C" fn(Handle, *mut SaneParameters) -> Status,
    pub read: unsafe extern "C" fn(Handle, *mut u8, Word, *mut Word) -> Status,
    pub cancel: unsafe extern "C" fn(Handle),
    strstatus: unsafe extern "C" fn(Status) -> *const c_char,
    /// SANE backends aren't guaranteed to be thread-safe. Device listing,
    /// opening and closing take this lock; per-handle calls run unlocked since a
    /// handle is only used by the one `SaneDevice` that owns it.
    pub global: Mutex<()>,
}

const CANDIDATES: &[&str] = &[
    "libsane.so.1",
    "libsane.so",
    "libsane.1.dylib",
    "libsane.dylib",
    "/opt/homebrew/lib/libsane.1.dylib",
    "/usr/local/lib/libsane.1.dylib",
];

impl Sane {
    pub fn strstatus(&self, status: Status) -> String {
        // SAFETY: sane_strstatus returns a static string (or null).
        unsafe { cstr((self.strstatus)(status)) }.unwrap_or_else(|| format!("SANE status {status}"))
    }

    fn load() -> Result<Sane, String> {
        let mut errors = Vec::new();
        let override_path = std::env::var("JANUS_SCAN_LIBSANE").ok();
        let names = override_path.iter().map(String::as_str).chain(CANDIDATES.iter().copied());
        for name in names {
            // SAFETY: loading libsane runs its (trivial) initializers.
            match unsafe { Library::new(name) } {
                Ok(lib) => return unsafe { Self::from_library(lib) }.map_err(|e| format!("{name}: {e}")),
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        Err(format!("SANE (libsane) is not installed: {}", errors.join("; ")))
    }

    unsafe fn from_library(lib: Library) -> Result<Sane, String> {
        unsafe {
            macro_rules! sym {
                ($name:literal) => {
                    *lib.get(concat!($name, "\0").as_bytes()).map_err(|e| e.to_string())?
                };
            }
            let init: unsafe extern "C" fn(*mut Word, AuthCallback) -> Status = sym!("sane_init");
            // Resolved only to confirm this really is libsane. We never call
            // sane_exit: the instance lives for the whole process.
            let _exit: unsafe extern "C" fn() = sym!("sane_exit");
            let sane = Sane {
                get_devices: sym!("sane_get_devices"),
                open: sym!("sane_open"),
                close: sym!("sane_close"),
                get_option_descriptor: sym!("sane_get_option_descriptor"),
                control_option: sym!("sane_control_option"),
                start: sym!("sane_start"),
                get_parameters: sym!("sane_get_parameters"),
                read: sym!("sane_read"),
                cancel: sym!("sane_cancel"),
                strstatus: sym!("sane_strstatus"),
                global: Mutex::new(()),
                _lib: lib,
            };
            let mut version: Word = 0;
            let status = init(&mut version, None);
            if status != STATUS_GOOD {
                return Err(format!("sane_init failed: {}", sane.strstatus(status)));
            }
            Ok(sane)
        }
    }
}

/// The process-wide libsane instance. `sane_init` runs exactly once, the first
/// time this is called.
pub fn sane() -> Result<&'static Sane, String> {
    static SANE: OnceLock<Result<Sane, String>> = OnceLock::new();
    SANE.get_or_init(Sane::load).as_ref().map_err(Clone::clone)
}

/// Copy a C string. `None` for null.
///
/// # Safety
/// `p` must be null or point to a NUL-terminated string.
pub unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

pub fn fixed_to_f64(w: Word) -> f64 {
    w as f64 / 65536.0
}

pub fn f64_to_fixed(v: f64) -> Word {
    (v * 65536.0).round() as Word
}
