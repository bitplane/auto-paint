use crate::narration::{Narration, SAMPLE_RATE};
use crate::subtitles::Subtitles;
use crate::{FPS, HEIGHT, Result, WIDTH};
use muxide::api::{AacProfile, AudioCodec, Muxer, MuxerBuilder, VideoCodec};
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode,
};
use openh264::formats::{RgbSliceU8, YUVBuffer};
use openh264::{OpenH264API, Timestamp};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

pub struct Video {
    mux: Muxer<BufWriter<File>>,
    encoder: Encoder,
    pixels: Vec<u32>,
    rgb: Vec<u8>,
    yuv: YUVBuffer,
    packet: Vec<u8>,
    subtitles: Option<Subtitles>,
}

impl Video {
    pub fn new(path: &Path, narrate: bool) -> Result<Self> {
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(2_000_000))
            .max_frame_rate(FrameRate::from_hz(FPS as f32))
            .rate_control_mode(RateControlMode::Bitrate)
            .intra_frame_period(IntraFramePeriod::from_num_frames(2 * FPS))
            .skip_frames(false);
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), config)?;
        let subtitles = narrate.then(Subtitles::new).transpose()?;
        let file = BufWriter::new(File::create(path)?);
        let mut builder = MuxerBuilder::new(file)
            .video(VideoCodec::H264, WIDTH, HEIGHT, f64::from(FPS))
            .with_fast_start(true);
        if narrate {
            builder = builder.audio(AudioCodec::Aac(AacProfile::Lc), SAMPLE_RATE, 1);
        }
        let size = (WIDTH * HEIGHT) as usize;
        Ok(Self {
            mux: builder.build()?,
            encoder,
            pixels: Vec::with_capacity(size),
            rgb: vec![0; size * 3],
            yuv: YUVBuffer::new(WIDTH as usize, HEIGHT as usize),
            packet: Vec::new(),
            subtitles,
        })
    }

    pub fn write(&mut self, pixels: &[u32], frame: u64, subtitle: Option<&str>) -> Result<()> {
        let subtitle_changed = self
            .subtitles
            .as_mut()
            .is_some_and(|subtitles| subtitles.update(subtitle));
        if self.pixels != pixels || subtitle_changed {
            self.pixels.clear();
            self.pixels.extend_from_slice(pixels);
            for (rgb, &pixel) in self.rgb.as_chunks_mut::<3>().0.iter_mut().zip(pixels) {
                let [_, r, g, b] = pixel.to_be_bytes();
                *rgb = [r, g, b];
            }
            if let Some(subtitles) = &self.subtitles {
                subtitles.blend(&mut self.rgb);
            }
            self.yuv.read_rgb8(RgbSliceU8::new(
                &self.rgb,
                (WIDTH as usize, HEIGHT as usize),
            ));
        }
        let timestamp = Timestamp::from_millis(frame * 1000 / u64::from(FPS));
        let stream = self.encoder.encode_at(&self.yuv, timestamp)?;
        let keyframe = matches!(stream.frame_type(), FrameType::IDR);
        self.packet.clear();
        stream.write_vec(&mut self.packet);
        if self.packet.is_empty() {
            return Err("OpenH264 did not emit a frame".into());
        }
        self.mux
            .write_video(frame as f64 / f64::from(FPS), &self.packet, keyframe)?;
        Ok(())
    }

    pub fn finish(mut self, narration: Option<Narration>) -> Result<()> {
        if let Some(narration) = narration {
            narration.write_to(&mut self.mux)?;
        }
        self.mux.finish_in_place()?;
        let mut file = self.mux.into_writer();
        file.flush()?;
        Ok(())
    }
}
