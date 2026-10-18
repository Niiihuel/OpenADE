//! A short two-note chime when a task needs the person. openADE plays it
//! itself: many notification services (mako, dunst) ignore a notification's
//! sound hint.

const FIRST: f32 = 880.0;
const SECOND: f32 = 1318.5;
const SPLIT: f32 = 0.09;
/// Seconds; after this every sample is silence.
const LENGTH: f32 = 0.32;

/// The chime's sample at `seconds` from its start, within ±0.2.
fn sample(seconds: f32) -> f32 {
    if !(0.0..LENGTH).contains(&seconds) {
        return 0.0;
    }
    let (frequency, start) = if seconds < SPLIT { (FIRST, 0.0) } else { (SECOND, SPLIT) };
    let local = seconds - start;
    // A 5 ms attack avoids a click; the decay ends near zero at LENGTH.
    let envelope = (local / 0.005).min(1.0) * (-local * 14.0).exp();
    0.2 * envelope * (std::f32::consts::TAU * frequency * local).sin()
}

/// Play the chime on the default output, off the interface thread. A
/// machine without one stays silent.
pub(crate) fn play() {
    // shortcut: Windows has no output here (cpal is not built for it) and its
    // toast plays its own sound; upgrade when cpal is enabled there.
    #[cfg(all(not(windows), not(test)))]
    let _ = std::thread::Builder::new()
        .name("chime".into())
        .spawn(|| {
            let _ = output::play();
        });
}

#[cfg(all(not(windows), not(test)))]
mod output {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig) -> Result<cpal::Stream, cpal::BuildStreamError>
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let channels = usize::from(config.channels.max(1));
        let rate = config.sample_rate as f32;
        let mut frame = 0_u64;
        device.build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                for out in data.chunks_mut(channels) {
                    let value = T::from_sample(super::sample(frame as f32 / rate));
                    out.fill(value);
                    frame += 1;
                }
            },
            |_| {},
            None,
        )
    }

    pub(super) fn play() -> Result<(), String> {
        let device = cpal::default_host().default_output_device().ok_or("no output")?;
        let supported = device.default_output_config().map_err(|error| error.to_string())?;
        let config = supported.config();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config),
            format => return Err(format!("unsupported output format {format}")),
        }
        .map_err(|error| error.to_string())?;
        stream.play().map_err(|error| error.to_string())?;
        // Room for the device's own latency after the last sample.
        std::thread::sleep(std::time::Duration::from_secs_f32(super::LENGTH + 0.15));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chime_is_two_soft_notes_that_end_in_silence() {
        let samples: Vec<f32> = (0..48_000).map(|frame| sample(frame as f32 / 48_000.0)).collect();
        assert!(samples.iter().all(|sample| sample.abs() <= 0.2));
        assert_eq!(samples[0], 0.0);
        assert!(samples[..(SPLIT * 48_000.0) as usize].iter().any(|sample| sample.abs() > 0.1));
        assert!(samples[(SPLIT * 48_000.0) as usize..(LENGTH * 48_000.0) as usize]
            .iter()
            .any(|sample| sample.abs() > 0.1));
        assert!(samples[(LENGTH * 48_000.0) as usize..].iter().all(|sample| *sample == 0.0));
    }
}
