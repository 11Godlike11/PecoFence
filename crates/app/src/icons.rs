//! Icon/thumbnail cache. Extraction runs on STA worker threads; the UI thread gets a
//! `WM_APP_ICON_READY` poke and drains finished results.

use pecofence_core::IconKey;
use pecofence_core::cache::BudgetCache;
use pecofence_platform::HWND;
use pecofence_platform::com::OleGuard;
use pecofence_platform::shell;
use pecofence_platform::window;
use pecofence_render::Image;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const WM_APP_ICON_READY: u32 = pecofence_platform::msg::WM_APP + 4;
const WORKERS: usize = 2;

/// Global icon post-processing (Fences Icon Tint / Chameleon); part of the cache key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IconVariant {
    pub tint: Option<[u8; 3]>,
    /// 0..=100 percent.
    pub tint_strength: u8,
    pub chameleon: bool,
}

impl IconVariant {
    /// Only the parts that change pixels: strength is irrelevant without a tint colour, so
    /// moving the strength slider with tinting off must not flush every icon.
    fn normalized(&self) -> Self {
        Self {
            tint: self.tint,
            tint_strength: if self.tint.is_some() {
                self.tint_strength
            } else {
                0
            },
            chameleon: self.chameleon,
        }
    }

    fn suffix(&self) -> String {
        match (self.tint, self.chameleon) {
            (None, false) => String::new(),
            (Some(t), c) => format!(
                "#t{:02x}{:02x}{:02x}{}{}",
                t[0],
                t[1],
                t[2],
                self.tint_strength,
                if c { "c" } else { "" }
            ),
            (None, true) => "#c".to_string(),
        }
    }

    fn apply(&self, img: &mut Image) {
        if let Some(t) = self.tint {
            img.colorize(t, self.tint_strength as f32 / 100.0);
        }
        if self.chameleon {
            img.fade(0.55, 0.72);
        }
    }
}

#[derive(Clone)]
struct Request {
    key: String,
    path: PathBuf,
    size_px: u32,
    icon_only: bool,
    variant: IconVariant,
    id: u64,
    valid: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
    visible: bool,
}

/// An extraction in flight (or waiting for a retry), with how often it has failed so far.
struct InFlight {
    req: Request,
    attempts: u32,
}

/// Extraction failures on a file that still exists are retried: the shell hands back nothing
/// while a file is being moved in (OneDrive materialises it a moment later), while a copy is
/// still writing, or when the icon handler was busy. Explorer's views show the generic icon
/// first and repaint when the extraction eventually succeeds; here the loading tile stays up.
const RETRY_ATTEMPTS: u32 = 3;
const RETRY_DELAYS_MS: [u64; 3] = [400, 1500, 4000];

struct Ready {
    key: String,
    size_px: u32,
    icon_only: bool,
    id: u64,
    image: Option<Image>,
}

pub enum Lookup {
    /// Extraction finished; `None` means it failed (draw the placeholder, do not retry).
    Ready(Option<Rc<Image>>),
    /// Queued or in flight.
    Pending,
}

type CacheKey = (String, u32, bool);

#[derive(Default)]
struct QueuedWork {
    visible: VecDeque<Request>,
    prefetch: VecDeque<Request>,
    closed: bool,
}

#[derive(Default)]
struct WorkQueue {
    work: Mutex<QueuedWork>,
    signal: Condvar,
}

impl WorkQueue {
    fn push(&self, request: Request) {
        let mut work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        if work.closed {
            return;
        }
        if request.visible {
            work.visible.push_back(request);
        } else {
            work.prefetch.push_back(request);
        }
        self.signal.notify_one();
    }

    fn next(&self) -> Option<Request> {
        let mut work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if work.closed {
                return None;
            }
            if let Some(request) = work
                .visible
                .pop_front()
                .or_else(|| work.prefetch.pop_front())
            {
                if request.valid.load(Ordering::Acquire)
                    && request
                        .started
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    return Some(request);
                }
                continue;
            }
            work = self.signal.wait(work).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn close(&self) {
        let mut work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        work.closed = true;
        work.visible.clear();
        work.prefetch.clear();
        self.signal.notify_all();
    }
}

