# Brand assets

Two versions of one mark: a tardigrade whose humped, layered back curls like a
croissant. The water bear survives conditions that kill everything around it,
which is roughly the job description of a sandbox that has to contain whatever
an agent writes.

The split is not decorative. Line art and filled art fail differently at small
sizes, and this project needs both a README-sized mark and an avatar-sized one.

| File | Use | Dark pixels at 128 px | 32 px | 16 px |
|---|---|---|---|---|
| `logo.svg` / `logo.png` | Outline mark. README header, any large use. | 3.9% | 0.9% | 0% |
| `icon.svg` / `icon.png` | Filled mark. Repository avatar, favicons, listings. | 13.5% | 14.4% | 13.9% |

Those columns are measured, not estimated: each file downscaled and then
thresholded at 50% grey, counting pixels that stay genuinely dark. At 16 px the
outline mark keeps nothing — its strokes blur to a 56%-grey ghost — while the
filled mark keeps 13.9% of its area dark with pure black intact. That is why the
reference icon ships two figures, and why an "editable stroke" icon can never be
the whole answer for a favicon.

## Files

| File | What it is |
|---|---|
| `logo.svg` | Outline mark. 5 paths, 399 points, 5.7 KB, `viewBox 0 0 64 64`. |
| `logo.png` | 512x512 render of that exact geometry. |
| `icon.svg` | Filled mark. 1 path, 181 points, 3.0 KB. |
| `icon.png` | 512x512 render of that exact geometry. |

One ink colour, transparent background. Both SVGs fill with `currentColor` and
set `color="#182749"` on their root, so they render navy anywhere; delete that
attribute and they follow the surrounding text colour instead.

## Palette

| Colour | Hex | Role |
|---|---|---|
| Navy | `#182749` | The single ink colour of both marks |

Monochrome by choice: one colour survives being printed, etched, reversed out of
a dark terminal, or rendered as a favicon, without a second version.

## Verification

- **Trace fidelity.** Filled mark: 0.5% of the bounding box differs from the
  generated artwork, all on antialiased edges. Outline mark: 0.8% of the box
  (5.5% of ink, since its strokes are thin, so simplification error is a larger
  share of a smaller total).
- **No edge contamination.** Both masks were shaved clear of the generator's
  hairline frame and confirmed not to touch the canvas edge before tracing.
- **Islands preserved.** The outline mark has three separate ink islands (the
  stroke network plus two detached marks). The tracer keeps every island above
  250 px; an earlier version silently kept only the largest and deleted a head.
- **Independent render.** Both SVGs are valid XML and render in Chromium with the
  ink colour resolving to `srgb(24,39,73)`.

## Provenance

Artwork generated with Google Gemini 3 Pro Image (via OpenRouter), conditioned on
a reference icon supplied by the project owner
(`~/Downloads/CcCl6jjN21M5e04QvT8IL-nrQDUwYTONNToPrMEZkHK2tpUrpgHEgFzFcD20TwOH-sy0jdyznu3UMB17FN-I3cIYn5z3SiDByT8uCHFbu8DIgBcsI8TEDP_pS1jFyWGnk9Ee3BWmw6GZdiKYNbi7cFsgBoac0mUq4L0kcO3N4rTOaYmIl1_-Dce2g7UJFBth.jpeg`)
to match its bold-outline, flat-monochrome style. The reference itself is a stock
icon and is not redistributed here; only the derived marks are.

Each mark was then converted to a genuine vector source rather than shipped as a
raster:

1. crop clear of the generator's edge frame, threshold to one ink colour,
2. drop specks below 250 px, fill pinholes below the same size,
3. trace every ink island by contour following,
4. simplify with Douglas-Peucker at 1.6 px tolerance,
5. emit one path per contour, `fill-rule="evenodd"`, so the internal seams are
   holes and no background colour is baked in.

The PNGs are rendered from those same polygons, so raster and vector cannot
drift apart. No external tracer was used: `potrace`, `autotrace` and
`mkbitmap` are not installed on the build host.

Known limitation: because the outline mark was traced, its strokes are closed
shapes rather than a stroked path, so its line weight cannot be retuned by
editing a `stroke-width`. That would require skeletonising the strokes to
centrelines and fitting curves to them.

## Licence

Copyright 2026 zbrdc, Apache-2.0, same as the project.

