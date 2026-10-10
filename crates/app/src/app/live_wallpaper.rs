//! Runtime live-wallpaper overlay, separate from the static virtual-desktop cache.
use super::*;
use pecofence_platform::live_wallpaper::{CONTRAST_SAMPLE_FACTOR, LiveWallpaper};

pub(super) struct LiveBackdrop {
    pub worker: LiveWallpaper,
    static_set: Option<Rc<BackdropSets>>,
    last_frame: Option<Instant>,
    applied: Option<(u64, Rc<BackdropSets>)>,
    waiting: bool,
    disabled: bool,
}

impl Default for LiveBackdrop {
    fn default() -> Self {
        Self {
            worker: LiveWallpaper::new(),
            static_set: None,
            last_frame: None,
            applied: None,
            waiting: false,
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
            let started = Instant::now();
            let fingerprint = frame.fingerprint;
            if self
                .live_wallpaper
                .applied
                .as_ref()
                .is_some_and(|(last, set)| {
                    *last == fingerprint && Rc::ptr_eq(set, &self.ctx.backdrops.borrow())
                })
            {
                // A paused WPE frame is a valid heartbeat: no uploads or redraws needed.
                self.live_wallpaper.last_frame = Some(frame.captured_at);
                self.live_wallpaper.waiting = false;
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
            for (captured, sample) in frame.images.into_iter().zip(frame.samples) {
                let image = Image {
                    width: captured.width,
                    height: captured.height,
                    bgra: captured.bgra,
                };
                // Common path: one captured desktop root and one monitor. Move its
                // buffer straight into the upload slot, with no full-screen copy or
                // box averaging on the animation/input thread.
                if let Some(source) = sources.iter_mut().find(|s| {
                    s.left == captured.left
                        && s.top == captured.top
                        && s.width == captured.width as i32
                        && s.height == captured.height as i32
                }) {
                    source.image = Image {
                        width: sample.width,
                        height: sample.height,
                        bgra: sample.bgra,
                    };
                    source.gpu = Some(pecofence_render::backdrop::GpuPixels::new(1, image, || {
                        None
                    }));
                    source.downscale = CONTRAST_SAMPLE_FACTOR;
                    replaced += 1;
                    continue;
                }
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
                self.live_wallpaper.waiting = false;
                self.live_wallpaper.applied = Some((fingerprint, set.clone()));
                let prepared = Instant::now();
                self.apply_live_backdrops(set);
                tracing::trace!(target: "pecofence::live_timing", prep_us = prepared.duration_since(started).as_micros(), draw_us = prepared.elapsed().as_micros(), "live wallpaper apply");
                return;
            }
        }
        if self
            .live_wallpaper
            .last_frame
            .is_some_and(|_| !enabled || !self.live_wallpaper.worker.engine_running())
        {
            self.live_wallpaper.last_frame = None;
            self.live_wallpaper.applied = None;
            self.live_wallpaper.waiting = false;
            if let Some(base) = self.live_wallpaper.static_set.take() {
                // Prefer a newly refreshed static set after a theme/topology change.
                let base = self
                    .wallpaper_sig
                    .as_ref()
                    .and_then(|sig| self.wallpaper_cache.get(sig))
                    .unwrap_or(base);
                self.apply_live_backdrops(base);
            }
            tracing::info!(
                "Wallpaper Engine stopped or live capture disabled; static glass restored"
            );
        } else if enabled && self.live_wallpaper.worker.engine_running() {
            // A transition or temporary capture failure must never silently change the
            // material to the Windows wallpaper. Also protect against an OS wallpaper
            // notification temporarily replacing the context with a newly decoded static set.
            if let Some((_, set)) = &self.live_wallpaper.applied
                && !Rc::ptr_eq(set, &self.ctx.backdrops.borrow())
            {
                self.apply_live_backdrops(set.clone());
            }
            if !self.live_wallpaper.waiting
                && self
                    .live_wallpaper
                    .last_frame
                    .is_some_and(|at| at.elapsed() > Duration::from_secs(2))
            {
                self.live_wallpaper.waiting = true;
                tracing::warn!("Wallpaper Engine capture reconnecting; retaining last live frame");
            }
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
