# App icon decision

**Chosen: option 2, the Shelf at the edge** (`docs/mockups/icon-tasca.svg`).
The other four prototypes (pocket, stack, clip, drop tray) and the side-by-side
page are in `prototype/icon/`; they are throwaway.

## Why

It draws what Tasca does: a dark strip for the screen's left edge, with the
Shelf (a rust panel with three rows) sliding out of it. It uses the same tile
geometry, ivory tile and rust/dark-brown palette as the quill icon of the
sibling app scrivi.

## What the build ticket must do

- **Small variant for 16 and 20 px.** At 16 px the dark strip nearly vanishes
  and the three rows blur. Draw a `tasca-small.svg` (wider strip, two thicker
  rows, no gloss), as scrivi does, and use it for the small sizes.
- **Multi-size `assets/tasca.ico`**: 16, 20, 24, 32, 48, 256 px, PNG entries,
  generated from the SVGs by a script (headless Edge/Chrome plus .NET, no new
  dependency). Replace the current icon; `assets/tasca.rc` already points at it.
- **Tray icon**: use the 16 px (and 20/24 for high DPI) entries so the tray
  shows the small variant. Check it on both light and dark taskbars.
- **README**: show the icon, and mention it only if a section lists assets.
- **Open choice**: reuse scrivi's exact palette across apps for a brand
  identity (as drawn here), or diverge. Confirm before building.
