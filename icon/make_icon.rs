//! Draws the ScreenKnob app icon: a dark rounded square (macOS icon shape) with
//! a volume dial — an arc of level marks and an aluminium knob pointing at them.
//! Run with `cargo run --example make_icon`; writes icon/AppIcon.iconset/*.png,
//! then `iconutil -c icns icon/AppIcon.iconset -o icon/AppIcon.icns`.

use image::{Rgba, RgbaImage};
use std::f32::consts::PI;

const S: f32 = 1024.0;

#[derive(Clone, Copy)]
struct C(f32, f32, f32, f32);

fn mix(a: C, b: C, t: f32) -> C {
    let t = t.clamp(0.0, 1.0);
    C(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t, a.3 + (b.3 - a.3) * t)
}

fn hex(h: u32) -> C {
    C(((h >> 16) & 255) as f32 / 255.0, ((h >> 8) & 255) as f32 / 255.0, (h & 255) as f32 / 255.0, 1.0)
}

/// "Over" compositing of `src` with coverage `a` onto `dst` (straight alpha).
fn over(dst: C, src: C, a: f32) -> C {
    let sa = (src.3 * a).clamp(0.0, 1.0);
    let oa = sa + dst.3 * (1.0 - sa);
    if oa <= 0.0 {
        return C(0.0, 0.0, 0.0, 0.0);
    }
    let f = |s: f32, d: f32| (s * sa + d * dst.3 * (1.0 - sa)) / oa;
    C(f(src.0, dst.0), f(src.1, dst.1), f(src.2, dst.2), oa)
}

/// Coverage from a signed distance (negative inside), one pixel of smoothing.
fn cov(d: f32, px: f32) -> f32 {
    (0.5 - d / px).clamp(0.0, 1.0)
}

/// Signed distance to a superellipse ("squircle") of half-size `r`, like macOS icons.
fn squircle(x: f32, y: f32, r: f32) -> f32 {
    let n = 5.0;
    let k = ((x / r).abs().powf(n) + (y / r).abs().powf(n)).powf(1.0 / n);
    (k - 1.0) * r
}

fn main() {
    let sizes = [16u32, 32, 64, 128, 256, 512, 1024];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("icon/AppIcon.iconset");
    std::fs::create_dir_all(&dir).expect("iconset folder");
    let big = render(1024);
    big.save(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("icon/ScreenKnob.png")).expect("png");
    for &px in &sizes {
        let img = image::imageops::resize(&big, px, px, image::imageops::FilterType::Lanczos3);
        let name = |n: u32, two: bool| if two { format!("icon_{n}x{n}@2x.png") } else { format!("icon_{n}x{n}.png") };
        if px <= 512 {
            img.save(dir.join(name(px, false))).expect("png");
        }
        if px >= 32 {
            img.save(dir.join(name(px / 2, true))).expect("png");
        }
    }
    println!("wrote {}", dir.display());
}

