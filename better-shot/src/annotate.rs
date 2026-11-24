//! Non-interactive annotations drawn onto a captured image.
//!
//! Specs are pixel-based and repeatable:
//! `rect:x,y,w,h`, `text:x,y,STRING`, `arrow:x1,y1,x2,y2`.
//! Text uses a built-in 5x7 bitmap font scaled 3x, so annotations need no
//! fonts, GUI toolkit, or display. `satty` remains available separately.

use image::{Rgba, RgbaImage};

const RED: Rgba<u8> = Rgba([255, 59, 48, 255]);
const SHADOW: Rgba<u8> = Rgba([0, 0, 0, 255]);
const STROKE: i32 = 3;
const TEXT_SCALE: u32 = 3;
const GLYPH_ADVANCE: i32 = (5 * TEXT_SCALE + 2) as i32;

/// A single annotation parsed from `--annotate=SPEC`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Annotation {
    Rect { x: u32, y: u32, w: u32, h: u32 },
    Text { x: u32, y: u32, text: String },
    Arrow { x1: u32, y1: u32, x2: u32, y2: u32 },
}

impl Annotation {
    /// Parses `rect:...`, `text:...` or `arrow:...`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (kind, rest) = spec
            .split_once(':')
            .ok_or_else(|| format!("annotation `{spec}` needs a `kind:...` prefix"))?;
        match kind {
            "rect" => {
                let [x, y, w, h] = parse_numbers::<4>(rest, spec)?;
                if w == 0 || h == 0 {
                    return Err(format!(
                        "annotation `{spec}` needs a non-zero width and height"
                    ));
                }
                Ok(Self::Rect { x, y, w, h })
            }
            "arrow" => {
                let [x1, y1, x2, y2] = parse_numbers::<4>(rest, spec)?;
                if x1 == x2 && y1 == y2 {
                    return Err(format!("annotation `{spec}` has a zero-length arrow"));
                }
                Ok(Self::Arrow { x1, y1, x2, y2 })
            }
            "text" => {
                let mut parts = rest.splitn(3, ',');
                let x = parse_number(parts.next(), spec)?;
                let y = parse_number(parts.next(), spec)?;
                let text = parts
                    .next()
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| format!("annotation `{spec}` needs text after x,y"))?;
                Ok(Self::Text {
                    x,
                    y,
                    text: text.to_owned(),
                })
            }
            other => Err(format!(
                "unknown annotation kind `{other}` (expected rect, text or arrow)"
            )),
        }
    }

    /// Canonical form used in metadata.
    pub fn describe(&self) -> String {
        match self {
            Self::Rect { x, y, w, h } => format!("rect:{x},{y},{w},{h}"),
            Self::Text { x, y, text } => format!("text:{x},{y},{text}"),
            Self::Arrow { x1, y1, x2, y2 } => format!("arrow:{x1},{y1},{x2},{y2}"),
        }
    }

    /// Draws the annotation, validating it against the image size.
    pub fn draw(&self, image: &mut RgbaImage) -> Result<(), String> {
        let (width, height) = (image.width(), image.height());
        match self {
            Self::Rect { x, y, w, h } => {
                let (x, y, w, h) = (*x, *y, *w, *h);
                if w == 0 || h == 0 {
                    return Err("rectangle has a zero width or height".to_owned());
                }
                if x as u64 + w as u64 > width as u64 || y as u64 + h as u64 > height as u64 {
                    return Err(format!(
                        "rectangle {x},{y},{w},{h} does not fit in the {width}x{height} capture"
                    ));
                }
                draw_rect_outline(image, x as i32, y as i32, w as i32, h as i32);
                Ok(())
            }
            Self::Arrow { x1, y1, x2, y2 } => {
                let (x1, y1, x2, y2) = (*x1, *y1, *x2, *y2);
                if x1 >= width || y1 >= height || x2 >= width || y2 >= height {
                    return Err(format!(
                        "arrow {x1},{y1} -> {x2},{y2} lies outside the {width}x{height} capture"
                    ));
                }
                draw_arrow(image, x1 as i32, y1 as i32, x2 as i32, y2 as i32);
                Ok(())
            }
            Self::Text { x, y, text } => {
                let (x, y) = (*x, *y);
                if x >= width || y >= height {
                    return Err(format!(
                        "text origin {x},{y} lies outside the {width}x{height} capture"
                    ));
                }
                draw_text(image, x as i32, y as i32, text);
                Ok(())
            }
        }
    }
}

