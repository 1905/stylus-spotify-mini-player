//! The player's audio output: librespot's rodio sink, but with the volume applied at
//! playback time. librespot applies its soft volume when it decodes a packet, and the
//! rodio queue holds ~0.5 s of packets, so a volume change was heard late and in a jump.
//! Here the Player gets a fixed 1.0 volume, and every sample that reaches the device is
//! scaled by a gain that moves toward the SoftMixer's volume over `RAMP_MS`.
//!
//! The device opens on the first `start()`, not in the builder: the builder can't return
//! an error, and librespot's own sink panics there when no output device exists. A failed
//! open makes `start()` fail, the player pauses, and the next play tries again.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use librespot_playback::{
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
    mixer::{Mixer, VolumeGetter},
    NUM_CHANNELS, SAMPLE_RATE,
};
use rodio::cpal::traits::{DeviceTrait, HostTrait};

/// A full 0 → 1 volume swing takes this long; smaller changes take less.
pub const RAMP_MS: f32 = 150.0;
/// Packets queued in rodio before `write` waits: ~0.5 s, as in librespot's rodio sink.
const MAX_QUEUED: usize = 26;

/// The largest gain change per frame, so a full 0 → 1 swing takes `ramp_ms`.
pub fn ramp_step(sample_rate: u32, ramp_ms: f32) -> f32 {
    1.0 / (sample_rate as f32 * ramp_ms / 1000.0).max(1.0)
}

/// The gain for the next frame: `current` moved toward `target` by at most `step`.
/// Lands exactly on the target and never passes it.
pub fn next_gain(current: f32, target: f32, step: f32) -> f32 {
    let diff = target - current;
    if diff.abs() <= step {
        target
    } else {
        current + step.copysign(diff)
    }
}

/// The open output, for the commands that cut what's queued (see flush).
static CURRENT: std::sync::Mutex<Option<std::sync::Weak<rodio::Sink>>> = std::sync::Mutex::new(None);

/// Drop the audio already queued for output (up to MAX_QUEUED packets, ~0.5 s): next, previous,
/// seek and a new load are heard at once instead of after the old song's tail. A paused output
/// stays paused.
pub fn flush() {
    let sink = CURRENT.lock().ok().and_then(|g| g.as_ref().and_then(std::sync::Weak::upgrade));
    if let Some(sink) = sink {
        let paused = sink.is_paused();
        sink.clear(); // clears and pauses
        if !paused {
            sink.play();
        }
    }
}

/// The output stage. Built inside librespot's player thread and used only there.
pub struct RampSink {
    mixer: Arc<dyn Mixer>,
    /// The gain of the last frame played (f32 bits), carried from one packet to the next.
    gain: Arc<AtomicU32>,
    out: Option<(Arc<rodio::Sink>, rodio::OutputStream)>,
}

impl RampSink {
    /// `mixer` is the SoftMixer Spirc drives; its mapped volume is the target gain.
    pub fn new(mixer: Arc<dyn Mixer>) -> Self {
        let start = mixer.get_soft_volume().attenuation_factor() as f32;
        RampSink { mixer, gain: Arc::new(AtomicU32::new(start.to_bits())), out: None }
    }

    fn opened(&mut self) -> SinkResult<&rodio::Sink> {
        if self.out.is_none() {
            let out = open_device().map_err(|e| {
                log::error!("audio output: could not open the device: {e}");
                SinkError::ConnectionRefused(e)
            })?;
            let sink = Arc::new(out.0);
            if let Ok(mut g) = CURRENT.lock() {
                *g = Some(Arc::downgrade(&sink));
            }
            self.out = Some((sink, out.1));
        }
        Ok(&self.out.as_ref().expect("opened above").0)
    }
}

impl Sink for RampSink {
    fn start(&mut self) -> SinkResult<()> {
        self.opened()?.play();
        Ok(())
    }

