# Wallpaper Engine personal build

This fork adds live Wallpaper Engine backgrounds to **Liquid Glass**. Version
`0.1.5-we.3` is based on upstream PecoFence 0.1.5. This fork redesigns the GPU
optical surface: a 32 DIP elliptical shoulder replaces the narrow quartic bezel,
and a shallow dome gently magnifies the face by 2.4%. Broad directional highlights,
a faint white bounce and larger corners make the glass read as a rounded
volume. The black underside overlay from we.2 is removed. Reflection preserves
the wallpaper saturation and blends at 6%; it no longer
forms a highly saturated closed ring. Small controls scale the whole cross-section.
The geometry and light masks remain cached between wallpaper frames.

## Supported path

Verified on Windows 11 24H2 at 2560 × 1440 and 150% DPI, with the existing
**Hide real desktop icons** setting enabled. Windows Graphics Capture reads the
actual GPU-composited desktop root containing Wallpaper Engine. Modern Explorer
embeds WPE as a child window, which cannot be captured directly by CreateForWindow.
The root is eligible only when its real icon layer is hidden. Top-level fences and
foreground applications are outside that root and were absent from the captured
web, video and native DX11 scene frames. This is window capture, not a full-screen
screenshot or a read of the Windows wallpaper file.

Discovery recognizes `WPEDesktopCEFWindow`, `WPEDesktopDX11Window` and
`WPEVideoWallpaper`. A persistent worker was exercised through scene → web → video
→ scene, delivering 77/80/80/81 frames during the four-second phases; saved images
were inspected to verify actual wallpaper content, and the probe exited normally.
Black GPU frames are valid, including transitions. If real icons are visible,
the desktop-root path is refused; the old isolated PrintWindow web path can still
work, but native scene support under that configuration is not claimed.

While WPE remains running, a capture gap keeps the last live frame and retries,
rather than switching to the native Windows wallpaper after 1.5 seconds. A stale
frame is reported as reconnecting in the log. Before the first successful capture,
the initial static material can still be visible. Exiting WPE or disabling live
capture restores the static material. Fluent retains its static-wallpaper pipeline.

The app does not change Wallpaper Engine's playback settings. If it pauses for
maximized/full-screen applications, the glass keeps the current paused frame.

## Runtime behavior

- Enabled automatically for Liquid Glass; no config migration is required.
- Default update rate: 20 FPS. Set `PECOFENCE_WALLPAPER_FPS` before starting the
  app to choose 1–60 FPS. Higher rates increase CPU/GPU work.
- Set `PECOFENCE_LIVE_WALLPAPER=0` before starting to use the original static
  pipeline. An explicit `--wallpaper` override also disables live capture.
- Capture runs on a dedicated worker with a capacity-one mailbox. A blocked
  `PrintWindow` cannot block the UI. Shutdown allows up to 500 ms to drain GPU sessions
  and only joins a completed worker. The activated GraphicsCapture runtime is pinned
  until process exit to avoid a 24H2 asynchronous DLL unload crash observed in testing.
- Physical window coordinates map directly to monitor pixels, including a
  spanning surface and negative monitor origins. Uncovered monitors keep static
  backgrounds. Multi-monitor operation has not been exercised on physical hardware.
- GPU sessions/GDI surfaces are reused and rediscovered every 500 ms. Failed GPU
  sessions are recreated. Lens geometry is
  reused between frames; unchanged images skip GPU uploads and fence redraws.
- Capture suspends when no fence window is visible or the theme is Fluent.
- Update links point to this fork, so checking for updates cannot install the
  upstream build over this personal modification.

## Verification

```powershell
cargo build --locked --release -p pecofence -p pecofence-watchdog
cargo build --locked --release -p pecofence-cli
cargo fmt --all --check
cargo clippy -p pecofence -p pecofence-platform --all-targets -- -D warnings
cargo clippy -p pecofence-render --all-targets -- -D warnings
cargo test -p pecofence-render --lib
cargo test -p pecofence-core updates::tests
cargo run -p pecofence-platform --example live_wallpaper_probe -- .cache/live-probe
cargo run -p pecofence-platform --example live_wallpaper_probe -- .cache/gpu-probe --gpu
```

The diagnostic writes two real wallpaper frames, checks dimensions and opaque
BGRA pixels, and exercises worker delivery and suspension. It reports whether
the wallpaper moved; a paused wallpaper cannot validate motion.

`wallpaper_switch_probe` is an explicit integration tool that changes installed
wallpapers using WPE's CLI and restores the original on exit. Arguments are WPE
executable, original project, web project, video project, output directory. It
keeps one worker alive across all phases and saves each source image for inspection.
Uniform gray GPU rendering also checked material brightness: a 160 input produced
161 at the face and 162 on both sampled shoulders, with no black tint applied.

On the development machine, the capture worker delivered 80 frames in four
seconds, and the two visible-desktop samples differed. An isolated application
instance displayed the captured rain wallpaper with refracting glass edges and
survived switching Fluent → Liquid Glass. The installed build was then replaced
after backing up the original executable and configuration; the user confirmed
the real desktop fences display the Wallpaper Engine wallpaper. The revised
optics were compared using GPU renders over the same captured rain frame, including
a narrow tab strip. Regression checks cover DPI invariance, corner continuity,
the shoulder-to-face transition and directional reflection width.

A five-second sample of `0.1.5-we.1` with five installed fences measured about 1.4% of total CPU
capacity on a 20-logical-processor machine and a 109 MB working set. This is a
short local observation, not a guarantee for other wallpapers or hardware.

## Install and revert

Keep a copy of the original PecoFence program directory and
`%APPDATA%\PecoFence` before replacing an existing install. Exit PecoFence, then
replace `pecofence.exe`, `pecofence-watchdog.exe`, and `pecofence-cli.exe` together.
The existing `WebView2Loader.dll` and configuration remain compatible.

To revert, exit PecoFence and restore the three original binaries. Restore the
saved data directory only if you also want to discard later layout/settings
changes. Restart PecoFence from its existing shortcut.

For an independent portable installation, extract the **entire** portable ZIP and
start `pecofence.exe --portable`. See [PORTABLE.md](PORTABLE.md) for data paths.
