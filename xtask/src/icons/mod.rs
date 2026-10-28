// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `icons` task: renders `assets/icon/sqler.svg` into PNG, ICO, ICNS and the docs favicon.

mod container;

use std::path::Path;
use std::process::ExitCode;

use resvg::{tiny_skia, usvg};

use crate::plugins::build::repo_root;

/// Every PNG size written to `assets/icon/png`.
const PNG_SIZES: [u32; 10] = [16, 22, 24, 32, 48, 64, 128, 256, 512, 1024];
/// The sizes stored in the ICO.
const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];
/// ICNS PNG types: `(OSType, pixel size)`.
const ICNS_TYPES: [([u8; 4], u32); 11] = [
    (*b"icp4", 16),
    (*b"icp5", 32),
    (*b"icp6", 64),
    (*b"ic07", 128),
    (*b"ic08", 256),
    (*b"ic09", 512),
    (*b"ic10", 1024),
    (*b"ic11", 32),
    (*b"ic12", 64),
    (*b"ic13", 256),
    (*b"ic14", 512),
];
/// Pixel size of the docs favicon PNG.
const FAVICON_SIZE: u32 = 32;

/// Run `icons`; never panics. Exit codes: 0 ok, 1 failure, 2 usage error (an argument was given).
pub fn run(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        eprintln!("error: `icons` takes no arguments");
        eprintln!("usage: cargo run -p xtask -- icons");
        return ExitCode::from(2);
    }
    match generate(&repo_root()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(1)
        }
    }
}

/// Render `svg` at `size` x `size` pixels and encode it as PNG.
pub(crate) fn render_png(svg: &[u8], size: u32) -> Result<Vec<u8>, String> {
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default())
        .map_err(|e| format!("cannot parse the SVG: {e}"))?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)
        .ok_or_else(|| format!("cannot allocate a {size}x{size} pixmap"))?;
    let tree_size = tree.size();
    #[allow(clippy::cast_precision_loss, reason = "icon sizes are small")]
    let transform = tiny_skia::Transform::from_scale(
        size as f32 / tree_size.width(),
        size as f32 / tree_size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap
        .encode_png()
        .map_err(|e| format!("cannot encode the PNG: {e}"))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn generate(root: &Path) -> Result<(), String> {
    let icon_dir = root.join("assets").join("icon");
    let svg_path = icon_dir.join("sqler.svg");
    let svg =
        std::fs::read(&svg_path).map_err(|e| format!("cannot read {}: {e}", svg_path.display()))?;

    let png_dir = icon_dir.join("png");
    std::fs::create_dir_all(&png_dir)
        .map_err(|e| format!("cannot create {}: {e}", png_dir.display()))?;
    let mut pngs: Vec<(u32, Vec<u8>)> = Vec::new();
    for size in PNG_SIZES {
        let png = render_png(&svg, size)?;
        write(&png_dir.join(format!("sqler-{size}.png")), &png)?;
        pngs.push((size, png));
    }
    let find = |size: u32| -> Result<Vec<u8>, String> {
        pngs.iter()
            .find(|(s, _)| *s == size)
            .map(|(_, p)| p.clone())
            .ok_or_else(|| format!("no {size} px render"))
    };

    let ico_entries = ICO_SIZES
        .iter()
        .map(|&s| Ok((s, find(s)?)))
        .collect::<Result<Vec<_>, String>>()?;
    write(
        &icon_dir.join("sqler.ico"),
        &container::write_ico(&ico_entries),
    )?;

    let icns_entries = ICNS_TYPES
        .iter()
        .map(|&(kind, s)| Ok((kind, find(s)?)))
        .collect::<Result<Vec<_>, String>>()?;
    write(
        &icon_dir.join("sqler.icns"),
        &container::write_icns(&icns_entries),
    )?;

    let theme = root.join("docs").join("theme");
    std::fs::create_dir_all(&theme)
        .map_err(|e| format!("cannot create {}: {e}", theme.display()))?;
    write(&theme.join("favicon.svg"), &svg)?;
    write(&theme.join("favicon.png"), &find(FAVICON_SIZE)?)?;
    println!("wrote assets/icon/** and docs/theme/favicon.*");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8" viewBox="0 0 8 8"><rect width="8" height="8" fill="#ff0000"/></svg>"##;

    fn be32(b: &[u8]) -> u32 {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }

    fn ihdr(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        (be32(&png[16..20]), be32(&png[20..24]))
    }

    #[test]
    fn render_png_has_requested_size_and_pixels() {
        let png = render_png(TINY, 16).unwrap();
        assert_eq!(ihdr(&png), (16, 16));
        let pixmap = tiny_skia::Pixmap::decode_png(&png).unwrap();
        assert!(pixmap.pixels().iter().any(|p| p.alpha() > 0));
    }

    #[test]
    fn run_rejects_arguments() {
        assert_eq!(run(&["x".to_owned()]), ExitCode::from(2));
    }

    #[test]
    fn committed_assets_are_consistent() {
        let root = repo_root();
        let icon = root.join("assets").join("icon");
        let read = |p: std::path::PathBuf| std::fs::read(&p).unwrap();
        let png = |n: u32| read(icon.join("png").join(format!("sqler-{n}.png")));
        for n in PNG_SIZES {
            assert_eq!(ihdr(&png(n)), (n, n), "png {n}");
        }

        let ico = read(icon.join("sqler.ico"));
        assert_eq!(&ico[..4], &[0, 0, 1, 0]);
        let count = usize::from(u16::from_le_bytes([ico[4], ico[5]]));
        let mut sizes = Vec::new();
        for i in 0..count {
            let e = &ico[6 + 16 * i..22 + 16 * i];
            let size = if e[0] == 0 { 256 } else { u32::from(e[0]) };
            assert_eq!(e[0], e[1]);
            assert_eq!(u16::from_le_bytes([e[6], e[7]]), 32);
            let len = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
            let off = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
            assert_eq!(ico[off..off + len], png(size)[..], "ico entry {size}");
            sizes.push(size);
        }
        assert_eq!(sizes, ICO_SIZES);

        let icns = read(icon.join("sqler.icns"));
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(be32(&icns[4..8]) as usize, icns.len());
        let mut pos = 8;
        let mut kinds = Vec::new();
        while pos < icns.len() {
            let kind: [u8; 4] = icns[pos..pos + 4].try_into().unwrap();
            let len = be32(&icns[pos + 4..pos + 8]) as usize;
            let (_, size) = ICNS_TYPES.iter().find(|(k, _)| *k == kind).unwrap();
            assert_eq!(icns[pos + 8..pos + len], png(*size)[..], "icns entry");
            kinds.push((kind, *size));
            pos += len;
        }
        assert_eq!(kinds, ICNS_TYPES);

        let svg = read(icon.join("sqler.svg"));
        assert_eq!(read(root.join("docs/theme/favicon.svg")), svg);
        assert_eq!(read(root.join("docs/theme/favicon.png")), png(FAVICON_SIZE));
    }
}