fn parse_numbers<const N: usize>(rest: &str, spec: &str) -> Result<[u32; N], String> {
    let parts: Vec<&str> = rest.split(',').collect();
    if parts.len() != N {
        return Err(format!(
            "annotation `{spec}` needs {N} comma-separated numbers"
        ));
    }
    let mut values = [0u32; N];
    for (slot, part) in values.iter_mut().zip(parts) {
        *slot = part
            .trim()
            .parse()
            .map_err(|_| format!("annotation `{spec}` has a non-numeric value `{part}`"))?;
    }
    Ok(values)
}

fn parse_number(value: Option<&str>, spec: &str) -> Result<u32, String> {
    value
        .ok_or_else(|| format!("annotation `{spec}` is missing coordinates"))?
        .trim()
        .parse()
        .map_err(|_| format!("annotation `{spec}` has a non-numeric coordinate"))
}

fn put(image: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < image.width() && (y as u32) < image.height() {
        image.put_pixel(x as u32, y as u32, color);
    }
}

fn fill_block(image: &mut RgbaImage, x: i32, y: i32, size: i32, color: Rgba<u8>) {
    for dy in 0..size {
        for dx in 0..size {
            put(image, x + dx, y + dy, color);
        }
    }
}

fn draw_rect_outline(image: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32) {
    let thickness = STROKE.min(w).min(h);
    for px in x..x + w {
        for offset in 0..thickness {
            put(image, px, y + offset, RED);
            put(image, px, y + h - 1 - offset, RED);
        }
    }
    for py in y..y + h {
        for offset in 0..thickness {
            put(image, x + offset, py, RED);
            put(image, x + w - 1 - offset, py, RED);
        }
    }
}

fn draw_arrow(image: &mut RgbaImage, x1: i32, y1: i32, x2: i32, y2: i32) {
    draw_thick_line(image, x1, y1, x2, y2);

    let dx = f64::from(x2 - x1);
    let dy = f64::from(y2 - y1);
    let length = dx.hypot(dy).max(1.0);
    let (ux, uy) = (dx / length, dy / length);
    let head_length = 14.0_f64.min(length * 0.6).max(6.0);
    let (bx, by) = (
        f64::from(x2) - ux * head_length,
        f64::from(y2) - uy * head_length,
    );
    // Perpendicular offset for a ~30 degree head.
    let spread = head_length * 0.55;
    let (px, py) = (-uy * spread, ux * spread);
    let h1 = ((bx + px).round() as i32, (by + py).round() as i32);
    let h2 = ((bx - px).round() as i32, (by - py).round() as i32);
    draw_thick_line(image, x2, y2, h1.0, h1.1);
    draw_thick_line(image, x2, y2, h2.0, h2.1);
}

fn draw_thick_line(image: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32) {
    let radius = STROKE / 2;
    let (mut x, mut y) = (x0, y0);
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let mut err = dx + dy;
    loop {
        fill_block(image, x - radius, y - radius, STROKE, RED);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

fn draw_text(image: &mut RgbaImage, x: i32, y: i32, text: &str) {
    let mut cursor = x;
    for ch in text.chars() {
        let glyph = glyph(ch);
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                let px = cursor + column * TEXT_SCALE as i32;
                let py = y + row as i32 * TEXT_SCALE as i32;
                // One-pixel drop shadow keeps red text readable on light UI.
                fill_block(image, px + 1, py + 1, TEXT_SCALE as i32, SHADOW);
                fill_block(image, px, py, TEXT_SCALE as i32, RED);
            }
        }
        cursor += GLYPH_ADVANCE;
    }
}

