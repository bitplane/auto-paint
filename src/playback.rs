use crate::Result;
use crate::narration::SAMPLE_RATE;
use maudio::audio::sample_rate::SampleRate;
use maudio::backend::Backend;
use maudio::context::ContextBuilder;
use maudio::device::Device;
use maudio::device::device_builder::{DeviceBuilder, DeviceBuilderOps};
use std::sync::mpsc::{Sender, channel};

pub struct Playback {
    _device: Device<i16>,
    clips: Sender<Vec<i16>>,
}

impl Playback {
    pub fn open() -> Result<Self> {
        let context = ContextBuilder::new()
            .preferred_backends(
                Backend::all_supported_on_this_platform()
                    .filter(|backend| !matches!(backend, Backend::Null | Backend::Custom)),
            )
            .build()?;
        let (clips, queued) = channel::<Vec<i16>>();
        let mut clip = Vec::new().into_iter();
        let mut device = DeviceBuilder::playback()
            .i16()
            .context(&context)
            .playback_channels(1)
            .sample_rate(SampleRate::try_from(SAMPLE_RATE)?)
            .period_size_millis(20)
            .with_callback(move |_, output| {
                output.fill(0);
                for sample in output {
                    let next = clip.next().or_else(|| {
                        clip = queued.try_recv().ok()?.into_iter();
                        clip.next()
                    });
                    let Some(next) = next else { break };
                    *sample = next;
                }
            })?;
        device.device_start()?;
        Ok(Self {
            _device: device,
            clips,
        })
    }

    pub fn play(&self, samples: &[i16]) {
        let _ = self.clips.send(samples.to_vec());
    }
}
