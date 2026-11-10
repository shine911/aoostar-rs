// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

#![forbid(non_ascii_idents)]
#![deny(unsafe_code)]

use bytes::BytesMut;
use image::{RgbImage, RgbaImage};

mod aoo_screen;
mod fake_serialport;

pub use aoo_screen::{AooScreen, AooScreenBuilder, DISPLAY_SIZE};
pub use fake_serialport::{FakeSerialPort, WriteRecord, WriteStep};

/// Trait definition to get a RGB 565 representation from a source image.
pub trait ToRgb565 {
    /// Get an RGB 565 representation of the image in little endian format.
    fn to_rgb565_le(&self) -> BytesMut;

    /// Write the little endian RGB 565 representation of the image into `out`,
    /// replacing its previous content. Lets callers reuse one buffer per frame
    /// instead of allocating a new ~720 KB buffer for every frame.
    ///
    /// The default implementation delegates to [`ToRgb565::to_rgb565_le`];
    /// the image implementations of this crate convert in place.
    fn write_rgb565_le(&self, out: &mut BytesMut) {
        *out = self.to_rgb565_le();
    }

    /// Convert a single RGB 888 pixel to 16 bit RGB 565 format.
    fn convert_rgb(&self, r: u8, g: u8, b: u8) -> u16 {
        rgb888_to_rgb565(r, g, b)
    }
}

/// Convert a single RGB 888 pixel to 16 bit RGB 565 format.
#[inline]
pub const fn rgb888_to_rgb565(r: u8, g: u8, b: u8) -> u16 {
    ((r & 248) as u16) << 8 | ((g & 252) as u16) << 3 | ((b as u16) >> 3)
}

/// Convert raw interleaved pixels with `N` channels (RGB first) to little
/// endian RGB 565 in `out`, replacing its previous content.
fn raw_to_rgb565_le<const N: usize>(raw: &[u8], out: &mut BytesMut) {
    let pixels = raw.len() / N;
    out.clear();
    out.resize(pixels * 2, 0);
    for (dst, px) in out.chunks_exact_mut(2).zip(raw.chunks_exact(N)) {
        dst.copy_from_slice(&rgb888_to_rgb565(px[0], px[1], px[2]).to_le_bytes());
    }
}

impl ToRgb565 for &RgbImage {
    fn to_rgb565_le(&self) -> BytesMut {
        let mut out = BytesMut::new();
        self.write_rgb565_le(&mut out);
        out
    }

    fn write_rgb565_le(&self, out: &mut BytesMut) {
        let len = self.width() as usize * self.height() as usize * 3;
        raw_to_rgb565_le::<3>(&self.as_raw()[..len], out);
    }
}

impl ToRgb565 for &RgbaImage {
    fn to_rgb565_le(&self) -> BytesMut {
        let mut out = BytesMut::new();
        self.write_rgb565_le(&mut out);
        out
    }

    fn write_rgb565_le(&self, out: &mut BytesMut) {
        let len = self.width() as usize * self.height() as usize * 4;
        raw_to_rgb565_le::<4>(&self.as_raw()[..len], out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BufMut;
    use image::{Rgb, Rgba};

    /// The original per-pixel implementation, kept as the reference.
    fn reference_rgb565_le<I>(pixels: I, capacity: usize) -> BytesMut
    where
        I: Iterator<Item = [u8; 3]>,
    {
        let mut out = BytesMut::with_capacity(capacity);
        for [r, g, b] in pixels {
            out.put_u16_le(((r & 248) as u16) << 8 | ((g & 252) as u16) << 3 | ((b as u16) >> 3));
        }
        out
    }

    /// Deterministic pseudo-random test pattern covering all byte values.
    fn pattern(x: u32, y: u32, c: u32) -> u8 {
        (x.wrapping_mul(31) ^ y.wrapping_mul(17) ^ c.wrapping_mul(101)).wrapping_add(x * y) as u8
    }

    #[test]
    fn rgba_conversion_matches_reference() {
        let img = RgbaImage::from_fn(97, 61, |x, y| {
            Rgba([
                pattern(x, y, 0),
                pattern(x, y, 1),
                pattern(x, y, 2),
                pattern(x, y, 3),
            ])
        });
        let expected =
            reference_rgb565_le(img.pixels().map(|p| [p.0[0], p.0[1], p.0[2]]), 97 * 61 * 2);
        assert_eq!((&img).to_rgb565_le(), expected);

        // reused buffer with stale, longer content is fully replaced
        let mut buf = BytesMut::from(&[0xEEu8; 50_000][..]);
        (&img).write_rgb565_le(&mut buf);
        assert_eq!(buf, expected);
    }

    #[test]
    fn rgb_conversion_matches_reference() {
        let img = RgbImage::from_fn(960, 376, |x, y| {
            Rgb([pattern(x, y, 0), pattern(x, y, 1), pattern(x, y, 2)])
        });
        let expected = reference_rgb565_le(
            img.pixels().map(|p| [p.0[0], p.0[1], p.0[2]]),
            960 * 376 * 2,
        );
        let mut buf = BytesMut::new();
        (&img).write_rgb565_le(&mut buf);
        assert_eq!(buf, expected);
        assert_eq!(buf.len(), 960 * 376 * 2);
    }

    #[test]
    fn convert_rgb_extremes() {
        assert_eq!(rgb888_to_rgb565(0, 0, 0), 0);
        assert_eq!(rgb888_to_rgb565(255, 255, 255), 0xFFFF);
        assert_eq!(rgb888_to_rgb565(255, 0, 0), 0xF800);
        assert_eq!(rgb888_to_rgb565(0, 255, 0), 0x07E0);
        assert_eq!(rgb888_to_rgb565(0, 0, 255), 0x001F);
    }
}
