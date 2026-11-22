use crate::{HEIGHT, Result, WIDTH};
use fontdue::layout::{
    CoordinateSystem, HorizontalAlign, Layout, LayoutSettings, TextStyle, VerticalAlign,
};
use fontdue::{Font, FontSettings};

const REGION_HEIGHT: usize = 96;
const REGION_TOP: usize = HEIGHT as usize - REGION_HEIGHT - 18;
const REGION_PIXELS: usize = WIDTH as usize * REGION_HEIGHT;
const SIDE_PADDING: f32 = 24.0;
const VERTICAL_PADDING: f32 = 4.0;
const OUTLINE_RADIUS: i32 = 2;

pub struct Subtitles {
    font: Font,
    current: Option<String>,
    layout: Layout,
    text: Vec<u8>,
    outline: Vec<u8>,
}

impl Subtitles {
    pub fn new() -> Result<Self> {
        let font = Font::from_bytes(
            include_bytes!("../assets/LiberationSans-Regular.ttf").as_slice(),
            FontSettings::default(),
        )?;
        Ok(Self {
            font,
            current: None,
            layout: Layout::new(CoordinateSystem::PositiveYDown),
            text: vec![0; REGION_PIXELS],
            outline: vec![0; REGION_PIXELS],
        })
    }

    pub fn update(&mut self, text: Option<&str>) -> bool {
        if text == self.current.as_deref() {
            return false;
        }
        self.current = text.map(str::to_owned);
        if let Some(text) = text {
            self.rasterize(text);
        }
        true
    }

    pub fn blend(&self, rgb: &mut [u8]) {
        if self.current.is_none() {
            return;
        }
        let start = REGION_TOP * WIDTH as usize * 3;
        let region = &mut rgb[start..start + REGION_PIXELS * 3];
        for ((pixel, &fill), &outline) in region
            .as_chunks_mut::<3>()
            .0
            .iter_mut()
            .zip(&self.text)
            .zip(&self.outline)
        {
            if outline == 0 {
                continue;
            }
            let fill = u16::from(fill);
            let outline = u16::from(outline);
            for channel in pixel {
                let shaded = u16::from(*channel) * (255 - outline) / 255;
                *channel = (fill + shaded * (255 - fill) / 255) as u8;
            }
        }
    }

    fn rasterize(&mut self, text: &str) {
        self.text.fill(0);
        self.outline.fill(0);
        let max_height = REGION_HEIGHT as f32 - 2.0 * VERTICAL_PADDING;
        let settings = LayoutSettings {
            x: SIDE_PADDING,
            y: VERTICAL_PADDING,
            max_width: Some(WIDTH as f32 - 2.0 * SIDE_PADDING),
            max_height: Some(max_height),
            horizontal_align: HorizontalAlign::Center,
            vertical_align: VerticalAlign::Bottom,
            ..LayoutSettings::default()
        };
        for size in [28.0, 26.0, 24.0, 22.0, 20.0, 18.0] {
            self.layout.reset(&settings);
            self.layout.append(
                std::slice::from_ref(&self.font),
                &TextStyle::new(text, size, 0),
            );
            if self.layout.height() <= max_height {
                break;
            }
        }

        for glyph in self.layout.glyphs() {
            let (metrics, coverage) = self.font.rasterize_config(glyph.key);
            for (index, alpha) in coverage.into_iter().enumerate() {
                if alpha == 0 {
                    continue;
                }
                let x = glyph.x as i32 + (index % metrics.width) as i32;
                let y = glyph.y as i32 + (index / metrics.width) as i32;
                if let Some(pixel) = region_index(x, y) {
                    self.text[pixel] = self.text[pixel].max(alpha);
                }
                for dy in -OUTLINE_RADIUS..=OUTLINE_RADIUS {
                    for dx in -OUTLINE_RADIUS..=OUTLINE_RADIUS {
                        if dx * dx + dy * dy <= OUTLINE_RADIUS * OUTLINE_RADIUS
                            && let Some(pixel) = region_index(x + dx, y + dy)
                        {
                            self.outline[pixel] = self.outline[pixel].max(alpha);
                        }
                    }
                }
            }
        }
    }
}

fn region_index(x: i32, y: i32) -> Option<usize> {
    ((0..WIDTH as i32).contains(&x) && (0..REGION_HEIGHT as i32).contains(&y))
        .then(|| y as usize * WIDTH as usize + x as usize)
}
