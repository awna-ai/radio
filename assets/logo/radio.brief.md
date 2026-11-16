# Logo: provenance

Two files:

- `radio.svg`: the logo, on its navy tile. Use it anywhere, on light or dark.
- `radio-mark.svg`: the arcs and the dot alone, with no tile, for dark surfaces only. Against
  white its sky blue and amber reach about 2.1:1, under the 3:1 that graphics need; against
  `#0F172A` or `#0D1117` they reach 8.3:1 or more.

- **Chosen** on 2026-11-16 from round 1. The montage showed four concepts at 128 px and at 32 px, on
  light and dark backgrounds: three voices whose arcs meet, three overlapping rings, this signal
  mark, and a speech bubble with arcs. The signal mark was picked.
- **Generator:** Recraft, model `recraftv4_1_vector`, 1:1, n=1. A first wording of the same concept
  ran the arcs off the tile's edge; the prompt below keeps them inside.
- **Prompt:**
  > Minimal geometric app icon symbol. One rounded-square tile, solid slate navy (#0F172A),
  > transparent background outside the tile. Centred on the tile, well inside its edges: one small
  > amber (#F59E0B) dot, with two short thick sky-blue (#38BDF8) arcs to its left and two to its
  > right, opening outward, all of it fitting in the middle half of the tile. Flat vector, bold
  > thick shapes, few elements, no text, no hairlines, wide empty margin around the symbol,
  > legible at 32 px.
- **Palette:** slate navy `#0F172A` (tile), sky blue `#38BDF8` (arcs), amber `#F59E0B` (dot). The
  README's diagrams use the same three.

## Derivation

1. Remove the C2PA metadata and the white canvas path that Recraft adds outside the tile.
2. Map each fill to the nearest palette hex: the generator returned near misses
   (`rgb(60,141,231)` for the sky blue, `rgb(15,34,50)` for the navy, `rgb(252,172,66)` for the amber).
3. Crop the viewBox to the tile, square (`602 602 3162 3162`).
4. Round path coordinates to whole units, drop the no-op `translate(0,0)` transforms, and add
   `role="img"` with a `<title>`. Rendered at 256 px, the result differs from the uncleaned file
   by 0.18 % RMSE.
5. `radio-mark.svg`: drop the tile path, then crop the viewBox square around the arcs and the dot
   with a margin of a twelfth of their size (`1047 1045 2272 2272`).

## Checked

`radio.svg` at 128 px, 96 px and 32 px, on white and on `#0D1117`. The arcs and the dot stay
separate at 32 px; the navy tile is faint against a near-black page, as an icon tile on a dark
page usually is. `radio-mark.svg` at 128 px and 32 px on white, `#0D1117` and `#0F172A`.