pub struct IconCache {
    map: BudgetCache<CacheKey, Option<Rc<Image>>>,
    pending: HashMap<CacheKey, InFlight>,
    retry: Vec<(Instant, CacheKey)>,
    work: Arc<WorkQueue>,
    rx: Receiver<Ready>,
    variant: IconVariant,
    next_id: u64,
    discarded: u64,
}

impl Drop for IconCache {
    fn drop(&mut self) {
        for pending in self.pending.values() {
            pending.req.valid.store(false, Ordering::Release);
        }
        self.work.close();
    }
}

impl IconCache {
    pub fn new(notify: HWND) -> Self {
        let work = Arc::new(WorkQueue::default());
        let (done_tx, rx) = channel::<Ready>();
        let notify_raw = notify.0 as isize;
        for i in 0..WORKERS {
            let work = work.clone();
            let done_tx = done_tx.clone();
            let _ = std::thread::Builder::new()
                .name(format!("pecofence-icons-{i}"))
                .stack_size(256 * 1024)
                .spawn(move || {
                    let _sta = OleGuard::init();
                    while let Some(req) = work.next() {
                        let image = shell::shell_image(&req.path, req.size_px, req.icon_only)
                            .ok()
                            .map(|img| {
                                let mut image = Image {
                                    width: img.width,
                                    height: img.height,
                                    bgra: img.bgra,
                                };
                                req.variant.apply(&mut image);
                                image
                            });
                        if !req.valid.load(Ordering::Acquire) {
                            continue;
                        }
                        if done_tx
                            .send(Ready {
                                key: req.key,
                                size_px: req.size_px,
                                icon_only: req.icon_only,
                                id: req.id,
                                image,
                            })
                            .is_err()
                        {
                            break;
                        }
                        window::post_message(
                            HWND(notify_raw as *mut core::ffi::c_void),
                            WM_APP_ICON_READY,
                            0,
                            0,
                        );
                    }
                });
        }
        Self::with_queue(work, rx)
    }

    fn with_queue(work: Arc<WorkQueue>, rx: Receiver<Ready>) -> Self {
        Self {
            map: BudgetCache::new(512, 16 * 1024 * 1024),
            pending: HashMap::new(),
            retry: Vec::new(),
            work,
            rx,
            variant: IconVariant::default(),
            next_id: 0,
            discarded: 0,
        }
    }

    /// Changes the global tint / chameleon processing; drops everything cached (callers must
    /// also drop the per-window icon references).
    pub fn set_variant(&mut self, variant: IconVariant) -> bool {
        let variant = variant.normalized();
        if self.variant == variant {
            return false;
        }
        self.variant = variant;
        self.map.clear();
        for pending in self.pending.values() {
            pending.req.valid.store(false, Ordering::Release);
        }
        self.pending.clear();
        self.retry.clear();
        true
    }

    pub fn cache_key(key: &IconKey) -> String {
        match key {
            IconKey::ByExt(e) => format!("ext:{e}"),
            IconKey::ByContent { path, mtime } => format!("file:{path}:{mtime}"),
        }
    }

    /// Cached image, or queues extraction and reports `Pending`.
    pub fn get(&mut self, key: &IconKey, path: &Path, size_px: u32, icon_only: bool) -> Lookup {
        self.lookup(key, path, size_px, icon_only, true)
    }

    pub fn prefetch(
        &mut self,
        key: &IconKey,
        path: &Path,
        size_px: u32,
        icon_only: bool,
    ) -> Lookup {
        self.lookup(key, path, size_px, icon_only, false)
    }

