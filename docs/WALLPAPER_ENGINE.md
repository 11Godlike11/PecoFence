# Wallpaper Engine personal build

This fork adds live Wallpaper Engine backgrounds to **Liquid Glass**. Version
`0.1.5-we.2` is based on upstream PecoFence 0.1.5. This revision redesigns the GPU
optical surface: a 32 DIP elliptical shoulder replaces the narrow quartic bezel,
and a shallow dome gently magnifies the face by 2.4%. Broad directional highlights,
a restrained underside shade and larger corners make the glass read as a rounded
volume. Reflection tint is slightly desaturated and blends at 6%; it no longer
forms a highly saturated closed ring. Small controls scale the whole cross-section.
The geometry and light masks remain cached between wallpaper frames.

## Supported path

Verified with a Chromium/web wallpaper on Windows 11, at 2560 × 1440 and 150% DPI.
The source is Wallpaper Engine's `Intermediate D3D Window` beneath its
`WPEDesktopCEFWindow`. The app captures that surface with `PrintWindow`, rather
than reading the desktop screen. Fences, desktop icons and foreground applications
are therefore not included in the glass input.

Native scene/video desktop surfaces are discovered too, but have **not** been
validated. Renderers that print black, inaccessible surfaces, and Wallpaper Engine
being stopped fall back to the existing Windows static-wallpaper material after
1.5 seconds. A genuinely all-black wallpaper frame is also treated as unavailable
by this capture backend. Fluent still uses the upstream static-wallpaper pipeline.

The app does not change Wallpaper Engine's playback settings. If it pauses for
maximized/full-screen applications, the glass keeps the current paused frame.

## Runtime behavior

- Enabled automatically for Liquid Glass; no config migration is required.
- Default update rate: 20 FPS. Set `PECOFENCE_WALLPAPER_FPS` before starting the
  app to choose 1–60 FPS. Higher rates increase CPU/GPU work.
- Set `PECOFENCE_LIVE_WALLPAPER=0` before starting to use the original static
  pipeline. An explicit `--wallpaper` override also disables live capture.
- Capture runs on a dedicated worker with a capacity-one mailbox. A blocked
  `PrintWindow` cannot block the UI. Shutdown never waits for that foreign window.
- Physical window coordinates map directly to monitor pixels, including a
  spanning surface and negative monitor origins. Uncovered monitors keep static
  backgrounds. Multi-monitor operation has not been exercised on physical hardware.
- GDI surfaces are reused and rediscovered every two seconds. Lens geometry is
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
```

The diagnostic writes two real wallpaper frames, checks dimensions and opaque
BGRA pixels, and exercises worker delivery and suspension. It reports whether
the wallpaper moved; a paused wallpaper cannot validate motion.

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
