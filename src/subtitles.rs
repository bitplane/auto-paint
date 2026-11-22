use crate::{Result, WIDTH};
use fontdue::layout::{
    CoordinateSystem, HorizontalAlign, Layout, LayoutSettings, TextStyle, VerticalAlign,
};
use fontdue::{Font, FontSettings};

pub const STRIP_HEIGHT: u32 = 96;
pub const BACKGROUND: u8 = 24;
const TEXT_COLOR: u16 = 255;
const SIDE_PADDING: f32 = 24.0;
const TOP_PADDING: f32 = 4.0;

pub struct Subtitles {
    font: Font,
    current: Option<String>,
    layout: Layout,
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
        })
    }

    pub fn update(&mut self, text: Option<&str>, rgb: &mut [u8]) -> bool {
        if text == self.current.as_deref() {
            return false;
        }
        self.current = text.map(str::to_owned);
        rgb.fill(BACKGROUND);
        if let Some(text) = text {
            self.draw(text, rgb);
        }
        true
    }

    fn draw(&mut self, text: &str, rgb: &mut [u8]) {
        let max_height = STRIP_HEIGHT as f32 - 2.0 * TOP_PADDING;
        let settings = LayoutSettings {
            x: SIDE_PADDING,
            y: TOP_PADDING,
            max_width: Some(WIDTH as f32 - 2.0 * SIDE_PADDING),
            max_height: Some(max_height),
            horizontal_align: HorizontalAlign::Center,
            vertical_align: VerticalAlign::Middle,
            ..LayoutSettings::default()
        };
        for size in [26.0, 24.0, 22.0, 20.0, 18.0] {
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
                let x = glyph.x as i32 + (index % metrics.width) as i32;
                let y = glyph.y as i32 + (index / metrics.width) as i32;
                if (0..WIDTH as i32).contains(&x) && (0..STRIP_HEIGHT as i32).contains(&y) {
                    let start = (y as usize * WIDTH as usize + x as usize) * 3;
                    let ink = u16::from(BACKGROUND)
                        + (TEXT_COLOR - u16::from(BACKGROUND)) * u16::from(alpha) / 255;
                    rgb[start..start + 3].fill(ink as u8);
                }
            }
        }
    }
}
