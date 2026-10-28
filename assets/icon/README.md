# SQLer app icon

`sqler.svg` is the only source. Everything else in this directory is generated from it and committed, so CI and the
packaging scripts need no rendering tool.

## Design

An original mark: a generic database cylinder (a lighter top ellipse over three bands) with a `>_` prompt on its
front, on a rounded square. It follows the dark workbench look of PR #63 and the `sqler` theme.

| Part | Colour |
| ---- | ------ |
| Body gradient | `#2f343e` to `#282c33`, stroke `#363c46` |
| Cylinder | `#74ade8` to `#5a8fd0`, top ellipse `#a6cbf5`, band gaps `#2c3038` |
| Prompt | `#dce0e5` with a `#282c33` halo |

Grid: 1024 x 1024 viewBox, the body is 824 x 824 at (100, 100) with radius 185 (the macOS icon grid). The SVG uses
paths, shapes and gradients only (no text, raster images or filters), because the renderer is built without them.

## Regenerate

```
cargo run -p xtask -- icons
```

Then commit the outputs. Rendering is not bit-identical across CPUs, so nothing in CI compares a fresh render with the
committed files; a test checks that the ICO, ICNS and PNGs agree with each other.

## Generated files

- `png/sqler-{16,22,24,32,48,64,128,256,512,1024}.png`: RGBA PNGs (the `.deb` installs 16 to 512 into hicolor).
- `sqler.ico`: 16, 24, 32, 48, 64 and 256 px, PNG entries (embedded into `sqler.exe`, used by the MSI).
- `sqler.icns`: PNG entries `icp4 icp5 icp6 ic07 ic08 ic09 ic10 ic11 ic12 ic13 ic14` (used by the `.dmg`).
- `../../docs/theme/favicon.svg` (a copy of `sqler.svg`) and `favicon.png` (32 px) for the mdbook site.