fn render(size: u32) -> RgbaImage {
    let scale = size as f32 / S;
    let mut img = RgbaImage::new(size, size);
    let px = 1.0 / scale;

    // Palette.
    let bg_top = hex(0x3b4252);
    let bg_bot = hex(0x15181f);
    let tick_off = C(1.0, 1.0, 1.0, 0.16);
    let tick_lo = hex(0x2fd3c6);
    let tick_hi = hex(0x3f8cff);
    let knob_hi = hex(0xf4f6f9);
    let knob_lo = hex(0xa9b0bc);

    // Dial geometry (in 1024 space, centred).
    let start = PI * 0.75; // lower left
    let sweep = PI * 1.5; // 270°
    let level = 0.68; // how far the dial is turned
    let ticks = 15;
    let knob_r = 228.0;

    for j in 0..size {
        for i in 0..size {
            let x = (i as f32 + 0.5) / scale - S / 2.0;
            let y = (j as f32 + 0.5) / scale - S / 2.0;
            let mut c = C(0.0, 0.0, 0.0, 0.0);

            // Soft shadow under the tile.
            let sh = squircle(x, y - 14.0, 412.0);
            c = over(c, C(0.0, 0.0, 0.0, 0.35), (1.0 - (sh / 28.0).clamp(0.0, 1.0)) * cov(sh - 28.0, 56.0));

            // The tile: vertical gradient with a faint top highlight.
            let d = squircle(x, y, 412.0);
            let t = (y + 412.0) / 824.0;
            let mut tile = mix(bg_top, bg_bot, t);
            let glow = (1.0 - ((x * x + (y + 420.0) * (y + 420.0)).sqrt() / 700.0)).clamp(0.0, 1.0);
            tile = mix(tile, hex(0x5b6478), glow * 0.35);
            c = over(c, tile, cov(d, px));
            // Thin inner edge light.
            c = over(c, C(1.0, 1.0, 1.0, 0.10), cov(d.abs() - 1.5, px) * cov(d, px));

            let r = (x * x + y * y).sqrt();
            // Angle measured like the dial: 0 at `start`, growing clockwise.
            let mut a = y.atan2(x) - start;
            while a < 0.0 {
                a += 2.0 * PI;
            }

            // Level marks: rounded bars on a ring.
            if (300.0..=380.0).contains(&r) && (a <= sweep + 0.2 || a >= 2.0 * PI - 0.2) {
                for k in 0..ticks {
                    let f = k as f32 / (ticks - 1) as f32;
                    let ang = start + sweep * f;
                    let (dx, dy) = (ang.cos(), ang.sin());
                    // Distance to a capsule from radius 318 to 362 along the mark's direction.
                    let along = (x * dx + y * dy).clamp(318.0 + 6.0 * f, 366.0);
                    let (qx, qy) = (x - dx * along, y - dy * along);
                    let dd = (qx * qx + qy * qy).sqrt() - (9.0 + 5.0 * f);
                    if dd < 2.0 {
                        let on = f <= level + 1e-3;
                        let col = if on { mix(tick_lo, tick_hi, f / level) } else { tick_off };
                        c = over(c, col, cov(dd, px));
                    }
                }
            }

            // Knob: shadow, body with a brushed-metal radial shading, rim and pointer.
            let ks = ((x * x + (y - 18.0) * (y - 18.0)).sqrt()) - knob_r;
            c = over(c, C(0.0, 0.0, 0.0, 0.45), (1.0 - (ks / 40.0).clamp(0.0, 1.0)) * cov(ks - 40.0, 80.0));
            let kd = r - knob_r;
            if kd < 2.0 {
                let lx = (x + 90.0) / (knob_r * 1.7);
                let ly = (y + 110.0) / (knob_r * 1.7);
                let shade = (lx * lx + ly * ly).sqrt();
                let mut body = mix(knob_hi, knob_lo, shade);
                // Fine concentric "brushed" rings.
                let ring = ((r / 6.0).sin() * 0.5 + 0.5) * 0.04;
                body = mix(body, C(1.0, 1.0, 1.0, 1.0), ring);
                c = over(c, body, cov(kd, px));
                // Darker rim.
                c = over(c, C(0.25, 0.28, 0.33, 0.55), cov((kd + 6.0).abs() - 6.0, px) * cov(kd, px));
                // Inner face, slightly raised.
                let fd = r - (knob_r - 30.0);
                let face = mix(hex(0xeef1f5), hex(0xc3c9d3), ((y + 160.0) / 420.0).clamp(0.0, 1.0));
                c = over(c, face, cov(fd, px) * 0.85);
            }
            // Pointer: a coloured dot near the knob's edge at the dial's level.
            let pa = start + sweep * level;
            let (pxp, pyp) = (pa.cos() * (knob_r - 72.0), pa.sin() * (knob_r - 72.0));
            let pd = ((x - pxp).powi(2) + (y - pyp).powi(2)).sqrt() - 26.0;
            if pd < 2.0 {
                c = over(c, mix(tick_lo, tick_hi, 0.8), cov(pd, px));
                let hl = ((x - pxp + 8.0).powi(2) + (y - pyp + 10.0).powi(2)).sqrt() - 9.0;
                c = over(c, C(1.0, 1.0, 1.0, 0.45), cov(hl, px));
            }

            let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            img.put_pixel(i, j, Rgba([to8(c.0), to8(c.1), to8(c.2), to8(c.3)]));
        }
    }
    img
}
