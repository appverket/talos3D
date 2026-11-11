# Chrome symbol fallback

`TalosChromeSymbols.ttf` is a subset of DejaVu Sans 2.37, used only when the
application fonts lack keyboard modifiers, arrows or checkmarks. It is bundled
for identical offline/native/web coverage; no installed system font is required.
The existing body and monospace fonts remain the primary fonts.

Upstream: https://dejavu-fonts.github.io/Download.html
Archive: https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/dejavu-sans-ttf-2.37.zip
Archive SHA256: `5c6e497a2f36552cb5ffb112c413a6af39c0f3c47653662b90b4fa6499822fd7`
License: [DejaVu / Bitstream Vera / Arev](LICENSE-DejaVu.txt), reproduced in full.

Regenerate with FontTools 4.60+ after extracting the official archive:

```sh
pyftsubset dejavu-sans-ttf-2.37/ttf/DejaVuSans.ttf \
  --unicodes=U+2190-23FF,U+2500-27FF \
  --output-file=TalosChromeSymbols.ttf \
  --name-IDs='*' --name-legacy --name-languages='*'
```

The subset retains upstream naming, copyright and licensing metadata. It contains
no glyph design changes. The menu presentation tests verify actual egui glyph
coverage, row alignment, hover feedback, disabled input and keyboard activation.
