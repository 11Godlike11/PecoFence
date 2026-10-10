# Personal Liquid Glass renderer (we.6)

This is a Windows Direct2D approximation of Apple's material, not Apple's shader
or a demonstrated pixel-perfect reproduction. The current renderer was rebuilt
after the earlier broad highlights, face magnification and circular corners did
not give the desired appearance. We have not run a paired native iOS image test.

## Geometry and optics

- The fence uses a rounded rectangle with order-2.5 superellipse corners. Each
  quarter has 64 segments; optical distance is measured against that polygon.
  Analytic normals smooth out facet highlights. This is our own approximation,
  rather than a port of Flutter's rounded-superellipse implementation. During
  folding, the order smoothly decreases to 2 as opposing corners meet, producing
  circular capsule ends without a sudden shape change. The material mask, Windows
  Composition path clip, outer shadow and selection ring share the same geometry,
  including the folded fence's vertical offset.
- The corner radius is 42 DIP. The optical bevel is 20 DIP, reduced for small
  controls. The center is flat: there is no whole-face magnification.
- For inward depth `s` and bevel width `b`, edge displacement is proportional to
  `1 - sqrt(1 - (1 - clamp(s / b, 0, 1))^2)`. Its maximum is 2.2 times the bevel
  width, limited to half the short side. Samples move inward along the normal.
  This produces a curved shoulder with displacement returning to zero at the
  flat face. Chromatic dispersion is disabled.
- The interior mixes in a 1.15 DIP Gaussian blur, fading toward the bevel where
  sharper wallpaper detail makes refraction visible. Small controls reduce this
  blur. This is subtle frost, not an opaque dark tint.
- A 1.2 DIP directional glint follows the shoulder, with 21% inward bleed over
  four times that width. It fades at light-tangent edges. Glint color derives
  from the displaced wallpaper, with a limited lift. A separate contour and
  short inner shadow are limited by a 5.5% opacity; there is no full-face black
  overlay. Existing user opacity/tint and floating-fence backing still apply.

Geometry, displacement and coverage/frost/light masks are cached by size, radius
and DPI. Wallpaper uploads, Gaussian blur, displacement, masks and compositing
run on Direct2D effects. Geometry maps are not recomputed per wallpaper frame.
The CPU compatibility renderer is retained and does not have these new optics;
verification images must explicitly select the GPU renderer.

Map generation skips the flat interior and reuses precomputed polygon segments.
The CPU shadow obtains antialiased coverage from the same corner polygon using
a binary sector lookup. Liquid Glass labels use adaptive foreground colors
without the former four-offset white/black halo.
The single-title hover state and fold glyph no longer add an independent glass
capsule or circular rim across the fence corners. Hover/press feedback remains
in the outer material and glyph color; multi-tab lenses retain their tab feedback.

## Sources and attribution

The bevel displacement model was adapted from Kyant0's Apache-2.0 licensed
[AndroidLiquidGlass / Backdrop shaders](https://github.com/Kyant0/AndroidLiquidGlass/blob/kmp/backdrop/src/commonMain/kotlin/com/kyant/backdrop/internal/Shaders.kt).
Its [continuous corner discussion](https://kyant.gitbook.io/backdrop/tutorials/smoother-rounded-corners)
also informed the shape choice.

The flat face, narrow directional glint, tangency fade and small inward bleed
were informed by whynotmake-it's Apache-2.0 licensed
[liquid_glass_renderer](https://github.com/whynotmake-it/flutter_liquid_glass/tree/e4086e5e38151fe65172225d79fe4a516585a933/packages/liquid_glass_renderer),
especially its `geometry_fragment.glsl` and `liquid_glass_final_render_core.glsl`.
That revision targets iOS 27 and contains a native Apple comparison harness.
Its test results do not establish fidelity for this Windows implementation.
We adapted formulas and material structure, not the entire Flutter renderer.

Both projects are Apache-2.0 licensed, as is PecoFence. The PecoFence source
license remains in the package. These modifications and the sources above are
recorded here; upstream authors do not endorse this personal build.

Apple's [Meet Liquid Glass](https://developer.apple.com/videos/play/wwdc2025/219/)
describes the intended adaptive behavior. It does not provide a portable public
shader for Windows.

## Validation

`cargo test -p pecofence-render --lib` covers the common silhouette, signed edge
distance, tangent joins, small controls, DPI invariance, flat center and edge
lighting. `gpu_glass_bench` exercises actual GPU output: opacity, neutral color,
live source replacement, negative monitor coordinates, scaled source mapping,
geometry cache invalidation and control refraction. Its checkerboard test expects
neutral frosting rather than an unchanged sharp center.

`liquid_glass_wallpaper` renders actual GPU preview images using saved wallpaper
frames. Uniform gray, rain and a sunset scene are used to inspect brightness and
edge artifacts. These previews do not substitute for checking the installed
window, its compositor clip and the user's visual acceptance.

For we.5, paired native debug builds with the same optimization profile, saved
wallpaper, 800 x 1000 pixel fence and six fold/unfold cycles measured resize draw
time at median 7.51 -> 3.97 ms and p95 12.20 -> 6.88 ms. Those figures exclude
the separate CPU shadow stage, whose median was 2.04 -> 2.20 ms. They are local
stage timings, not a release FPS guarantee. Native input checks exercised
hover-open followed by arrow-close, suppression of hover reopening after manual
collapse, and reversing an in-flight fold by clicking again. Geometry tests
cover intermediate fold heights and agreement between shadow coverage and clip.

The we.6 header change was compared before/after at 125% DPI over the same saved
wallpaper using the GPU preview's `title-hover` mode. It removes the separate
header capsule and fold-button circle without changing the outer optical model.
