//! Wallpaper Engine window capture. Only WPE desktop descendants are eligible: never
//! capture the screen, Explorer's icon host, or a PecoFence window (feedback).
//! PrintWindow stays on a worker because it can block in the other process.

use crate::{bindings::*, desktop, wide::from_wide};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows_core::{BOOL, Error, PWSTR, Result};

windows_core::link!("user32.dll" "system" fn EnumChildWindows(hwnd: HWND, callback: WNDENUMPROC, data: LPARAM) -> BOOL);
windows_core::link!("user32.dll" "system" fn GetParent(hwnd: HWND) -> HWND);
windows_core::link!("user32.dll" "system" fn PrintWindow(hwnd: HWND, dc: HDC, flags: u32) -> BOOL);
windows_core::link!("kernel32.dll" "system" fn QueryFullProcessImageNameW(process: HANDLE, flags: u32, name: PWSTR, length: *mut u32) -> BOOL);

/// A frame uses physical virtual-screen coordinates and opaque, top-down BGRA pixels.
pub struct CapturedWallpaper {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub struct WallpaperFrame {
    pub images: Vec<CapturedWallpaper>,
    pub captured_at: Instant,
}

fn wpe_process(hwnd: HWND) -> bool {
    // SAFETY: process is opened read-only; the local name buffer is bounded and the handle
    // is closed on both success and failure.
    unsafe {
        let process = OpenProcess(0x1000, false, desktop::window_pid(hwnd));
        if process.0.is_null() {
            return false;
        }
        let mut name = [0u16; 1024];
        let mut len = name.len() as u32;
        let ok =
            QueryFullProcessImageNameW(process, 0, PWSTR(name.as_mut_ptr()), &mut len).as_bool();
        let _ = CloseHandle(process);
        if !ok {
            return false;
        }
        let name = from_wide(&name[..len as usize]).to_ascii_lowercase();
        name.ends_with("\\wallpaper64.exe") || name.ends_with("\\wallpaper32.exe")
    }
}

fn is_wpe_descendant(mut hwnd: HWND) -> bool {
    // Bound the ancestry walk in case another process destroys/reparents a window.
    for _ in 0..16 {
        if hwnd.0.is_null() {
            break;
        }
        if desktop::class_name(hwnd).starts_with("WPEDesktop") && wpe_process(hwnd) {
            return true;
        }
        // SAFETY: GetParent accepts an HWND; stale handles simply stop the walk.
        hwnd = unsafe { GetParent(hwnd) };
    }
    false
}

unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
    let class = desktop::class_name(hwnd);
    // CEF's intermediate GPU child has the real pixels; its parent prints black. Native
    // scene/video roots can be tried too, but unsupported renderers fall back to static.
    if desktop::is_visible(hwnd)
        && (class == "Intermediate D3D Window" || class.starts_with("WPEDesktop"))
        && is_wpe_descendant(hwnd)
    {
        // SAFETY: discovery passes a live Vec pointer synchronously to EnumWindows/children.
        unsafe { &mut *(data.0 as *mut Vec<HWND>) }.push(hwnd);
    }
    true.into()
}

unsafe extern "system" fn collect_top(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: synchronous enumeration reuses the same live Vec as collect.
    unsafe {
        let _ = collect(hwnd, data);
        let _ = EnumChildWindows(hwnd, Some(collect), data);
    }
    true.into()
}

fn discover() -> Vec<HWND> {
    let mut windows = Vec::new();
    // SAFETY: both callbacks complete before the stack-owned Vec goes away.
    unsafe {
        let _ = EnumWindows(
            Some(collect_top),
            LPARAM((&mut windows as *mut Vec<HWND>) as isize),
        );
    }
    // Prefer the CEF surface and suppress its black WPE wrapper for the same rectangle.
    windows.sort_by_key(|h| desktop::class_name(*h) != "Intermediate D3D Window");
    let mut rects = Vec::new();
    windows.retain(|h| {
        let Some(r) = capture_rect(*h) else {
            return false;
        };
        let rect = (r.left, r.top, r.right, r.bottom);
        if rects.contains(&rect) {
            return false;
        }
        rects.push(rect);
        true
    });
    windows
}

fn capture_rect(hwnd: HWND) -> Option<RECT> {
    if !desktop::is_visible(hwnd) {
        return None;
    }
    let mut r = RECT::default();
    // SAFETY: out pointer to a sized local. Refuse enormous/non-desktop allocations.
    if !unsafe { GetWindowRect(hwnd, &mut r) }.as_bool() {
        return None;
    }
    let w = i64::from(r.right) - i64::from(r.left);
    let h = i64::from(r.bottom) - i64::from(r.top);
    (w >= 100 && h >= 100 && w * h <= 33_177_600).then_some(r)
}

