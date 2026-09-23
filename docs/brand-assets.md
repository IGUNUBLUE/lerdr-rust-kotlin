# Brand assets

Iguana identity, one design family across all surfaces. Artwork is
project-generated (AI) and lives in `app/app/src/main/res/`.

## Adaptive launcher icon

`mipmap-anydpi-v26/ic_launcher.xml` composes three layers over a
108×108dp canvas. Everything important must sit inside the **Ø66% safe
zone** (central circle) — launchers apply circle/squircle/rounded-square
masks that clip the rest. Since `minSdk 28`, adaptive icons cover every
device; no per-density mipmaps are needed. There is intentionally no
`ic_launcher_round` — the system masks the same icon.

| Layer | File | Content |
|---|---|---|
| background | `drawable-nodpi/ic_launcher_jungle.png` | Full-bleed jungle foliage. No subject, no rings, no baked-in borders. |
| foreground | `drawable-nodpi/ic_launcher_gecko.png` | Color iguana head on **transparent**, sized ~64% of canvas so the whole subject stays inside the safe zone. |
| monochrome | `drawable-nodpi/ic_launcher_gecko_mono.png` | Same head as a flat silhouette (alpha is what counts — the system tints it for themed icons). |

Working size: **1024×1024 px** per layer.

## Notification icon

`drawable-<density>/ic_notification.png` — white silhouette of the same
iguana head, alpha-only (the system tints it). One PNG per density at
24dp: mdpi 24px, hdpi 36px, xhdpi 48px, xxhdpi 72px, xxxhdpi 96px.

## Splash screen

Android 12+ derives the launch splash from the adaptive icon
automatically — fixing the launcher icon fixes the splash. The base
theme stays dark (`Theme.Lerdr`).

## Regenerating

Source art is produced externally (e.g. image generation) at ≥1024².
Pipeline used for the current set (ImageMagick):

```sh
# background — full bleed, just resize
magick jungle.png -resize 1024x1024 ic_launcher_jungle.png

# foreground — remove flat backdrop from the corners, trim, scale to
# ~64% canvas, center on transparent 1024²
magick head.png -alpha set -channel RGBA -fuzz 6% \
  -fill none -draw 'alpha 0,0 floodfill' -draw 'alpha 1023,0 floodfill' \
  -draw 'alpha 0,1023 floodfill' -draw 'alpha 1023,1023 floodfill' \
  -trim +repage -resize 655x655 fg.png
magick -size 1024x1024 xc:none fg.png -gravity center -composite \
  ic_launcher_gecko.png

# monochrome — silhouette luminance becomes alpha
magick silhouette.png -alpha off -colorspace gray -negate mask.png
magick -size 1024x1024 xc:black mask.png -alpha off \
  -compose CopyOpacity -composite -trim +repage -resize 655x655 mono.png
magick -size 1024x1024 xc:none mono.png -gravity center -composite \
  ic_launcher_gecko_mono.png

# notification — white silhouette at each 24dp density
magick white_on_black.png -alpha off -colorspace gray mask.png
magick -size 1024x1024 xc:white mask.png -alpha off \
  -compose CopyOpacity -composite -trim +repage -resize 48x48 \
  -gravity center -background none -extent 48x48 \
  drawable-xhdpi/ic_notification.png
```

## Still open

- In-app wordmark/logo (Settings → About) — none today.
- Play Store listing (if ever published): hi-res icon 512×512 full-bleed,
  feature graphic 1024×500.
- GitHub social preview 1280×640.
