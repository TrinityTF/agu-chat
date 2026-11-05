//! Tiny Win32 helpers declared by hand so no extra crates are needed.
#[cfg(windows)]
mod w {
    #[repr(C)]
    struct Pmc {
        cb: u32,
        page_fault_count: u32,
        peak_working_set: usize,
        working_set: usize,
        qp_peak_paged: usize,
        qp_paged: usize,
        qp_peak_nonpaged: usize,
        qp_nonpaged: usize,
        pagefile_usage: usize,
        peak_pagefile: usize,
    }
    #[repr(C)]
    struct Systemtime([u16; 8]);
    #[repr(C)]
    struct Tzi {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: Systemtime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: Systemtime,
        daylight_bias: i32,
    }
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(h: isize, pmc: *mut Pmc, cb: u32) -> i32;
        fn GetTimeZoneInformation(tzi: *mut Tzi) -> u32;
    }
    /// (working set MB, private/pagefile MB)
    pub fn mem_mb() -> (f32, f32) {
        unsafe {
            let mut p: Pmc = std::mem::zeroed();
            p.cb = std::mem::size_of::<Pmc>() as u32;
            if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut p, p.cb) != 0 {
                return (p.working_set as f32 / 1048576.0, p.pagefile_usage as f32 / 1048576.0);
            }
        }
        (0.0, 0.0)
    }
    /// Local UTC offset in minutes (east positive).
    pub fn tz_offset_min() -> i32 {
        unsafe {
            let mut t: Tzi = std::mem::zeroed();
            let id = GetTimeZoneInformation(&mut t);
            let extra = if id == 2 { t.daylight_bias } else { t.standard_bias };
            -(t.bias + extra)
        }
    }
}
#[cfg(windows)]
pub use w::*;
#[cfg(not(windows))]
pub fn mem_mb() -> (f32, f32) {
    (0.0, 0.0)
}
#[cfg(not(windows))]
pub fn tz_offset_min() -> i32 {
    0
}

pub fn log(msg: &str) {
    use std::io::Write;
    static ON: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let target = ON.get_or_init(|| {
        std::env::var("AGU_LOG").ok().filter(|v| !v.is_empty() && v != "0").map(|v| {
            if v == "1" {
                "agu-chat-native.log".to_string()
            } else {
                v
            }
        })
    });
    if let Some(path) = target {
        let line = format!("[{}] {}\n", crate::irc::now_ms(), msg);
        eprint!("{line}");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

#[macro_export]
macro_rules! logf {
    ($($a:tt)*) => { $crate::sys::log(&format!($($a)*)) };
}
