use crate::{FPS, Result};
use muxide::api::Muxer;
use rusty_aac::{AacEncoder, AacEncoderConfig, AdtsHeader, write_adts_header};
use std::io::Write;
use std::time::Duration;

pub const SAMPLE_RATE: u32 = 16_000;
const AAC_FRAME: usize = 1024;

struct Cue {
    start: u64,
    end: u64,
    text: String,
}

pub struct Narration {
    engine: flite_rs::Engine,
    samples: Vec<i16>,
    cues: Vec<Cue>,
}

impl Narration {
    pub fn new() -> Result<Self> {
        let mut engine = flite_rs::Engine::new();
        if !engine.select_voice("kal16") {
            return Err("The embedded Flite voice is unavailable".into());
        }
        Ok(Self {
            engine,
            samples: Vec::new(),
            cues: Vec::new(),
        })
    }

    pub fn add(&mut self, at: Duration, text: &str) -> Option<&[i16]> {
        let offset = sample_offset(at);
        if offset < self.samples.len() || self.cues.len() == 128 {
            return None;
        }
        self.add_at(offset, text)
    }

    pub fn add_review(&mut self, at: Duration, text: &str) -> Option<&[i16]> {
        self.add_at(sample_offset(at).max(self.samples.len()), text)
    }

    fn add_at(&mut self, offset: usize, text: &str) -> Option<&[i16]> {
        let text: String = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(200)
            .collect();
        if text.is_empty() {
            return None;
        }
        let voice = self.engine.synthesize(&text);
        if voice.samples.is_empty() {
            return None;
        }
        self.samples.resize(offset, 0);
        self.samples.extend(voice.samples);
        self.cues.push(Cue {
            start: offset as u64,
            end: self.samples.len() as u64,
            text,
        });
        Some(&self.samples[offset..])
    }

    pub fn subtitle_at(&self, frame: u64) -> Option<&str> {
        // The AAC track retains one frame of encoder priming.
        let sample =
            (frame * u64::from(SAMPLE_RATE) / u64::from(FPS)).checked_sub(AAC_FRAME as u64)?;
        let next = self.cues.partition_point(|cue| cue.start <= sample);
        let cue = self.cues.get(next.checked_sub(1)?)?;
        (sample < cue.end).then_some(cue.text.as_str())
    }

    pub fn duration(&self) -> f64 {
        self.samples.len() as f64 / f64::from(SAMPLE_RATE)
    }

    pub fn write_to(self, mux: &mut Muxer<impl Write>) -> Result<()> {
        let mut encoder = AacEncoder::new(AacEncoderConfig {
            bitrate_bps: 32_000,
            ..AacEncoderConfig::default()
        });
        encoder.set_streaming(true);
        let mut index = 0;
        if self.samples.is_empty() {
            encoder.push_pcm(&[0.0; AAC_FRAME], 1, SAMPLE_RATE)?;
        } else {
            for chunk in self.samples.chunks(AAC_FRAME) {
                let pcm: Vec<f32> = chunk
                    .iter()
                    .map(|&sample| f32::from(sample) / 32768.0)
                    .collect();
                encoder.push_pcm(&pcm, 1, SAMPLE_RATE)?;
                write_packets(&mut encoder, mux, &mut index)?;
            }
        }
        encoder.finish();
        write_packets(&mut encoder, mux, &mut index)?;
        if index == 0 {
            return Err("The voiceover encoder did not emit audio".into());
        }
        Ok(())
    }
}

fn sample_offset(at: Duration) -> usize {
    (at.as_secs_f64() * f64::from(SAMPLE_RATE)).round() as usize
}

fn write_packets(
    encoder: &mut AacEncoder,
    mux: &mut Muxer<impl Write>,
    index: &mut usize,
) -> Result<()> {
    while let Ok(packet) = encoder.next_packet() {
        let mut adts = write_adts_header(&AdtsHeader {
            object_type: 2,
            sample_rate: SAMPLE_RATE,
            channels: 1,
            frame_length: 7 + packet.data.len(),
            header_len: 7,
        })
        .to_vec();
        adts.extend(packet.data);
        let pts = (*index * AAC_FRAME) as f64 / f64::from(SAMPLE_RATE);
        mux.write_audio(pts, &adts)?;
        *index += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_final_review_while_another_line_is_playing() -> Result<()> {
        let mut narration = Narration::new()?;
        let opening = narration
            .add(Duration::ZERO, "I am drawing a line.")
            .unwrap();
        let opening_end = opening.len() as u64;
        assert!(
            narration
                .add_review(Duration::ZERO, "That line looks lost.")
                .is_some()
        );
        assert_eq!(narration.cues[1].start, opening_end);
        let review_frame =
            ((opening_end + AAC_FRAME as u64) * u64::from(FPS)).div_ceil(u64::from(SAMPLE_RATE));
        assert_eq!(
            narration.subtitle_at(review_frame),
            Some("That line looks lost.")
        );
        Ok(())
    }
}