struct Surface {
    hwnd: HWND,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Surface {
    fn new(hwnd: HWND, r: RECT) -> Result<Self> {
        let (width, height) = (r.right - r.left, r.bottom - r.top);
        // SAFETY: the worker owns all GDI objects and frees them on failure or Drop. The
        // checked geometry defines exactly the DIB allocation and all subsequent slices.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.0.is_null() {
                return Err(Error::from_thread());
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB as u32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS as u32, &mut bits, None, 0);
            if bitmap.0.is_null() || bits.is_null() {
                let error = Error::from_thread();
                if !bitmap.0.is_null() {
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                }
                let _ = DeleteDC(dc);
                return Err(error);
            }
            let old = SelectObject(dc, HGDIOBJ(bitmap.0));
            if old.0.is_null() || old.0 as isize == -1 {
                let error = Error::from_thread();
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(dc);
                return Err(error);
            }
            Ok(Self {
                hwnd,
                dc,
                bitmap,
                old,
                bits: bits.cast(),
                width,
                height,
            })
        }
    }

    fn capture(&self) -> Option<CapturedWallpaper> {
        let r = capture_rect(self.hwnd)?;
        if (r.right - r.left, r.bottom - r.top) != (self.width, self.height)
            || !is_wpe_descendant(self.hwnd)
        {
            return None;
        }
        // SAFETY: selected DIB is live and exclusive to this worker. Clear old pixels so a
        // renderer that stops supporting PrintWindow cannot masquerade as a fresh frame.
        let len = self.width as usize * self.height as usize * 4;
        unsafe {
            std::ptr::write_bytes(self.bits, 0, len);
            if !PrintWindow(self.hwnd, self.dc, 2).as_bool() {
                return None;
            }
            let _ = GdiFlush();
            let pixels = std::slice::from_raw_parts(self.bits, len);
            // Unsupported GPU wrappers often return success with an entirely black DIB.
            // A genuinely black transition keeps the last frame briefly, then falls back.
            if !pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] != 0 || p[1] != 0 || p[2] != 0)
            {
                return None;
            }
            let mut bgra = pixels.to_vec();
            for p in bgra.as_chunks_mut::<4>().0 {
                p[3] = 255;
            }
            Some(CapturedWallpaper {
                left: r.left,
                top: r.top,
                width: self.width as u32,
                height: self.height as u32,
                bgra,
            })
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: owned objects, restored before deleting the selected bitmap.
        unsafe {
            let _ = SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Synchronous diagnostic entry point. Production uses the bounded worker below.
pub fn capture_once() -> Vec<CapturedWallpaper> {
    discover()
        .into_iter()
        .filter_map(|h| Surface::new(h, capture_rect(h)?).ok()?.capture())
        .collect()
}

struct Shared {
    enabled: AtomicBool,
    stop: AtomicBool,
    latest: Mutex<Option<WallpaperFrame>>,
}

/// Capacity-one mailbox: a slow UI drops old frames, never queues unlimited images.
pub struct LiveWallpaper {
    shared: Arc<Shared>,
    pub interval_ms: u32,
}

impl Default for LiveWallpaper {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveWallpaper {
    pub fn new() -> Self {
        let fps = std::env::var("PECOFENCE_WALLPAPER_FPS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(20)
            .clamp(1, 60);
        let interval_ms = 1000 / fps;
        let shared = Arc::new(Shared {
            enabled: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            latest: Mutex::new(None),
        });
        let worker = shared.clone();
        let spawned = thread::Builder::new()
            .name("wallpaper-engine-capture".into())
            .spawn(move || {
                let mut surfaces: Vec<Surface> = Vec::new();
                let mut discovery = Instant::now() - Duration::from_secs(3);
                while !worker.stop.load(Ordering::Relaxed) {
                    if !worker.enabled.load(Ordering::Relaxed) {
                        surfaces.clear();
                        discovery = Instant::now() - Duration::from_secs(3);
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    let start = Instant::now();
                    if discovery.elapsed() >= Duration::from_secs(2) {
                        let found = discover();
                        surfaces.retain(|s| {
                            found.contains(&s.hwnd)
                                && capture_rect(s.hwnd).is_some_and(|r| {
                                    (r.right - r.left, r.bottom - r.top) == (s.width, s.height)
                                })
                        });
                        for hwnd in found {
                            if !surfaces.iter().any(|s| s.hwnd == hwnd)
                                && let Some(r) = capture_rect(hwnd)
                                && let Ok(s) = Surface::new(hwnd, r)
                            {
                                surfaces.push(s);
                            }
                        }
                        discovery = Instant::now();
                    }
                    let images: Vec<_> = surfaces.iter().filter_map(Surface::capture).collect();
                    if worker.enabled.load(Ordering::Relaxed)
                        && !images.is_empty()
                        && let Ok(mut latest) = worker.latest.lock()
                    {
                        if worker.enabled.load(Ordering::Relaxed) {
                            *latest = Some(WallpaperFrame {
                                images,
                                captured_at: Instant::now(),
                            });
                        }
                    }
                    thread::sleep(
                        Duration::from_millis(interval_ms as u64).saturating_sub(start.elapsed()),
                    );
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "live wallpaper worker unavailable");
        }
        Self {
            shared,
            interval_ms,
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.shared.enabled.store(enabled, Ordering::Relaxed);
        if !enabled && let Ok(mut latest) = self.shared.latest.lock() {
            *latest = None;
        }
    }

    pub fn take_latest(&self) -> Option<WallpaperFrame> {
        self.shared.latest.lock().ok()?.take()
    }
}

impl Drop for LiveWallpaper {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        // Do not join a thread potentially blocked in another process's PrintWindow.
    }
}