    // a pause is heard at once: the queued audio waits, and start() plays it on resume
    fn stop(&mut self) -> SinkResult<()> {
        if let Some((sink, _)) = &self.out {
            sink.pause();
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet.samples().map_err(|e| SinkError::OnWrite(e.to_string()))?;
        let samples = converter.f64_to_f32(samples);
        let source = Ramped::new(samples, self.mixer.get_soft_volume(), self.gain.clone());
        let sink = self.opened()?;
        sink.append(source);
        while sink.len() > MAX_QUEUED {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

/// The default output device at 44.1 kHz stereo f32 when it can, else its own default
/// config (rodio resamples). Copied from librespot 0.8.0 audio_backend/rodio.rs.
fn open_device() -> Result<(rodio::Sink, rodio::OutputStream), String> {
    let host = rodio::cpal::default_host();
    let device = host.default_output_device().ok_or("no output device")?;
    log::info!("audio output: {}", device.name().as_deref().unwrap_or("[unknown name]"));
    let default_config = device.default_output_config().map_err(|e| e.to_string())?;
    let config = device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .find(|c| c.channels() == NUM_CHANNELS as rodio::cpal::ChannelCount)
        .and_then(|c| {
            c.try_with_sample_rate(rodio::cpal::SampleRate(SAMPLE_RATE))
                .or_else(|| c.try_with_sample_rate(default_config.sample_rate()))
        })
        .unwrap_or(default_config);
    let mut stream = match rodio::OutputStreamBuilder::default()
        .with_device(device.clone())
        .with_config(&config.config())
        .with_sample_format(rodio::cpal::SampleFormat::F32)
        .open_stream()
    {
        Ok(stream) => stream,
        Err(e) => {
            log::warn!("audio output: exact config refused ({e}), falling back to the device default");
            rodio::OutputStreamBuilder::from_device(device)
                .map_err(|e| e.to_string())?
                .open_stream_or_fallback()
                .map_err(|e| e.to_string())?
        }
    };
    stream.log_on_drop(false);
    let sink = rodio::Sink::connect_new(stream.mixer());
    Ok((sink, stream))
}

/// One decoded packet, scaled per frame by a gain ramping toward the mixer's volume.
/// The target is read while the samples play, so a change is heard within one ramp.
struct Ramped {
    samples: std::vec::IntoIter<f32>,
    target: Box<dyn VolumeGetter + Send>,
    shared: Arc<AtomicU32>,
    gain: f32,
    step: f32,
    /// Position inside the current frame (0 = a new frame starts: update the gain).
    channel: u16,
    started: bool,
}

impl Ramped {
    fn new(samples: Vec<f32>, target: Box<dyn VolumeGetter + Send>, shared: Arc<AtomicU32>) -> Self {
        Ramped {
            samples: samples.into_iter(),
            target,
            shared,
            gain: 0.0,
            step: ramp_step(SAMPLE_RATE, RAMP_MS),
            channel: 0,
            started: false,
        }
    }
}

impl Iterator for Ramped {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self.samples.next()?;
        if !self.started {
            // the gain the previous packet ended on, read when this one starts playing
            self.gain = f32::from_bits(self.shared.load(Ordering::Relaxed));
            self.started = true;
        }
        if self.channel == 0 {
            let target = self.target.attenuation_factor() as f32;
            self.gain = next_gain(self.gain, target, self.step);
            self.shared.store(self.gain.to_bits(), Ordering::Relaxed);
        }
        self.channel = (self.channel + 1) % u16::from(NUM_CHANNELS);
        Some(sample * self.gain)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.samples.size_hint()
    }
}

impl rodio::Source for Ramped {
    fn current_span_len(&self) -> Option<usize> {
        Some(self.samples.len())
    }

    fn channels(&self) -> rodio::ChannelCount {
        rodio::ChannelCount::from(NUM_CHANNELS)
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        SAMPLE_RATE
    }

    fn total_duration(&self) -> Option<Duration> {
        let frames = self.samples.len() as u64 / u64::from(NUM_CHANNELS);
        Some(Duration::from_nanos(frames * 1_000_000_000 / u64::from(SAMPLE_RATE)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames until `next_gain` lands on `target`, and every gain on the way.
    fn run(from: f32, target: f32, step: f32) -> (usize, Vec<f32>) {
        let mut gain = from;
        let mut seen = vec![];
        for frame in 1..1_000_000 {
            gain = next_gain(gain, target, step);
            seen.push(gain);
            if gain == target {
                return (frame, seen);
            }
        }
        panic!("never reached {target}");
    }

    #[test]
    fn full_swing_takes_the_ramp_time() {
        let step = ramp_step(44100, RAMP_MS);
        let ramp_frames = (44100.0 * RAMP_MS / 1000.0) as usize;
        for (from, to) in [(0.0, 1.0), (1.0, 0.0)] {
            let (frames, _) = run(from, to, step);
            // within one frame of the ramp length (f32 rounding)
            assert!(frames.abs_diff(ramp_frames) <= 1, "{from}→{to}: {frames} frames, ramp {ramp_frames}");
        }
    }

    #[test]
    fn small_changes_are_faster() {
        let step = ramp_step(44100, RAMP_MS);
        let (frames, _) = run(0.5, 0.6, step);
        let ramp_frames = (44100.0 * RAMP_MS / 1000.0) as usize;
        assert!(frames <= ramp_frames / 10 + 1, "{frames}");
    }

    #[test]
    fn never_overshoots_and_moves_monotonically() {
        let step = ramp_step(44100, RAMP_MS);
        for (from, to) in [(0.0, 1.0), (1.0, 0.0), (0.3, 0.30001), (0.9, 0.2), (0.25, 0.25)] {
            let (_, seen) = run(from, to, step);
            let mut prev = from;
            for g in seen {
                if to >= from {
                    assert!(g >= prev && g <= to, "{from}→{to}: {g}");
                } else {
                    assert!(g <= prev && g >= to, "{from}→{to}: {g}");
                }
                assert!((g - prev).abs() <= step * 1.0001, "step too big: {prev}→{g}");
                prev = g;
            }
        }
    }

    #[test]
    fn at_target_stays() {
        assert_eq!(next_gain(0.7, 0.7, 0.01), 0.7);
        assert_eq!(next_gain(0.7, 0.705, 0.01), 0.705);
    }

    #[test]
    fn ramp_step_scales_with_rate_and_time() {
        assert!((ramp_step(44100, 150.0) - 1.0 / 6615.0).abs() < 1e-9);
        assert!(ramp_step(48000, 150.0) < ramp_step(44100, 150.0));
        assert!(ramp_step(44100, 300.0) < ramp_step(44100, 150.0));
        // zero ramp time: jump in one frame
        assert_eq!(ramp_step(44100, 0.0), 1.0);
    }

    struct Fixed(f64);
    impl VolumeGetter for Fixed {
        fn attenuation_factor(&self) -> f64 {
            self.0
        }
    }

    #[test]
    fn source_scales_both_channels_alike_and_carries_the_gain() {
        let shared = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let samples = vec![1.0f32; 8]; // 4 stereo frames
        let out: Vec<f32> = Ramped::new(samples, Box::new(Fixed(0.0)), shared.clone()).collect();
        for frame in out.chunks(2) {
            assert_eq!(frame[0], frame[1]);
        }
        let step = ramp_step(SAMPLE_RATE, RAMP_MS);
        assert!((out[0] - (1.0 - step)).abs() < 1e-6, "{}", out[0]);
        assert!((out[6] - (1.0 - 4.0 * step)).abs() < 1e-6, "{}", out[6]);
        // the next packet starts from where this one ended
        assert_eq!(f32::from_bits(shared.load(Ordering::Relaxed)), out[7]);
    }
}
