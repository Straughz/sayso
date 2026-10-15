# Sayso brand assets

The mark is a text cursor with three voice-level bars beside it: speech arrives where the cursor is. Colours: tile `#1b1d22`, ink `#f2efe8` for the cursor, amber `#e8a33d` for the three voice bars (`#b7791f` on the light tile, so the bars keep 3:1 against it). Wordmark: Bahnschrift SemiBold, "Sayso" with a capital S.

| File | Use |
|---|---|
| `sayso-mark.svg` | Master mark, dark tile. App icon, dark pages. |
| `sayso-mark-light.svg` | Light tile for light pages. |
| `sayso-mark-small.svg` | Same drawing on a 16-unit grid, full-bleed tile. Use at 24 px and below (tray). |
| `sayso-tile-1024.png` | Square project tile (LinkedIn media, the site). Rendered from `tile.html`. |
| `sayso-social-1280x640.png` | GitHub social preview. Rendered from `social.html`; needs Bahnschrift installed. |
| `../../src-tauri/icons/icon.ico` | 256, 48, 32 from the master; 24, 16 from the small variant. |

## Regenerate

Render each SVG at 1024 px with headless Chrome, then downscale for the ICO frames (Lanczos for 256/48/32/24, box for 16, where the small variant's edges land on whole pixels).

```
chrome --headless=new --disable-gpu --hide-scrollbars --window-size=1024,1024 --default-background-color=00000000 --screenshot=<out.png> <wrapper.html>
chrome --headless=new --disable-gpu --hide-scrollbars --window-size=1024,1024 --screenshot=sayso-tile-1024.png tile.html
chrome --headless=new --disable-gpu --hide-scrollbars --window-size=1280,640 --screenshot=sayso-social-1280x640.png social.html
```

Assemble the ICO with Pillow: open the 256 frame, `save("icon.ico", format="ICO", sizes=[(256,256),(48,48),(32,32),(24,24),(16,16)], append_images=[f48, f32, f24, f16])`. The 256 frame must be the base image or Pillow drops the larger sizes. After replacing the ICO, touch `src-tauri/tauri.conf.json` so the next `cargo build` re-embeds it.
