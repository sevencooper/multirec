use std::path::Path;

/// Free bytes available to the user on the volume containing `path`
/// (walks up to the nearest existing ancestor).
#[cfg(windows)]
pub fn disk_free(path: &Path) -> Option<u64> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    let h = HSTRING::from(p.to_string_lossy().as_ref());
    let mut avail = 0u64;
    unsafe { GetDiskFreeSpaceExW(&h, Some(&mut avail), None, None).ok()? };
    Some(avail)
}

#[cfg(not(windows))]
pub fn disk_free(_path: &Path) -> Option<u64> {
    None
}

pub fn open_in_explorer(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}
