//! Pictures as terminal art: a small bitmap read from the system `sips` output, drawn with
//! half-block cells (two pixels per cell, upper in the text colour, lower in the background).

/// Largest side, in pixels, this accepts (the system tool shrinks pictures well below it).
const MAX_SIDE: usize = 4096;

/// An RGB picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    /// Row by row, top first.
    pub pixels: Vec<[u8; 3]>,
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

/// Read an uncompressed 24 or 32 bit Windows bitmap.
///
/// # Errors
/// The bytes are not such a bitmap, or it is larger than expected.
pub fn parse_bmp(b: &[u8]) -> Result<Image, String> {
    const BAD: &str = "this picture could not be read";
    if b.get(..2) != Some(b"BM") {
        return Err(BAD.to_owned());
    }
    let offset = le_u32(b, 10).ok_or(BAD)? as usize;
    let w = le_u32(b, 18).ok_or(BAD)? as i32;
    let h = le_u32(b, 22).ok_or(BAD)? as i32;
    let bpp = le_u16(b, 28).ok_or(BAD)? as usize;
    let compression = le_u32(b, 30).ok_or(BAD)?;
    if w <= 0 || h == 0 || !matches!(bpp, 24 | 32) || !matches!(compression, 0 | 3) {
        return Err(BAD.to_owned());
    }
    let (width, height) = (w as usize, h.unsigned_abs() as usize);
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(BAD.to_owned());
    }
    let stride = (width * bpp).div_ceil(32) * 4;
    let bottom_up = h > 0;
    let mut pixels = Vec::with_capacity(width * height);
    for row in 0..height {
        let src = if bottom_up { height - 1 - row } else { row };
        let start = offset + src * stride;
        let line = b.get(start..start + width * bpp / 8).ok_or(BAD)?;
        for px in line.chunks_exact(bpp / 8) {
            pixels.push([px[2], px[1], px[0]]);
        }
    }
    Ok(Image {
        width,
        height,
        pixels,
    })
}

/// One terminal cell: the upper pixel and the lower pixel.
pub type Cell = ([u8; 3], [u8; 3]);

impl Image {
    /// The picture fitted into `cols` x `rows` cells, aspect kept (a cell is two square pixels
    /// tall), each cell the average of the pixels it covers. Rows may be fewer than `rows`.
    #[must_use]
    pub fn fit(&self, cols: usize, rows: usize) -> Vec<Vec<Cell>> {
        if self.width == 0 || self.height == 0 || cols == 0 || rows == 0 {
            return Vec::new();
        }
        let max_h = rows * 2;
        // scale so that both sides fit
        let scale = (cols as f64 / self.width as f64).min(max_h as f64 / self.height as f64);
        let out_w = ((self.width as f64 * scale).round() as usize).clamp(1, cols);
        let out_h = (((self.height as f64 * scale).round() as usize).clamp(1, max_h) + 1) & !1;
        let sample = |x: usize, y: usize| -> [u8; 3] {
            let x0 = x * self.width / out_w;
            let x1 = ((x + 1) * self.width / out_w).max(x0 + 1).min(self.width);
            let y0 = y * self.height / out_h;
            let y1 = ((y + 1) * self.height / out_h).max(y0 + 1).min(self.height);
            let mut sum = [0u32; 3];
            let mut n = 0u32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let p = self.pixels[yy * self.width + xx];
                    for (s, c) in sum.iter_mut().zip(p) {
                        *s += u32::from(c);
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]
        };
        (0..out_h / 2)
            .map(|r| {
                (0..out_w)
                    .map(|x| (sample(x, r * 2), sample(x, r * 2 + 1)))
                    .collect()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 bottom-up 24-bit bitmap: bottom row red, green; top row blue, white.
    fn bmp() -> Vec<u8> {
        let mut b = b"BM".to_vec();
        b.extend([0u8; 12]);
        b[10..14].copy_from_slice(&54u32.to_le_bytes());
        b.extend(40u32.to_le_bytes()); // dib size
        b.extend(2i32.to_le_bytes());
        b.extend(2i32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(24u16.to_le_bytes());
        b.extend([0u8; 24]); // compression .. colours
                             // rows are padded to 4 bytes: 6 -> 8
        b.extend([0, 0, 255, 0, 255, 0, 0, 0]); // red, green (BGR)
        b.extend([255, 0, 0, 255, 255, 255, 0, 0]); // blue, white
        b
    }

    #[test]
    fn a_bitmap_is_read_top_row_first() {
        let img = parse_bmp(&bmp()).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.pixels[0], [0, 0, 255]);
        assert_eq!(img.pixels[1], [255, 255, 255]);
        assert_eq!(img.pixels[2], [255, 0, 0]);
    }

    #[test]
    fn garbage_is_refused() {
        assert!(parse_bmp(b"not a bitmap").is_err());
        assert!(parse_bmp(&bmp()[..40]).is_err());
    }

    #[test]
    fn a_picture_is_fitted_to_the_cells_given() {
        let img = parse_bmp(&bmp()).unwrap();
        let cells = img.fit(10, 10);
        assert_eq!((cells.len(), cells[0].len()), (5, 10), "enlarged to fit");
        assert_eq!(cells[0][0], ([0, 0, 255], [0, 0, 255]));
        // shrinking keeps within the area
        let big = Image {
            width: 100,
            height: 100,
            pixels: vec![[1, 2, 3]; 10_000],
        };
        let c = big.fit(40, 10);
        assert!(c.len() <= 10 && c.iter().all(|r| r.len() <= 40));
    }
}
