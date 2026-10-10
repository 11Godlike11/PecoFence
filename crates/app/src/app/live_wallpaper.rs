//! Runtime live-wallpaper overlay, separate from the static virtual-desktop cache.
use super::*;
use pecofence_platform::live_wallpaper::LiveWallpaper;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    rc::Weak,
};

pub(super) struct LiveBackdrop {
    pub worker: LiveWallpaper,
    static_set: Option<Rc<BackdropSets>>,
    last_frame: Option<Instant>,
    applied: Option<(u64, Weak<BackdropSets>)>,
    disabled: bool,
}

impl Default for LiveBackdrop {
    fn default() -> Self {
        Self {
            worker: LiveWallpaper::new(),
            static_set: None,
            last_frame: None,
            applied: None,
            disabled: pecofence_core::brand::var("PECOFENCE_LIVE_WALLPAPER")
                .is_ok_and(|s| s == "0"),
        }
    }
}

impl App {
    pub(super) fn tick_live_wallpaper(&mut self) {
        let enabled = !self.live_wallpaper.disabled
            && self.wallpaper_override.is_none()
            && self.ctx.theme.borrow().liquid_glass
            && self.fences.values().any(|f| desktop::is_visible(f.hwnd()));
        self.live_wallpaper.worker.set_enabled(enabled);
        let frame = enabled
            .then(|| self.live_wallpaper.worker.take_latest())
            .flatten();
        if let Some(frame) = frame.filter(|f| f.captured_at.elapsed() < Duration::from_millis(500))
        {
            let mut hash = DefaultHasher::new();
            for image in &frame.images {
                (image.left, image.top, image.width, image.height).hash(&mut hash);
                image.bgra.hash(&mut hash);
            }
            let fingerprint = hash.finish();
            if self
                .live_wallpaper
                .applied
                .as_ref()
                .is_some_and(|(last, set)| {
                    *last == fingerprint
                        && set
                            .upgrade()
                            .is_some_and(|set| Rc::ptr_eq(&set, &self.ctx.backdrops.borrow()))
                })
            {
                // A paused WPE frame is a valid heartbeat: no uploads or redraws needed.
                self.live_wallpaper.last_frame = Some(frame.captured_at);
                return;
            }
            // A theme/topology refresh may have installed a new static set since the last
            // frame. Track it without ever caching a video frame as a static wallpaper.
            if let Some(sig) = &self.wallpaper_sig
                && let Some(static_set) = self.wallpaper_cache.get(sig)
            {
                self.live_wallpaper.static_set = Some(static_set);
            }
            let base = self
                .live_wallpaper
                .static_set
                .clone()
                .unwrap_or_else(|| self.ctx.backdrops.borrow().clone());
            let mut sources = base.acrylic.as_ref().clone();
            let mut replaced = 0;
            for captured in frame.images {
                let image = Image {
                    width: captured.width,
                    height: captured.height,
                    bgra: captured.bgra,
                };
                for source in &mut sources {
                    // Window coordinates are already screen pixels; do not apply wallpaper
                    // fill/fit or DPI scaling again. This also splits a spanning WPE surface.
                    if source.left >= captured.left
                        && source.top >= captured.top
                        && i64::from(source.left) + i64::from(source.width)
                            <= i64::from(captured.left) + i64::from(captured.width)
                        && i64::from(source.top) + i64::from(source.height)
                            <= i64::from(captured.top) + i64::from(captured.height)
                    {
                        let pixels = image.crop(
                            source.left - captured.left,
                            source.top - captured.top,
                            source.width,
                            source.height,
                        );
                        *source = MonitorBackdrop {
                            left: source.left,
                            top: source.top,
                            width: source.width,
                            height: source.height,
                            downscale: 1,
                            image: pixels,
                            gpu: None,
                        }
                        .into_gpu_backed(8, || None);
                        replaced += 1;
                    }
                }
            }
            if replaced > 0 {
                let set = Rc::new(BackdropSets {
                    acrylic: Rc::new(sources),
                });
                if self.live_wallpaper.last_frame.is_none() {
                    tracing::info!(
                        monitors = replaced,
                        fps = 1000 / self.live_wallpaper.worker.interval_ms,
                        "Wallpaper Engine live glass active"
                    );
                }
                self.live_wallpaper.last_frame = Some(frame.captured_at);
                self.live_wallpaper.applied = Some((fingerprint, Rc::downgrade(&set)));
                self.apply_live_backdrops(set);
                return;
            }
        }
        if self
            .live_wallpaper
            .last_frame
            .is_some_and(|at| !enabled || at.elapsed() > Duration::from_millis(1500))
        {
            self.live_wallpaper.last_frame = None;
            self.live_wallpaper.applied = None;
            if let Some(base) = self.live_wallpaper.static_set.take() {
                // Prefer a newly refreshed static set after a theme/topology change.
                let base = self
                    .wallpaper_sig
                    .as_ref()
                    .and_then(|sig| self.wallpaper_cache.get(sig))
                    .unwrap_or(base);
                self.apply_live_backdrops(base);
            }
            tracing::info!("live wallpaper unavailable/disabled; static glass restored");
        }
    }

    fn apply_live_backdrops(&mut self, set: Rc<BackdropSets>) {
        *self.ctx.backdrops.borrow_mut() = set.clone();
        for fence in self.fences.values() {
            fence.set_live_backdrops(set.clone());
        }
        self.ctx.bitmaps.borrow_mut().prune_wallpapers();
    }
}
