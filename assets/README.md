# Brand assets

The mark is a tardigrade whose humped, layered back curls like a croissant — the
flake layers of the pastry and the segments of the animal are the same curves. A
water bear survives conditions that kill everything around it, which is roughly
the job description of a sandbox that has to contain whatever an agent writes.

| File | What it is |
|---|---|
| `logo.svg` | The source. One path element, 5 subpaths, 394 points, 6.0 KB, `viewBox 0 0 64 64`. |
| `logo.png` | 512x512 render of that exact geometry. |

Ink is navy `#182749` on a transparent background.

## Two rules this file depends on

Both of these were bugs here first, so they are written down rather than just
fixed:

- **Every contour is a subpath of ONE `<path>`.** `fill-rule="evenodd"` applies
  per path element, so splitting the contours across several paths does not
  cancel the holes — it fills them. That version rendered 4.7x too much ink
  (26.8% of the canvas instead of 5.7%).
- **The fill is a literal colour, not `currentColor`.** Renderers other than
  browsers, ImageMagick among them, paint `currentColor` as nothing at all, so
  the file appeared blank. To follow the surrounding text colour instead, swap
  `fill="#182749"` for `fill="currentColor"` and set `color` on the root element.

## Verified

- **Matches the approved raster.** Chromium's render of `logo.svg` against
  `logo.png`: RMSE 0.0056, and zero pixels differing by more than 15% grey.
- **Renders outside browsers.** ImageMagick now draws it (3.1% ink versus
  Chromium's 5.7%; its own SVG renderer is cruder, but it no longer draws
  nothing).
- **Valid XML.** One `<path>`, 5 subpaths, `fill-rule="evenodd"`.
- **Trace fidelity.** 0.8% of the bounding box differs from the generated
  artwork, all on antialiased edges. That is 5.5% of the ink, because the
  strokes are thin and so simplification error is a larger share of a smaller
  total.
- **Islands preserved.** Three separate ink islands were traced. An earlier
  version of the tracer kept only the largest and silently deleted a detached
  piece.

## Known limitation

At 16 px the mark holds **no** dark pixels — the strokes blur to a light grey,
and outline art cannot survive favicon scale. Use it at 24 px and above. A filled
silhouette variant would solve this, and one was produced, but it was rejected,
so none is shipped.

## Provenance

Artwork generated with Google Gemini 3 Pro Image (via OpenRouter), conditioned on
a reference icon supplied by the project owner (in
`~/Downloads/`), to match its bold-outline, flat-monochrome style. The reference
is a stock icon and is not redistributed here; only the derived mark is.

The mark was then converted to a genuine vector source rather than shipped as a
raster: crop clear of the generator's edge frame, threshold to one ink colour,
drop specks and fill pinholes below 250 px, trace every ink island by contour
following, simplify with Douglas-Peucker at 1.6 px, and emit all contours as
subpaths of one path with `fill-rule="evenodd"`. The PNG is rendered from the
same polygons, so raster and vector cannot drift apart. No external tracer was
used — `potrace`, `autotrace` and `mkbitmap` are not installed on the build
host.

## Licence

Copyright 2026 zbrdc, Apache-2.0, same as the project.