    fn lookup(
        &mut self,
        key: &IconKey,
        path: &Path,
        size_px: u32,
        icon_only: bool,
        visible: bool,
    ) -> Lookup {
        let k = (
            format!("{}{}", Self::cache_key(key), self.variant.suffix()),
            size_px,
            icon_only,
        );
        if let Some(entry) = self.map.get(&k) {
            return Lookup::Ready(entry.clone());
        }
        if let Some(pending) = self.pending.get_mut(&k) {
            if visible && !pending.req.visible {
                pending.req.visible = true;
                if !pending.req.started.load(Ordering::Acquire) {
                    self.work.push(pending.req.clone());
                }
            }
        } else {
            self.next_id = self.next_id.wrapping_add(1);
            let req = Request {
                key: k.0.clone(),
                path: path.to_path_buf(),
                size_px,
                icon_only,
                variant: self.variant,
                id: self.next_id,
                valid: Arc::new(AtomicBool::new(true)),
                started: Arc::new(AtomicBool::new(false)),
                visible,
            };
            self.work.push(req.clone());
            self.pending.insert(k, InFlight { req, attempts: 0 });
        }
        Lookup::Pending
    }

    /// Bitmap-cache key for an icon as currently processed (variant suffix included).
    pub fn draw_key(&self, key: &IconKey, size_px: u32, icon_only: bool) -> String {
        format!(
            "{}{}#mode{}@{}",
            Self::cache_key(key),
            self.variant.suffix(),
            icon_only as u8,
            size_px
        )
    }

    /// Moves finished extractions into the cache. Returns how many arrived.
    pub fn drain_results(&mut self) -> usize {
        let mut n = 0;
        let now = Instant::now();
        while let Ok(r) = self.rx.try_recv() {
            let k = (r.key, r.size_px, r.icon_only);
            if !self
                .pending
                .get(&k)
                .is_some_and(|f| f.req.id == r.id && f.req.valid.load(Ordering::Acquire))
            {
                self.discarded += 1;
                continue;
            }
            if r.image.is_none()
                && let Some(f) = self.pending.get_mut(&k)
                && f.attempts < RETRY_ATTEMPTS
                && f.req.path.exists()
            {
                // Still there but not extractable right now: try again later, and keep the
                // entry pending so the windows keep showing the loading tile instead of the
                // failure placeholder.
                let delay = RETRY_DELAYS_MS[f.attempts as usize % RETRY_DELAYS_MS.len()];
                f.attempts += 1;
                tracing::debug!(key = %k.0, attempt = f.attempts, delay_ms = delay, "icon extraction failed; retrying");
                self.retry.push((now + Duration::from_millis(delay), k));
                continue;
            }
            self.pending.remove(&k);
            let bytes = r.image.as_ref().map_or(0, |image| image.bgra.len());
            self.map.insert(k, r.image.map(Rc::new), bytes);
            n += 1;
        }
        n
    }

    /// Re-queues the retries that are due; returns how long until the next one (None = no
    /// retries outstanding), so the caller can arm a timer.
    pub fn flush_retries(&mut self) -> Option<Duration> {
        let now = Instant::now();
        let mut i = 0;
        while i < self.retry.len() {
            if self.retry[i].0 <= now {
                let (_, k) = self.retry.swap_remove(i);
                if let Some(f) = self.pending.get(&k) {
                    f.req.started.store(false, Ordering::Release);
                    self.work.push(f.req.clone());
                }
            } else {
                i += 1;
            }
        }
        self.retry
            .iter()
            .map(|(due, _)| due.saturating_duration_since(now))
            .min()
    }

    pub fn invalidate(&mut self, key: &IconKey) {
        let prefix = Self::cache_key(key);
        let hit = |k: &str| {
            (k.starts_with(&prefix) && k[prefix.len()..].starts_with(['#'])) || k == prefix
        };
        self.map.retain(|(k, _, _), _| !hit(k));
        self.pending.retain(|(k, _, _), pending| {
            if hit(k) {
                pending.req.valid.store(false, Ordering::Release);
                false
            } else {
                true
            }
        });
        self.retry.retain(|(_, (k, _, _))| !hit(k));
    }

