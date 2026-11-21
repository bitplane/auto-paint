use crate::Result;
use muxide::api::Muxer;
use rusty_aac::{AacEncoder, AacEncoderConfig, AdtsHeader, write_adts_header};
use std::io::Write;
use std::time::Duration;

pub const SAMPLE_RATE: u32 = 16_000;
const AAC_FRAME: usize = 1024;

pub struct Cue {
    pub at: Duration,
    pub text: String,
}

pub struct Audio {
    samples: Vec<i16>,
}

pub fn synthesize(cues: &[Cue]) -> Result<Audio> {
    let mut samples = Vec::new();
    if !cues.is_empty() {
        let mut engine = flite_rs::Engine::new();
        if !engine.select_voice("kal16") {
            return Err("The embedded Flite voice is unavailable".into());
        }
        for cue in cues {
            let offset = (cue.at.as_secs_f64() * f64::from(SAMPLE_RATE)).round() as usize;
            if offset < samples.len() {
                continue;
            }
            let voice = engine.synthesize(&cue.text);
            samples.resize(offset, 0);
            samples.extend(voice.samples);
        }
    }
    Ok(Audio { samples })
}

impl Audio {
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