/// 5x7 glyph, one byte per row, bit 4 = leftmost pixel.
#[allow(clippy::match_same_arms)]
fn glyph(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01111, 0b10000, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        '!' => [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
        '?' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b00000, 0b00100,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        '_' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00100,
        ],
        ',' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00100, 0b01000,
        ],
        ':' => [
            0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000,
        ],
        ';' => [
            0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b01000, 0b00000,
        ],
        '/' => [
            0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000,
        ],
        '+' => [
            0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000,
        ],
        '=' => [
            0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000,
        ],
        '#' => [
            0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010,
        ],
        '%' => [
            0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011,
        ],
        '\'' => [
            0b00100, 0b00100, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000,
        ],
        '"' => [
            0b01010, 0b01010, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000,
        ],
        '(' => [
            0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010,
        ],
        ')' => [
            0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000,
        ],
        '<' => [
            0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010,
        ],
        '>' => [
            0b01000, 0b00100, 0b00010, 0b00001, 0b00010, 0b00100, 0b01000,
        ],
        '*' => [
            0b00000, 0b10101, 0b01110, 0b11111, 0b01110, 0b10101, 0b00000,
        ],
        _ => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b00000, 0b00100,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> RgbaImage {
        RgbaImage::from_pixel(64, 48, Rgba([255, 255, 255, 255]))
    }

    fn is_red(pixel: Rgba<u8>) -> bool {
        pixel[0] > 200 && pixel[1] < 100 && pixel[2] < 100
    }

    #[test]
    fn parses_all_specs() {
        assert_eq!(
            Annotation::parse("rect:1,2,30,40").unwrap(),
            Annotation::Rect {
                x: 1,
                y: 2,
                w: 30,
                h: 40
            }
        );
        assert_eq!(
            Annotation::parse("arrow:1,2,30,40").unwrap(),
            Annotation::Arrow {
                x1: 1,
                y1: 2,
                x2: 30,
                y2: 40
            }
        );
        assert_eq!(
            Annotation::parse("text:3,4,hello, world").unwrap(),
            Annotation::Text {
                x: 3,
                y: 4,
                text: "hello, world".to_owned()
            }
        );
        assert_eq!(
            Annotation::parse("text:0,0,x").unwrap().describe(),
            "text:0,0,x"
        );
    }

    #[test]
    fn rejects_bad_specs() {
        for spec in [
            "rect:1,2,3",
            "rect:1,2,0,4",
            "rect:-1,2,3,4",
            "arrow:1,1,1,1",
            "arrow:1,2,3",
            "text:1,2",
            "text:1,2,",
            "circle:1,2,3,4",
            "nonsense",
        ] {
            assert!(Annotation::parse(spec).is_err(), "accepted {spec}");
        }
    }

    #[test]
    fn draws_rectangle_outline_only() {
        let mut image = canvas();
        Annotation::parse("rect:5,6,20,15")
            .unwrap()
            .draw(&mut image)
            .unwrap();
        assert!(is_red(*image.get_pixel(5, 6)));
        assert!(is_red(*image.get_pixel(24, 20)));
        assert!(is_red(*image.get_pixel(5, 20)));
        assert!(is_red(*image.get_pixel(24, 6)));
        assert_eq!(*image.get_pixel(15, 13), Rgba([255, 255, 255, 255]));
        assert_eq!(*image.get_pixel(4, 6), Rgba([255, 255, 255, 255]));
    }

    #[test]
    fn rejects_out_of_bounds_geometry() {
        let mut image = canvas();
        let err = Annotation::parse("rect:60,40,10,10")
            .unwrap()
            .draw(&mut image)
            .unwrap_err();
        assert!(err.contains("does not fit"));
        let err = Annotation::parse("arrow:1,1,100,1")
            .unwrap()
            .draw(&mut image)
            .unwrap_err();
        assert!(err.contains("outside"));
        let err = Annotation::parse("text:100,1,x")
            .unwrap()
            .draw(&mut image)
            .unwrap_err();
        assert!(err.contains("outside"));
    }

    #[test]
    fn draws_arrow_shaft_and_head() {
        let mut image = canvas();
        Annotation::parse("arrow:5,20,40,20")
            .unwrap()
            .draw(&mut image)
            .unwrap();
        assert!(is_red(*image.get_pixel(5, 20)));
        assert!(is_red(*image.get_pixel(20, 20)));
        assert!(is_red(*image.get_pixel(40, 20)));
        // The head spreads away from the 3 px shaft (y = 19..=21).
        let head_pixels = (0..48)
            .flat_map(|y| (25..=40).map(move |x| (x, y)))
            .filter(|&(x, y)| !(19..=21).contains(&y) && is_red(*image.get_pixel(x, y)))
            .count();
        assert!(head_pixels > 10, "arrow head too small: {head_pixels}");
    }

    #[test]
    fn draws_text_with_builtin_font() {
        let mut image = canvas();
        Annotation::parse("text:2,2,Hi 42!")
            .unwrap()
            .draw(&mut image)
            .unwrap();
        let red_pixels = image.pixels().filter(|pixel| is_red(**pixel)).count();
        assert!(red_pixels > 40, "text drew only {red_pixels} pixels");
        // Unknown characters fall back to `?` instead of panicking.
        let mut fallback = canvas();
        Annotation::parse("text:2,2,§")
            .unwrap()
            .draw(&mut fallback)
            .unwrap();
        assert!(fallback.pixels().any(|pixel| is_red(*pixel)));
    }
}