    pub fn bytes(&self) -> usize {
        self.map.bytes()
    }
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
    pub fn discarded(&self) -> u64 {
        self.discarded
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// Decides how an item's icon is cached: per-extension for ordinary documents, per-file for
/// things whose icon depends on content (executables, shortcuts, images, folders with icons).
pub fn icon_key_for(path: &Path, is_folder: bool, mtime: i64) -> (IconKey, bool) {
    // Namespace items have no extension; each one has its own icon, and the Recycle Bin's
    // changes with its contents (encoded in `mtime`).
    if pecofence_platform::shell::is_namespace_path(path) {
        return (
            IconKey::ByContent {
                path: path.to_string_lossy().to_lowercase(),
                mtime,
            },
            true,
        );
    }
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let by_content = is_folder
        || matches!(
            ext.as_str(),
            ".exe"
                | ".lnk"
                | ".url"
                | ".ico"
                | ".cur"
                | ".appref-ms"
                | ".msi"
                | ".scr"
                | ".png"
                | ".jpg"
                | ".jpeg"
                | ".gif"
                | ".bmp"
                | ".webp"
                | ".heic"
                | ".tif"
                | ".tiff"
                | ".mp4"
                | ".mkv"
                | ".mov"
                | ".avi"
                | ".pdf"
        );
    let thumbnail_capable = matches!(
        ext.as_str(),
        ".png"
            | ".jpg"
            | ".jpeg"
            | ".gif"
            | ".bmp"
            | ".webp"
            | ".heic"
            | ".tif"
            | ".tiff"
            | ".mp4"
            | ".mkv"
            | ".mov"
            | ".avi"
    );
    let key = if by_content {
        IconKey::ByContent {
            path: path.to_string_lossy().to_lowercase(),
            mtime,
        }
    } else {
        IconKey::ByExt(ext)
    };
    (key, !thumbnail_capable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (IconCache, std::sync::mpsc::Sender<Ready>) {
        let (tx, rx) = channel();
        (
            IconCache::with_queue(Arc::new(WorkQueue::default()), rx),
            tx,
        )
    }

    fn complete(tx: &std::sync::mpsc::Sender<Ready>, req: &Request, pixel: u8) {
        tx.send(Ready {
            key: req.key.clone(),
            size_px: req.size_px,
            icon_only: req.icon_only,
            id: req.id,
            image: Some(Image {
                width: 1,
                height: 1,
                bgra: vec![pixel; 4],
            }),
        })
        .unwrap();
    }

    #[test]
    fn late_result_after_refresh_cannot_replace_or_remove_the_new_request() {
        let (mut cache, tx) = fixture();
        let key = IconKey::ByExt(".txt".into());
        cache.get(&key, Path::new("fixture.txt"), 48, true);
        let old = cache.work.next().unwrap();
        cache.invalidate(&key);
        assert!(!old.valid.load(Ordering::Acquire));
        cache.get(&key, Path::new("fixture.txt"), 48, true);
        let new = cache.work.next().unwrap();
        complete(&tx, &old, 1);
        assert_eq!(cache.drain_results(), 0);
        assert_eq!(cache.pending_len(), 1);
        complete(&tx, &new, 2);
        assert_eq!(cache.drain_results(), 1);
        complete(&tx, &old, 1);
        assert_eq!(cache.drain_results(), 0);
        let Lookup::Ready(Some(image)) = cache.get(&key, Path::new("fixture.txt"), 48, true) else {
            panic!("fresh icon missing")
        };
        assert_eq!(image.bgra, vec![2; 4]);
        assert_eq!(cache.discarded(), 2);
    }

    #[test]
    fn switching_variant_back_does_not_resurrect_the_previous_generation() {
        let (mut cache, tx) = fixture();
        let key = IconKey::ByExt(".txt".into());
        cache.get(&key, Path::new("fixture.txt"), 48, true);
        let old = cache.work.next().unwrap();
        cache.set_variant(IconVariant {
            chameleon: true,
            ..Default::default()
        });
        cache.set_variant(IconVariant::default());
        cache.get(&key, Path::new("fixture.txt"), 48, true);
        complete(&tx, &old, 1);
        assert_eq!(cache.drain_results(), 0);
        assert_eq!(cache.pending_len(), 1);
    }

    #[test]
    fn queued_cancelled_extractions_are_skipped_before_entering_shell() {
        let (mut cache, _) = fixture();
        let key = IconKey::ByExt(".txt".into());
        cache.get(&key, Path::new("old.txt"), 48, true);
        cache.invalidate(&key);
        cache.get(&key, Path::new("new.txt"), 48, true);
        assert_eq!(cache.work.next().unwrap().path, Path::new("new.txt"));
    }

    #[test]
    fn visible_requests_promote_prefetch_and_extract_only_once() {
        let (mut cache, _) = fixture();
        let a = IconKey::ByExt(".a".into());
        let b = IconKey::ByExt(".b".into());
        cache.prefetch(&a, Path::new("a"), 48, true);
        cache.prefetch(&b, Path::new("b"), 48, true);
        cache.get(&b, Path::new("b"), 48, true);
        cache.get(&b, Path::new("b"), 48, true);
        let first = cache.work.next().unwrap();
        assert_eq!(first.path, Path::new("b"));
        let second = cache.work.next().unwrap();
        assert_eq!(second.path, Path::new("a"));
        assert_eq!(cache.pending_len(), 2);
        assert!(first.started.load(Ordering::Acquire));
        assert_eq!(cache.work.work.lock().unwrap().prefetch.len(), 1);
    }

    #[test]
    fn extraction_mode_has_distinct_cpu_and_gpu_identity() {
        let (mut cache, tx) = fixture();
        let key = IconKey::ByExt(".png".into());
        cache.get(&key, Path::new("image.png"), 48, true);
        cache.get(&key, Path::new("image.png"), 48, false);
        let icon = cache.work.next().unwrap();
        let thumb = cache.work.next().unwrap();
        complete(&tx, &icon, 1);
        complete(&tx, &thumb, 2);
        assert_eq!(cache.drain_results(), 2);
        assert_ne!(
            cache.draw_key(&key, 48, true),
            cache.draw_key(&key, 48, false)
        );
        let Lookup::Ready(Some(image)) = cache.get(&key, Path::new("image.png"), 48, false) else {
            panic!("thumbnail missing")
        };
        assert_eq!(image.bgra, vec![2; 4]);
    }

    #[test]
    fn large_batches_evict_cpu_images_at_the_byte_budget() {
        let (mut cache, tx) = fixture();
        for index in 0..400 {
            let key = IconKey::ByContent {
                path: format!("fixture-{index}.png"),
                mtime: 1,
            };
            cache.get(&key, Path::new("fixture.png"), 128, false);
            let req = cache.work.next().unwrap();
            tx.send(Ready {
                key: req.key,
                size_px: req.size_px,
                icon_only: req.icon_only,
                id: req.id,
                image: Some(Image {
                    width: 128,
                    height: 128,
                    bgra: vec![255; 128 * 128 * 4],
                }),
            })
            .unwrap();
            assert_eq!(cache.drain_results(), 1);
        }
        assert_eq!(cache.bytes(), 16 * 1024 * 1024);
        assert_eq!(cache.len(), 256);
        assert_eq!(cache.pending_len(), 0);
    }

    #[test]
    fn variant_suffix_ignores_strength_without_tint() {
        let off = IconVariant::default();
        let strong = IconVariant {
            tint_strength: 90,
            ..off
        };
        assert_eq!(off.normalized(), strong.normalized());
        assert_eq!(off.suffix(), "");
        let tinted = IconVariant {
            tint: Some([1, 2, 3]),
            tint_strength: 60,
            chameleon: true,
        };
        assert_eq!(tinted.suffix(), "#t01020360c");
        assert_eq!(
            IconVariant {
                chameleon: true,
                ..off
            }
            .suffix(),
            "#c"
        );
    }
}

#[cfg(test)]
mod namespace_icon_tests {
    use super::*;

    /// Namespace items get a per-item content key (no extension to share) whose `mtime` carries
    /// the Recycle Bin's empty/full state, and they are icons rather than thumbnails.
    #[test]
    fn namespace_items_use_content_keys() {
        let bin = Path::new("::{645FF040-5081-101B-9F08-00AA002F954E}");
        let (empty, icon_only) = icon_key_for(bin, false, 0);
        let (full, _) = icon_key_for(bin, false, 1);
        assert!(icon_only);
        assert!(matches!(&empty, IconKey::ByContent { path, mtime: 0 } if path.starts_with("::{")));
        assert_ne!(empty, full);
    }
}
