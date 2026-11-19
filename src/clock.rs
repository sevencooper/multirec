//! Master clock in 100-ns units. On Windows this is QueryPerformanceCounter scaled
//! exactly like WASAPI scales its `u64QPCPosition`, so packet timestamps and
//! `now_hns()` share one timeline.

pub const HNS_PER_SEC: i64 = 10_000_000;

#[cfg(windows)]
pub fn now_hns() -> i64 {
    use std::sync::OnceLock;
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    static FREQ: OnceLock<i64> = OnceLock::new();
    let freq = *FREQ.get_or_init(|| {
        let mut f = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    });
    let mut c = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
    }
    ((c as i128 * HNS_PER_SEC as i128) / freq as i128) as i64
}

#[cfg(not(windows))]
pub fn now_hns() -> i64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let s = *START.get_or_init(Instant::now);
    (s.elapsed().as_nanos() / 100) as i64
}
