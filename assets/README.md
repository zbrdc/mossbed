# Brand assets

The tardigrade is the mascot for tardigrade-runner. A water bear survives
conditions that kill everything around it, which is roughly the job
description of a sandbox that has to contain whatever an agent writes.

## Files

| File | Size | Use |
|---|---|---|
| `logo.png` | 508x508 | The mark, centred with a 28% margin. Serves as the README header and the repository avatar; that margin keeps the mark inside the inscribed circle, so a circular avatar crop does not clip it. |

It is a 24-colour PNG under 8 KB with the licence in its metadata. The
background is opaque white; on a dark page it renders as a white plate.

## Palette

| Colour | Hex | Role |
|---|---|---|
| Teal | `#49AFA6` | Body |
| Navy | `#182749` | Outline, eye pupil |
| Cream | `#F5E9C8` | Belly, eye |
| White | `#FFFFFF` | Background |

## Provenance

Generated with Google Gemini 3 Pro Image (via OpenRouter), then processed:
dropped the dark hairline frame the generator draws at the canvas edge,
normalised the off-white background to pure white, trimmed to the mark,
quantised to 24 colours without dithering, and resampled by exact integer
factor so the edges stay clean.

There is no vector source: `potrace`, `autotrace` and `mkbitmap` are not
installed, so a scalable version has to be drawn, not traced. Replacing the
mark means generating a new one and re-running those steps.

## Licence

Copyright 2026 zbrdc, Apache-2.0, same as the project.

