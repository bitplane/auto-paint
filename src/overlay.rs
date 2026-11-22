use crate::{HEIGHT, Result, WIDTH};
use fontdue::layout::{
    CoordinateSystem, HorizontalAlign, Layout, LayoutSettings, TextStyle, VerticalAlign,
};
use fontdue::{Font, FontSettings};

const SIDE_PADDING: f32 = 24.0;
const VERTICAL_PADDING: f32 = 4.0;

struct Style {
    top: usize,
    height: usize,
    align: HorizontalAlign,
    sizes: &'static [f32],
    outline_radius: i32,
}

const CAPTION: Style = Style {
    top: HEIGHT as usize - 96 - 18,
    height: 96,
    align: HorizontalAlign::Center,
    sizes: &[28.0, 26.0, 24.0, 22.0, 20.0, 18.0],
    outline_radius: 2,
};
const MODEL: Style = Style {
    top: 54,
    height: 30,
    align: HorizontalAlign::Right,
    sizes: &[18.0, 16.0, 14.0],
    outline_radius: 1,
};

pub struct Overlays {
    font: Font,
    layout: Layout,
    model: TextMask,
    caption: TextMask,
}

struct TextMask {
    style: Style,
    current: String,
    text: Vec<u8>,
    outline: Vec<u8>,
}

impl Overlays {
    pub fn new(model: &str) -> Result<Self> {
        let font = Font::from_bytes(
            include_bytes!("../assets/LiberationSans-Regular.ttf").as_slice(),
            FontSettings::default(),
        )?;
        let mut overlays = Self {
            font,
            layout: Layout::new(CoordinateSystem::PositiveYDown),
            model: TextMask::new(MODEL),
            caption: TextMask::new(CAPTION),
        };
        overlays.set_model(model);
        Ok(overlays)
    }

    pub fn set_model(&mut self, model: &str) {
        let label: String = model.chars().take(100).collect();
        self.model.update(&self.font, &mut self.layout, &label);
    }

    pub fn compose<'a>(
        &mut self,
        buffer: &'a mut Vec<u32>,
        pixels: &'a [u32],
        subtitle: Option<&str>,
    ) -> &'a [u32] {
        if self.model.current.is_empty() && subtitle.is_none() {
            return pixels;
        }
        buffer.clear();
        buffer.extend_from_slice(pixels);
        if !self.model.current.is_empty() {
            self.model.blend(buffer);
        }
        if let Some(text) = subtitle {
            self.caption.update(&self.font, &mut self.layout, text);
            self.caption.blend(buffer);
        }
        buffer
    }
}

impl TextMask {
    fn new(style: Style) -> Self {
        let pixels = WIDTH as usize * style.height;
        Self {
            style,
            current: String::new(),
            text: vec![0; pixels],
            outline: vec![0; pixels],
        }
    }

    fn update(&mut self, font: &Font, layout: &mut Layout, text: &str) {
        if text != self.current {
            self.rasterize(font, layout, text);
            self.current.clear();
            self.current.push_str(text);
        }
    }

    fn blend(&self, pixels: &mut [u32]) {
        let start = self.style.top * WIDTH as usize;
        let region = &mut pixels[start..start + self.text.len()];
        for ((pixel, &fill), &outline) in region.iter_mut().zip(&self.text).zip(&self.outline) {
            if outline == 0 {
                continue;
            }
            let fill = u16::from(fill);
            let outline = u16::from(outline);
            let [alpha, r, g, b] = pixel.to_be_bytes();
            let [r, g, b] = [r, g, b].map(|channel| {
                let shaded = u16::from(channel) * (255 - outline) / 255;
                (fill + shaded * (255 - fill) / 255) as u8
            });
            *pixel = u32::from_be_bytes([alpha, r, g, b]);
        }
    }

    fn rasterize(&mut self, font: &Font, layout: &mut Layout, text: &str) {
        self.text.fill(0);
        self.outline.fill(0);
        let max_height = self.style.height as f32 - 2.0 * VERTICAL_PADDING;
        let settings = LayoutSettings {
            x: SIDE_PADDING,
            y: VERTICAL_PADDING,
            max_width: Some(WIDTH as f32 - 2.0 * SIDE_PADDING),
            max_height: Some(max_height),
            horizontal_align: self.style.align,
            vertical_align: VerticalAlign::Bottom,
            ..LayoutSettings::default()
        };
        for &size in self.style.sizes {
            layout.reset(&settings);
            layout.append(std::slice::from_ref(font), &TextStyle::new(text, size, 0));
            if layout.height() <= max_height {
                break;
            }
        }

        let radius = self.style.outline_radius;
        for glyph in layout.glyphs() {
            let (metrics, coverage) = font.rasterize_config(glyph.key);
            for (index, alpha) in coverage.into_iter().enumerate() {
                if alpha == 0 {
                    continue;
                }
                let x = glyph.x as i32 + (index % metrics.width) as i32;
                let y = glyph.y as i32 + (index / metrics.width) as i32;
                if let Some(pixel) = region_index(self.style.height, x, y) {
                    self.text[pixel] = self.text[pixel].max(alpha);
                }
                for dy in -radius..=radius {
                    for dx in -radius..=radius {
                        if dx * dx + dy * dy <= radius * radius
                            && let Some(pixel) = region_index(self.style.height, x + dx, y + dy)
                        {
                            self.outline[pixel] = self.outline[pixel].max(alpha);
                        }
                    }
                }
            }
        }
    }
}

fn region_index(height: usize, x: i32, y: i32) -> Option<usize> {
    ((0..WIDTH as i32).contains(&x) && (0..height as i32).contains(&y))
        .then(|| y as usize * WIDTH as usize + x as usize)
}
