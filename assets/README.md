# Brand assets

The mark is a single-colour tardigrade for tardigrade-runner. A water bear
survives conditions that kill everything around it, which is roughly the job
description of a sandbox that has to contain whatever an agent writes.

Containment lives in the silhouette rather than beside it. The body is squared
off into four rounded segments whose seams are negative space: up close they
read as the walls of a container, and at favicon size they survive as the
divisions of the outline. Nothing is drawn inside the shape, so the artwork
carries no background of its own and the same file works on light and dark
surfaces.

## Files

| File | What it is |
|---|---|
| `logo.svg` | The source. Five paths, 166 points, 3.0 KB, `viewBox 0 0 64 64`. Scales to any size. |
| `logo.png` | 512x512 render of that exact geometry, for places that need a raster: the repository avatar, listings, favicons. |

One ink colour, transparent background. The SVG fills with `currentColor` and
sets `color="#182749"` on its root, so it renders navy everywhere; delete that
attribute and it follows the surrounding text colour instead.

## Verified

- **Four legs.** Counted from the raster as ink runs across the leg band:
  `[4, 4, 4, 4, 4]`, consistent at every scanline. Earlier candidates measured
  three, five, or hid the legs behind a container and could not be measured at
  all.
- **Trace fidelity.** The traced polygons differ from the generated artwork in
  0.4% of the mark's bounding box, all of it edge antialiasing.
- **Independent render.** `logo.svg` is valid XML; Chromium and the project's own
  rasteriser agree on ink coverage within half a percentage point and both paint
  `#182749`.
- **Favicon scale.** At 32x16 the outline still shows the segmentation and the
  four legs; only the fine claw detail disappears, which the silhouette does not
  depend on.

## Provenance

Artwork generated with Google Gemini 3 Pro Image (via OpenRouter), then converted
into a genuine vector source rather than shipped as a raster:

1. threshold to one ink colour,
2. drop specks below 250 px and fill pinholes,
3. trace each ink island by contour following,
4. simplify with Douglas-Peucker at 1.6 px tolerance,
5. emit one path per contour, `fill-rule="evenodd"` so the seams and the eye are
   holes and no background colour is baked in.

`logo.png` is rendered from those same polygons, so the two files cannot
disagree. No external tracer was used — `potrace`, `autotrace` and `mkbitmap`
are not installed on the build host, and the tracing script is not committed.

## Licence

Copyright 2026 zbrdc, Apache-2.0, same as the project.

