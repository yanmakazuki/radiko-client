//! CPALのコールバックは左右一組のフレームを消費し、ロック・メモリ確保・待機を行わない。
use super::aac::DecodedPcm;
use cpal::{
    SampleFormat, SampleRate,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq, Eq)]
pub enum OutputError {
    InvalidPcm,
    NoDevice,
    DeviceInfo,
    UnsupportedFormat,
    IncompatibleOutput {
        requested_rate: u32,
        offered: Vec<OfferedOutputFormat>,
    },
    Device,
    Stalled,
    ReconnectGaveUp,
    ProbeTimeout,
    Deadline,
}
impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPcm => f.write_str("出力用PCMまたは再生時間が不正です"),
            Self::NoDevice => f.write_str("既定の音声出力機器がありません"),
            Self::DeviceInfo => f.write_str("音声出力機器の情報を取得できません"),
            Self::UnsupportedFormat => f.write_str(
                "出力中のサンプルレートまたはチャンネル数が、開いた音声と一致しません",
            ),
            Self::IncompatibleOutput {
                requested_rate,
                offered,
            } => f.write_str(&incompatible_message(*requested_rate, offered)),
            Self::Device => f.write_str("音声出力機器の開始・実行に失敗しました"),
            Self::Stalled => f.write_str(
                "音声コールバックが進まないため出力を停止しました。スリープ復帰・機器変更後は出力先を確認し、番組を選び直して再生してください",
            ),
            Self::ReconnectGaveUp => f.write_str(crate::reconnect::GAVE_UP),
            Self::ProbeTimeout => {
                f.write_str("無音出力診断の実時間上限を超えたため停止しました")
            }
            Self::Deadline => {
                f.write_str("番組終了：放送終了日時に達したため音声を停止しました")
            }
        }
    }
}
impl std::error::Error for OutputError {}

pub const PCM_SAMPLE_RATE_RANGE: std::ops::RangeInclusive<u32> = 8_000..=192_000;

/// Device sample formats this process can open without resampling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputSampleFormat {
    F32,
    I16,
    I32,
}
impl OutputSampleFormat {
    fn from_cpal(format: SampleFormat) -> Option<Self> {
        match format {
            SampleFormat::F32 => Some(Self::F32),
            SampleFormat::I16 => Some(Self::I16),
            SampleFormat::I32 => Some(Self::I32),
            _ => None,
        }
    }
}
impl fmt::Display for OutputSampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::F32 => "f32",
            Self::I16 => "i16",
            Self::I32 => "i32",
        })
    }
}

/// One range the device declared. Not a promise that playback will succeed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferedOutputFormat {
    pub channels: u16,
    pub min_sample_rate: u32,
    pub max_sample_rate: u32,
    pub sample_format: &'static str,
}

/// Read-time OS capability declaration, not an opened stream or a playback result.
pub struct OutputDeviceInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: Option<u16>,
    pub sample_format: Option<OutputSampleFormat>,
    /// Japanese explanation when no f32/i16/i32 mono/stereo range contains `sample_rate`.
    pub mismatch: String,
}

fn format_preference(format: SampleFormat) -> Option<u8> {
    match format {
        SampleFormat::F32 => Some(0),
        SampleFormat::I32 => Some(1),
        SampleFormat::I16 => Some(2),
        _ => None,
    }
}

fn sample_format_label(format: SampleFormat) -> &'static str {
    match format {
        SampleFormat::F32 => "f32",
        SampleFormat::I16 => "i16",
        SampleFormat::I32 => "i32",
        SampleFormat::I8 => "i8",
        SampleFormat::I64 => "i64",
        SampleFormat::U8 => "u8",
        SampleFormat::U16 => "u16",
        SampleFormat::U32 => "u32",
        SampleFormat::U64 => "u64",
        SampleFormat::F64 => "f64",
        _ => "その他",
    }
}

/// Same-rate mono or stereo. f32 wins over i32, then i16. More channels win within a format.
fn compatible_config(
    configs: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
    rate: u32,
) -> Option<cpal::SupportedStreamConfigRange> {
    configs
        .filter(|config| {
            format_preference(config.sample_format()).is_some()
                && matches!(config.channels(), 1 | 2)
                && config.min_sample_rate().0 <= rate
                && config.max_sample_rate().0 >= rate
        })
        .min_by_key(|config| {
            (
                format_preference(config.sample_format()).unwrap_or(u8::MAX),
                std::cmp::Reverse(config.channels()),
            )
        })
}

fn offered_formats(configs: &[cpal::SupportedStreamConfigRange]) -> Vec<OfferedOutputFormat> {
    configs
        .iter()
        .map(|config| OfferedOutputFormat {
            channels: config.channels(),
            min_sample_rate: config.min_sample_rate().0,
            max_sample_rate: config.max_sample_rate().0,
            sample_format: sample_format_label(config.sample_format()),
        })
        .collect()
}

fn incompatible_output(
    requested_rate: u32,
    configs: &[cpal::SupportedStreamConfigRange],
) -> OutputError {
    OutputError::IncompatibleOutput {
        requested_rate,
        offered: offered_formats(configs),
    }
}

/// Opens only at `rate`. A device range that does not contain `rate` is an error.
/// This does not substitute another rate from the device.
fn select_playback_config(
    configs: &[cpal::SupportedStreamConfigRange],
    rate: u32,
) -> Result<(cpal::StreamConfig, OutputSampleFormat), OutputError> {
    if configs.is_empty() {
        return Err(empty_output_error(rate));
    }
    let supported = compatible_config(configs.iter().cloned(), rate)
        .ok_or_else(|| incompatible_output(rate, configs))?;
    let sample_format = OutputSampleFormat::from_cpal(supported.sample_format())
        .ok_or_else(|| incompatible_output(rate, configs))?;
    let config = supported
        .try_with_sample_rate(SampleRate(rate))
        .ok_or_else(|| incompatible_output(rate, configs))?
        .config();
    if config.sample_rate.0 != rate {
        return Err(incompatible_output(rate, configs));
    }
    Ok((config, sample_format))
}

fn incompatible_message(requested_rate: u32, offered: &[OfferedOutputFormat]) -> String {
    let mut ranges = String::new();
    if offered.is_empty() {
        ranges.push_str("なし");
    }
    for (index, offer) in offered.iter().enumerate() {
        if index == 12 {
            ranges.push_str("、ほか");
            break;
        }
        if index > 0 {
            ranges.push('、');
        }
        if offer.min_sample_rate == offer.max_sample_rate {
            ranges.push_str(&format!("{}Hz", offer.min_sample_rate));
        } else {
            ranges.push_str(&format!(
                "{}〜{}Hz",
                offer.min_sample_rate, offer.max_sample_rate
            ));
        }
        ranges.push('・');
        ranges.push_str(offer.sample_format);
        ranges.push('・');
        ranges.push_str(&format!("{}ch", offer.channels));
    }
    format!(
        "必要な形式は{requested_rate}Hzのmonoまたはstereoのf32・i16・i32です。機器の対応範囲: {ranges}"
    )
}

fn config_query_error(for_playback: bool) -> OutputError {
    #[cfg(target_os = "linux")]
    {
        let _ = for_playback;
        OutputError::NoDevice
    }
    #[cfg(not(target_os = "linux"))]
    if for_playback {
        OutputError::Device
    } else {
        OutputError::DeviceInfo
    }
}

fn supported_configs(
    device: &cpal::Device,
    on_error: OutputError,
) -> Result<Vec<cpal::SupportedStreamConfigRange>, OutputError> {
    device
        .supported_output_configs()
        .map(|configs| configs.collect())
        .map_err(|_| on_error)
}

/// `RADIKO_ALSA_LOG=1` leaves ALSA's own stderr handler in place.
fn alsa_diagnostics_suppressed(value: Option<&str>) -> bool {
    value != Some("1")
}

fn prepare_alsa_errors() {
    let suppress = alsa_diagnostics_suppressed(std::env::var("RADIKO_ALSA_LOG").ok().as_deref());
    #[cfg(target_os = "linux")]
    if suppress {
        install_silent_alsa_error_handler();
    }
    #[cfg(not(target_os = "linux"))]
    let _ = suppress;
}

#[cfg(target_os = "linux")]
static ALSA_ERROR_HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn radiko_install_silent_alsa_error_handler();
}

#[cfg(target_os = "linux")]
fn install_silent_alsa_error_handler() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        radiko_install_silent_alsa_error_handler();
        ALSA_ERROR_HANDLER_INSTALLED.store(true, Ordering::SeqCst);
    });
}

fn empty_output_error(rate: u32) -> OutputError {
    #[cfg(target_os = "linux")]
    {
        let _ = rate;
        OutputError::NoDevice
    }
    #[cfg(not(target_os = "linux"))]
    incompatible_output(rate, &[])
}

fn open_device_stream<T, F>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut renderer: Renderer,
    silence: T,
    encode: F,
) -> Result<cpal::Stream, OutputError>
where
    T: cpal::SizedSample + Send + 'static,
    F: Fn(f32) -> T + Send + 'static,
{
    let channels = usize::from(config.channels);
    let failed = renderer.state.clone();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], info| {
                let timestamp = info.timestamp();
                let delay = timestamp
                    .playback
                    .duration_since(&timestamp.callback)
                    .unwrap_or_default();
                let now = Instant::now();
                renderer.render_samples(
                    data,
                    channels,
                    now.checked_add(delay).unwrap_or(now),
                    silence,
                    &encode,
                );
            },
            move |_| {
                failed.failed.store(true, Ordering::Release);
            },
            None,
        )
        .map_err(|_| OutputError::Device)
}

/// The enumerated output whose name matches CPAL's current default.
/// On macOS, CPAL 0.16 names that device after the CoreAudio default and registers
/// `DeviceIsAlive` only for enumerated devices. The default wrapper follows later OS
/// default changes and is used only when no enumerated name matches.
/// On Linux, CPAL 0.16's ALSA host names the default PCM `default`. Enumeration
/// includes that name when it opens, so this selects the `default` PCM.
fn selected_output_device() -> Result<cpal::Device, OutputError> {
    prepare_alsa_errors();
    let host = cpal::default_host();
    let fallback = host.default_output_device().ok_or(OutputError::NoDevice)?;
    let name = fallback.name().map_err(|_| OutputError::DeviceInfo)?;
    if let Ok(devices) = host.output_devices() {
        for device in devices {
            if device.name().ok().as_deref() == Some(name.as_str()) {
                return Ok(device);
            }
        }
    }
    Ok(fallback)
}

pub fn inspect_default_output(sample_rate: u32) -> Result<OutputDeviceInfo, OutputError> {
    if !PCM_SAMPLE_RATE_RANGE.contains(&sample_rate) {
        return Err(OutputError::InvalidPcm);
    }
    let device = selected_output_device()?;
    let name = device.name().map_err(|_| OutputError::DeviceInfo)?;
    let name: String = name.chars().filter(|c| !c.is_control()).take(256).collect();
    let configs = supported_configs(&device, config_query_error(false))?;
    #[cfg(target_os = "linux")]
    if configs.is_empty() {
        return Err(OutputError::NoDevice);
    }
    let chosen = compatible_config(configs.iter().cloned(), sample_rate);
    let sample_format = chosen
        .as_ref()
        .and_then(|config| OutputSampleFormat::from_cpal(config.sample_format()));
    let mismatch = if chosen.is_some() {
        String::new()
    } else {
        incompatible_output(sample_rate, &configs).to_string()
    };
    Ok(OutputDeviceInfo {
        name,
        sample_rate,
        channels: chosen.map(|config| config.channels()),
        sample_format,
        mismatch,
    })
}

#[derive(Default)]
struct StopState {
    cancelled: AtomicBool,
    parent: Option<StopToken>,
}
#[derive(Clone, Default)]
pub struct StopToken(Arc<StopState>);
impl StopToken {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
            || self.0.parent.as_ref().is_some_and(StopToken::is_cancelled)
    }
    pub fn child(&self) -> Self {
        Self(Arc::new(StopState {
            cancelled: AtomicBool::new(false),
            parent: Some(self.clone()),
        }))
    }
}

#[derive(Default)]
struct State {
    paused: AtomicBool,
    volume: AtomicU32,
    stopped: AtomicBool,
    enabled: AtomicBool,
    failed: AtomicBool,
    ended: AtomicBool,
    expired: AtomicBool,
    frames: AtomicU64,
    callbacks: AtomicU64,
    total: AtomicU64,
    underrun_frames: AtomicU64,
}
#[derive(Debug)]
pub struct OutputSummary {
    pub frames: u64,
    pub underrun_frames: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: OutputSampleFormat,
}

struct Renderer {
    queue: Consumer<[f32; 2]>,
    state: Arc<State>,
    cancel: StopToken,
    rate: u32,
    deadline: Option<Instant>,
}
fn encode_f32(sample: f32) -> f32 {
    sample
}

fn encode_i16(sample: f32) -> i16 {
    let clipped = sample.clamp(-1.0, 1.0);
    (clipped * 32767.0).round().clamp(-32767.0, 32767.0) as i16
}

fn encode_i32(sample: f32) -> i32 {
    let clipped = sample.clamp(-1.0, 1.0);
    if clipped >= 1.0 {
        return i32::MAX;
    }
    if clipped <= -1.0 {
        return i32::MIN;
    }
    let scaled = (clipped * 2147483647.0).round();
    if scaled >= 2147483647.0 {
        i32::MAX
    } else if scaled <= -2147483648.0 {
        i32::MIN
    } else {
        scaled as i32
    }
}

impl Renderer {
    #[cfg(test)]
    fn render(&mut self, data: &mut [f32], channels: usize, now: Instant) {
        self.render_samples(data, channels, now, 0.0, encode_f32);
    }

    fn render_samples<T: Copy, F: Fn(f32) -> T>(
        &mut self,
        data: &mut [T],
        channels: usize,
        now: Instant,
        silence: T,
        encode: F,
    ) {
        self.state.callbacks.fetch_add(1, Ordering::Release);
        data.fill(silence);
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.state.expired.store(true, Ordering::Release);
            self.state.stopped.store(true, Ordering::Release);
        }
        if !self.state.enabled.load(Ordering::Acquire) || self.state.paused.load(Ordering::Acquire)
        {
            return;
        }
        let gain = self.state.volume.load(Ordering::Acquire) as f32 / 100.0;
        let mut frames = self.state.frames.load(Ordering::Relaxed);
        let mut missing = 0;
        let available = self.deadline.map_or(usize::MAX, |end| {
            (end.saturating_duration_since(now).as_secs_f64() * f64::from(self.rate)).floor()
                as usize
        });
        for (index, output) in data.chunks_exact_mut(channels).enumerate() {
            if self.cancel.is_cancelled()
                || self.state.stopped.load(Ordering::Acquire)
                || self.state.failed.load(Ordering::Acquire)
            {
                break;
            }
            if frames >= self.state.total.load(Ordering::Acquire) {
                break;
            }
            if index >= available {
                self.state.expired.store(true, Ordering::Release);
                self.state.stopped.store(true, Ordering::Release);
                break;
            }
            match self.queue.pop() {
                Ok(frame) => {
                    if channels == 1 {
                        output[0] = encode((frame[0] + frame[1]) * 0.5 * gain);
                    } else {
                        output[0] = encode(frame[0] * gain);
                        output[1] = encode(frame[1] * gain);
                    }
                    frames += 1;
                }
                Err(_) => missing += 1,
            }
        }
        self.state.frames.store(frames, Ordering::Release);
        self.state
            .underrun_frames
            .fetch_add(missing, Ordering::Relaxed);
        if frames >= self.state.total.load(Ordering::Acquire) {
            self.state.ended.store(true, Ordering::Release);
        }
    }
}

fn queue(rate: u32, total: u64, deadline: Option<Instant>) -> (Producer<[f32; 2]>, Renderer) {
    let (producer, consumer) = RingBuffer::new(rate as usize * 2);
    let state = Arc::new(State::default());
    state.volume.store(100, Ordering::Release);
    state.total.store(total, Ordering::Release);
    state.enabled.store(true, Ordering::Release);
    (
        producer,
        Renderer {
            queue: consumer,
            state,
            cancel: StopToken::default(),
            rate,
            deadline,
        },
    )
}

struct Output {
    stream: cpal::Stream,
    producer: Producer<[f32; 2]>,
    state: Arc<State>,
    channels: u16,
    sample_format: OutputSampleFormat,
}
impl Drop for Output {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
        let _ = self.stream.pause();
        // Dropping the stream and its consumer discards the remaining queue.
    }
}
impl Output {
    fn open(rate: u32, total: u64, deadline: Option<Instant>) -> Result<Self, OutputError> {
        Self::open_with_stop(rate, total, deadline, StopToken::default())
    }
    fn open_with_stop(
        rate: u32,
        total: u64,
        deadline: Option<Instant>,
        stop: StopToken,
    ) -> Result<Self, OutputError> {
        let device = selected_output_device()?;
        let configs = supported_configs(&device, config_query_error(true))?;
        let (config, sample_format) = select_playback_config(&configs, rate)?;
        let (producer, mut renderer) = queue(rate, total, deadline);
        renderer.state.enabled.store(false, Ordering::Release);
        renderer.cancel = stop;
        let state = renderer.state.clone();
        let stream = match sample_format {
            OutputSampleFormat::F32 => {
                open_device_stream(&device, &config, renderer, 0.0, encode_f32)?
            }
            OutputSampleFormat::I16 => {
                open_device_stream(&device, &config, renderer, 0, encode_i16)?
            }
            OutputSampleFormat::I32 => {
                open_device_stream(&device, &config, renderer, 0, encode_i32)?
            }
        };
        Ok(Self {
            stream,
            producer,
            state,
            channels: config.channels,
            sample_format,
        })
    }
    fn check(&self) -> Result<(), OutputError> {
        if self.state.failed.load(Ordering::Acquire) {
            Err(OutputError::Device)
        } else if self.state.expired.load(Ordering::Acquire) {
            Err(OutputError::Deadline)
        } else {
            Ok(())
        }
    }
}

/// One validated, bounded sample. No resampling or OS decoding is performed.
/// i16 and i32 devices are converted from f32 in the callback.
/// `remaining` is the selected live slot's remaining wall-clock time.
pub fn play_pcm(
    pcm: &DecodedPcm,
    remaining: Option<Duration>,
) -> Result<OutputSummary, OutputError> {
    let rate = pcm.summary.sample_rate;
    let channels = usize::from(pcm.summary.channels);
    if !PCM_SAMPLE_RATE_RANGE.contains(&rate)
        || !(1..=2).contains(&channels)
        || pcm.samples.is_empty()
        || pcm.samples.len() > 1_048_576
        || pcm.samples.len() % channels != 0
        || pcm.samples.len() / channels != pcm.summary.frames
        || pcm.samples.iter().any(|v| !v.is_finite())
    {
        return Err(OutputError::InvalidPcm);
    }
    let now = Instant::now();
    let deadline = remaining
        .map(|duration| now.checked_add(duration).ok_or(OutputError::InvalidPcm))
        .transpose()?;
    if remaining == Some(Duration::ZERO) {
        return Err(OutputError::Deadline);
    }
    let total = pcm.summary.frames as u64;
    let mut output = Output::open(rate, total, deadline)?;
    let mut cursor = 0;
    let mut started = false;
    let mut last_frames = 0;
    let mut last_progress = Instant::now();
    loop {
        output.check()?;
        if deadline.is_some_and(|end| Instant::now() >= end) {
            return Err(OutputError::Deadline);
        }
        while cursor < pcm.samples.len() && output.producer.slots() > 0 {
            let left = pcm.samples[cursor].clamp(-1.0, 1.0) * 0.25;
            let right = if channels == 2 {
                pcm.samples[cursor + 1].clamp(-1.0, 1.0) * 0.25
            } else {
                left
            };
            if output.producer.push([left, right]).is_err() {
                break;
            }
            cursor += channels;
        }
        if !started {
            output.stream.play().map_err(|_| OutputError::Device)?;
            output.state.enabled.store(true, Ordering::Release);
            started = true;
        }
        let frames = output.state.frames.load(Ordering::Acquire);
        if output.state.ended.load(Ordering::Acquire) {
            // The last callback has submitted audio, not necessarily played it yet.
            // Keep the stream alive for a bounded hardware drain, checking errors/deadline.
            let drain_end = Instant::now() + Duration::from_millis(250);
            while Instant::now() < drain_end {
                output.check()?;
                if deadline.is_some_and(|end| Instant::now() >= end) {
                    return Err(OutputError::Deadline);
                }
                thread::sleep(Duration::from_millis(5));
            }
            return Ok(OutputSummary {
                frames: output.state.frames.load(Ordering::Acquire),
                underrun_frames: output.state.underrun_frames.load(Ordering::Relaxed),
                sample_rate: rate,
                channels: output.channels,
                sample_format: output.sample_format,
            });
        }
        if frames != last_frames {
            last_frames = frames;
            last_progress = Instant::now();
        }
        if last_progress.elapsed() > Duration::from_secs(3) {
            return Err(OutputError::Stalled);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Keeps one device stream alive across segment boundaries.
pub struct ContinuousOutput {
    output: Output,
    rate: u32,
    source_channels: usize,
    accepted: u64,
    maximum: u64,
    started: bool,
    stop: StopToken,
    deadline: Option<Instant>,
    watch: OutputStallWatch,
    drained_at: Option<Instant>,
}
/// Decides whether a quiet callback is a stopped process, a device to rebuild, or a real stall.
/// Tests pass `now` directly so a sleep or a five-minute wait does not block.
struct OutputStallWatch {
    last_poll: Option<Instant>,
    last_progress: Option<Instant>,
    last_callback: u64,
    started: bool,
    reconnecting: bool,
    rebuilt: bool,
    elapsed: Duration,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputWatch {
    Continue,
    /// `poll` itself was late. The baseline was reset. This is not `Stalled`.
    Suspended,
    Rebuild,
    Stalled,
    Expired,
}
impl OutputStallWatch {
    fn new() -> Self {
        Self {
            last_poll: None,
            last_progress: None,
            last_callback: 0,
            started: false,
            reconnecting: false,
            rebuilt: false,
            elapsed: Duration::ZERO,
        }
    }
    fn start(&mut self, now: Instant) {
        self.started = true;
        self.last_progress = Some(now);
    }
    fn reconnecting_for(&self) -> Option<Duration> {
        self.reconnecting.then_some(self.elapsed)
    }
    /// After a new device is opened, its callback counter starts again.
    fn rebase(&mut self, now: Instant, callbacks: u64) {
        self.last_poll = Some(now);
        self.last_progress = Some(now);
        self.last_callback = callbacks;
    }
    fn poll(&mut self, now: Instant, callbacks: u64) -> OutputWatch {
        let gap = self
            .last_poll
            .map(|then| now.saturating_duration_since(then));
        self.last_poll = Some(now);
        if gap.is_some_and(|gap| gap > crate::reconnect::OUTPUT_POLL_GAP) {
            self.last_callback = callbacks;
            self.last_progress = Some(now);
            if self.started {
                self.reconnecting = true;
                self.rebuilt = false;
                self.elapsed = Duration::ZERO;
                return OutputWatch::Suspended;
            }
            return OutputWatch::Continue;
        }
        if self.reconnecting {
            if let Some(gap) = gap {
                self.elapsed = self.elapsed.saturating_add(gap);
            }
        }
        if callbacks != self.last_callback {
            self.last_callback = callbacks;
            self.last_progress = Some(now);
            self.reconnecting = false;
            self.rebuilt = false;
            self.elapsed = Duration::ZERO;
            return OutputWatch::Continue;
        }
        if self.reconnecting && self.elapsed > crate::reconnect::LIMIT {
            return OutputWatch::Expired;
        }
        let frozen = self.started
            && self.last_progress.is_some_and(|at| {
                now.saturating_duration_since(at) > crate::reconnect::OUTPUT_CALLBACK_WAIT
            });
        if frozen && self.reconnecting && !self.rebuilt {
            self.rebuilt = true;
            self.last_progress = Some(now);
            return OutputWatch::Rebuild;
        }
        if frozen && !self.reconnecting {
            return OutputWatch::Stalled;
        }
        OutputWatch::Continue
    }
}
impl ContinuousOutput {
    pub fn new(
        pcm: &DecodedPcm,
        maximum: u64,
        remaining: Option<Duration>,
        stop: StopToken,
    ) -> Result<Self, OutputError> {
        validate_pcm(pcm)?;
        if maximum == 0 || remaining == Some(Duration::ZERO) {
            return Err(OutputError::Deadline);
        }
        let deadline = remaining
            .map(|d| Instant::now().checked_add(d).ok_or(OutputError::InvalidPcm))
            .transpose()?;
        let rate = pcm.summary.sample_rate;
        let output = Output::open_with_stop(rate, maximum, deadline, stop.clone())?;
        output.state.volume.store(25, Ordering::Release);
        Ok(Self {
            output,
            rate,
            source_channels: usize::from(pcm.summary.channels),
            accepted: 0,
            maximum,
            started: false,
            stop,
            deadline,
            watch: OutputStallWatch::new(),
            drained_at: None,
        })
    }
    /// Nonblocking producer step; caller retains the current segment until complete.
    pub fn enqueue(&mut self, pcm: &DecodedPcm, cursor: &mut usize) -> Result<bool, OutputError> {
        if *cursor == 0 {
            validate_pcm(pcm)?;
        }
        if pcm.summary.sample_rate != self.rate
            || usize::from(pcm.summary.channels) != self.source_channels
        {
            return Err(OutputError::UnsupportedFormat);
        }
        self.output.check()?;
        while *cursor < pcm.samples.len()
            && self.accepted < self.maximum
            && self.output.producer.slots() > 0
        {
            let left = pcm.samples[*cursor].clamp(-1.0, 1.0);
            let right = if self.source_channels == 2 {
                pcm.samples[*cursor + 1].clamp(-1.0, 1.0)
            } else {
                left
            };
            if self.output.producer.push([left, right]).is_err() {
                break;
            }
            *cursor += self.source_channels;
            self.accepted += 1;
        }
        if !self.started && self.accepted >= self.maximum.min(u64::from(self.rate) * 2) {
            self.output.stream.play().map_err(|_| OutputError::Device)?;
            self.output.state.enabled.store(true, Ordering::Release);
            self.started = true;
            self.watch.start(Instant::now());
        }
        Ok(*cursor == pcm.samples.len() || self.accepted == self.maximum)
    }
    pub fn finish(&mut self) {
        if !self.started {
            match self.output.stream.play() {
                Ok(()) => {
                    self.output.state.enabled.store(true, Ordering::Release);
                    self.started = true;
                    self.watch.start(Instant::now());
                }
                Err(_) => self.output.state.failed.store(true, Ordering::Release),
            }
        }
        self.output
            .state
            .total
            .store(self.accepted, Ordering::Release);
    }
    pub fn accepted(&self) -> u64 {
        self.accepted
    }
    pub fn summary(&self) -> OutputSummary {
        OutputSummary {
            frames: self.output.state.frames.load(Ordering::Acquire),
            underrun_frames: self.output.state.underrun_frames.load(Ordering::Relaxed),
            sample_rate: self.rate,
            channels: self.output.channels,
            sample_format: self.output.sample_format,
        }
    }
    pub fn configure(&self, paused: bool, volume: u32) {
        self.output.state.paused.store(paused, Ordering::Release);
        self.output
            .state
            .volume
            .store(volume.min(100), Ordering::Release);
    }
    pub fn reconnecting_for(&self) -> Option<Duration> {
        self.watch.reconnecting_for()
    }
    pub fn poll(&mut self) -> Result<bool, OutputError> {
        self.output.check()?;
        let callbacks = self.output.state.callbacks.load(Ordering::Acquire);
        if self.output.state.ended.load(Ordering::Acquire) {
            let start = self.drained_at.get_or_insert_with(Instant::now);
            return Ok(start.elapsed() >= Duration::from_millis(250));
        }
        let now = Instant::now();
        match self.watch.poll(now, callbacks) {
            OutputWatch::Continue | OutputWatch::Suspended => Ok(false),
            OutputWatch::Rebuild => {
                self.rebuild()?;
                Ok(false)
            }
            OutputWatch::Stalled => Err(OutputError::Stalled),
            OutputWatch::Expired => Err(OutputError::ReconnectGaveUp),
        }
    }
    fn rebuild(&mut self) -> Result<(), OutputError> {
        let paused = self.output.state.paused.load(Ordering::Acquire);
        let volume = self.output.state.volume.load(Ordering::Acquire);
        let output =
            Output::open_with_stop(self.rate, self.maximum, self.deadline, self.stop.clone())
                .map_err(|_| OutputError::ReconnectGaveUp)?;
        output.state.paused.store(paused, Ordering::Release);
        output.state.volume.store(volume, Ordering::Release);
        if self.started {
            output
                .stream
                .play()
                .map_err(|_| OutputError::ReconnectGaveUp)?;
            output.state.enabled.store(true, Ordering::Release);
        }
        self.output = output;
        let callbacks = self.output.state.callbacks.load(Ordering::Acquire);
        self.watch.rebase(Instant::now(), callbacks);
        Ok(())
    }
}
fn validate_pcm(pcm: &DecodedPcm) -> Result<(), OutputError> {
    let channels = usize::from(pcm.summary.channels);
    if !PCM_SAMPLE_RATE_RANGE.contains(&pcm.summary.sample_rate)
        || !(1..=2).contains(&channels)
        || pcm.samples.is_empty()
        || pcm.samples.len() > 1_048_576
        || pcm.samples.len() % channels != 0
        || pcm.samples.len() / channels != pcm.summary.frames
        || pcm.samples.iter().any(|v| !v.is_finite())
    {
        return Err(OutputError::InvalidPcm);
    }
    Ok(())
}

/// Explicit hardware-only probe: one second of zero PCM, no source or decoder.
pub fn probe_silent(
    seconds: u32,
    stop: StopToken,
    mut progress: impl FnMut(OutputSummary),
) -> Result<OutputSummary, OutputError> {
    if !(1..=1800).contains(&seconds) {
        return Err(OutputError::InvalidPcm);
    }
    let pcm = silent_pcm();
    let maximum = u64::from(pcm.summary.sample_rate) * u64::from(seconds);
    let mut output = ContinuousOutput::new(&pcm, maximum, None, stop.clone())?;
    output.configure(false, 0);
    let mut cursor = 0;
    let mut report_at = Instant::now();
    let deadline = report_at + Duration::from_secs(u64::from(seconds) + 30);
    loop {
        if stop.is_cancelled() {
            return Ok(output.summary());
        }
        if Instant::now() >= deadline {
            return Err(OutputError::ProbeTimeout);
        }
        if output.enqueue(&pcm, &mut cursor)? {
            cursor = 0;
        }
        if output.poll()? {
            return Ok(output.summary());
        }
        if Instant::now() >= report_at {
            progress(output.summary());
            report_at = Instant::now() + Duration::from_secs(5);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn silent_pcm() -> DecodedPcm {
    DecodedPcm {
        samples: vec![0.0; 48_000 * 2],
        summary: super::aac::AacSummary {
            core_sample_rate: 48_000,
            core_channels: 2,
            sample_rate: 48_000,
            channels: 2,
            packets: 0,
            frames: 48_000,
            sbr_detected: false,
            stereo_expansion: false,
            explicit_ps: false,
            media_duration: Duration::from_secs(1),
            decode_duration: Duration::ZERO,
            nonzero_samples: 0,
        },
    }
}

pub fn probe_tone(seconds: u32) -> Result<OutputSummary, OutputError> {
    if !(1..=10).contains(&seconds) {
        return Err(OutputError::InvalidPcm);
    }
    let rate = 48_000;
    let frames = rate as usize * seconds as usize;
    let samples = (0..frames)
        .flat_map(|i| {
            let envelope = (i.min(frames - 1 - i) as f32 / 960.0).min(1.0);
            let value =
                (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.1 * envelope;
            [value, value]
        })
        .collect();
    play_pcm(
        &DecodedPcm {
            samples,
            summary: super::aac::AacSummary {
                core_sample_rate: rate,
                core_channels: 2,
                sample_rate: rate,
                channels: 2,
                packets: 0,
                frames,
                sbr_detected: false,
                stereo_expansion: false,
                explicit_ps: false,
                media_duration: Duration::from_secs(u64::from(seconds)),
                decode_duration: Duration::ZERO,
                nonzero_samples: 0,
            },
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn device_inspection_rejects_rates_outside_the_pcm_range_before_opening_hardware() {
        use super::*;
        for rate in [0, 7999, 192001, u32::MAX] {
            assert!(matches!(
                inspect_default_output(rate),
                Err(OutputError::InvalidPcm)
            ));
        }
    }

    use super::*;

    fn range(
        channels: u16,
        low: u32,
        high: u32,
        format: SampleFormat,
    ) -> cpal::SupportedStreamConfigRange {
        cpal::SupportedStreamConfigRange::new(
            channels,
            SampleRate(low),
            SampleRate(high),
            cpal::SupportedBufferSize::Unknown,
            format,
        )
    }

    #[test]
    fn compatible_config_prefers_f32_then_i32_then_i16_and_stereo_within_a_format() {
        let mixed = [
            range(1, 24_000, 48_000, SampleFormat::F32),
            range(2, 48_000, 48_000, SampleFormat::F32),
            range(2, 8_000, 192_000, SampleFormat::I16),
            range(6, 8_000, 192_000, SampleFormat::F32),
        ];
        let at_48k = compatible_config(mixed.into_iter(), 48_000).unwrap();
        assert_eq!(at_48k.sample_format(), SampleFormat::F32);
        assert_eq!(at_48k.channels(), 2);
        let at_24k = compatible_config(mixed.into_iter(), 24_000).unwrap();
        assert_eq!(at_24k.sample_format(), SampleFormat::F32);
        assert_eq!(at_24k.channels(), 1);
        let wide_integer = compatible_config(mixed.into_iter(), 96_000).unwrap();
        assert_eq!(wide_integer.sample_format(), SampleFormat::I16);
        assert_eq!(wide_integer.channels(), 2);
        for rate in [7_999, 192_001] {
            assert!(compatible_config(mixed.into_iter(), rate).is_none());
        }

        let integers = [
            range(2, 48_000, 48_000, SampleFormat::I16),
            range(1, 48_000, 48_000, SampleFormat::I32),
            range(8, 48_000, 48_000, SampleFormat::I32),
        ];
        let chosen = compatible_config(integers.into_iter(), 48_000).unwrap();
        assert_eq!(chosen.sample_format(), SampleFormat::I32);
        assert_eq!(chosen.channels(), 1);

        let stereo_i16 = [
            range(1, 48_000, 48_000, SampleFormat::I16),
            range(2, 44_100, 48_000, SampleFormat::I16),
        ];
        let chosen = compatible_config(stereo_i16.into_iter(), 48_000).unwrap();
        assert_eq!(chosen.sample_format(), SampleFormat::I16);
        assert_eq!(chosen.channels(), 2);

        let only_i32 = [range(2, 48_000, 48_000, SampleFormat::I32)];
        assert_eq!(
            compatible_config(only_i32.into_iter(), 48_000)
                .unwrap()
                .sample_format(),
            SampleFormat::I32
        );
    }

    #[test]
    fn rate_mismatch_names_the_requested_rate_and_the_device_ranges() {
        let offered = [
            range(2, 44_100, 44_100, SampleFormat::F32),
            range(2, 44_100, 44_100, SampleFormat::I16),
            range(6, 48_000, 48_000, SampleFormat::F32),
        ];
        let text = incompatible_output(48_000, &offered).to_string();
        assert!(text.contains("48000"), "{text}");
        assert!(text.contains("44100"), "{text}");
        assert!(text.contains("f32"), "{text}");
        assert!(text.contains("i16"), "{text}");
        assert!(text.contains("2ch"), "{text}");
        assert!(text.contains("6ch"), "{text}");
        assert!(text.contains("monoまたはstereo"), "{text}");
        let empty = incompatible_output(48_000, &[]).to_string();
        assert!(empty.contains("48000"), "{empty}");
        assert!(empty.contains("なし"), "{empty}");
    }

    #[test]
    fn sample_rate_outside_every_device_range_errors_instead_of_opening_at_another_rate() {
        let only_cd = [
            range(2, 44_100, 44_100, SampleFormat::F32),
            range(2, 44_100, 44_100, SampleFormat::I16),
            range(1, 8_000, 44_100, SampleFormat::I32),
        ];
        let error = select_playback_config(&only_cd, 48_000).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("必要な形式は48000Hz"), "{text}");
        assert!(text.contains("44100Hz"), "{text}");
        assert!(text.contains("8000〜44100Hz"), "{text}");
        match error {
            OutputError::IncompatibleOutput {
                requested_rate,
                offered,
            } => {
                assert_eq!(requested_rate, 48_000);
                assert!(offered.iter().all(|offer| {
                    offer.max_sample_rate < requested_rate || offer.min_sample_rate > requested_rate
                }));
            }
            other => panic!("expected a rate mismatch, got {other}"),
        }

        let above = [range(2, 96_000, 96_000, SampleFormat::F32)];
        let error = select_playback_config(&above, 48_000).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("48000"), "{text}");
        assert!(text.contains("96000"), "{text}");
        assert!(matches!(
            error,
            OutputError::IncompatibleOutput {
                requested_rate: 48_000,
                ..
            }
        ));

        let opened = select_playback_config(&only_cd, 44_100).unwrap();
        assert_eq!(opened.0.sample_rate, SampleRate(44_100));
        let wide = [range(2, 44_100, 96_000, SampleFormat::F32)];
        let opened = select_playback_config(&wide, 48_000).unwrap();
        assert_eq!(opened.0.sample_rate.0, 48_000);
        assert_ne!(opened.0.sample_rate.0, 44_100);
        assert_ne!(opened.0.sample_rate.0, 96_000);
    }

    #[test]
    fn alsa_library_diagnostics_stay_quiet_unless_the_debug_variable_is_one() {
        assert!(alsa_diagnostics_suppressed(None));
        assert!(alsa_diagnostics_suppressed(Some("")));
        assert!(alsa_diagnostics_suppressed(Some("0")));
        assert!(alsa_diagnostics_suppressed(Some("true")));
        assert!(!alsa_diagnostics_suppressed(Some("1")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_installs_the_silent_alsa_error_handler() {
        install_silent_alsa_error_handler();
        assert!(ALSA_ERROR_HANDLER_INSTALLED.load(Ordering::SeqCst));
    }

    #[test]
    fn integer_samples_clip_scale_after_volume_and_underruns_stay_zero() {
        assert_eq!(encode_i16(0.0), 0);
        assert_eq!(encode_i16(1.0), 32_767);
        assert_eq!(encode_i16(-1.0), -32_767);
        assert_eq!(encode_i16(1.5), 32_767);
        assert_eq!(encode_i16(-1.5), -32_767);
        assert_eq!(encode_i16(0.5), 16_384);
        assert_eq!(encode_i16(-0.5), -16_384);
        assert_eq!(encode_i32(0.0), 0);
        assert_eq!(encode_i32(1.0), i32::MAX);
        assert_eq!(encode_i32(-1.0), i32::MIN);
        assert_eq!(encode_i32(2.0), i32::MAX);
        assert_eq!(encode_i32(-2.0), i32::MIN);
        assert!(encode_i32(0.5) > 1_000_000_000);
        assert!(encode_i32(-0.5) < -1_000_000_000);

        let (mut producer, mut renderer) = queue(48_000, 4, None);
        producer.push([1.0, -1.0]).unwrap();
        renderer.state.volume.store(50, Ordering::Release);
        let mut i16_samples = [9i16; 4];
        renderer.render_samples(&mut i16_samples, 2, Instant::now(), 0, encode_i16);
        assert_eq!(i16_samples, [encode_i16(0.5), encode_i16(-0.5), 0, 0]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 1);
        assert_eq!(renderer.state.underrun_frames.load(Ordering::Relaxed), 1);

        producer.push([2.0, -3.0]).unwrap();
        renderer.state.volume.store(100, Ordering::Release);
        renderer.render_samples(&mut i16_samples, 2, Instant::now(), 0, encode_i16);
        assert_eq!(i16_samples[0], 32_767);
        assert_eq!(i16_samples[1], -32_767);
        assert_eq!(i16_samples[2], 0);
        assert_eq!(i16_samples[3], 0);

        let (mut producer, mut renderer) = queue(48_000, 4, None);
        producer.push([1.0, 0.0]).unwrap();
        let mut mono = [9i16; 2];
        renderer.render_samples(&mut mono, 1, Instant::now(), 0, encode_i16);
        assert_eq!(mono[0], encode_i16(0.5));
        assert_eq!(mono[1], 0);
        assert_eq!(renderer.state.underrun_frames.load(Ordering::Relaxed), 1);

        let (mut producer, mut renderer) = queue(48_000, 2, None);
        producer.push([1.0, -1.0]).unwrap();
        renderer.state.volume.store(50, Ordering::Release);
        let mut i32_samples = [9i32; 4];
        renderer.render_samples(&mut i32_samples, 2, Instant::now(), 0, encode_i32);
        assert_eq!(i32_samples, [encode_i32(0.5), encode_i32(-0.5), 0, 0]);
        assert_eq!(renderer.state.underrun_frames.load(Ordering::Relaxed), 1);
        producer.push([4.0, -4.0]).unwrap();
        renderer.state.volume.store(100, Ordering::Release);
        renderer.render_samples(&mut i32_samples, 2, Instant::now(), 0, encode_i32);
        assert_eq!(i32_samples[0], i32::MAX);
        assert_eq!(i32_samples[1], i32::MIN);
    }

    #[test]
    fn silent_probe_pcm_is_bounded_zero_and_invalid_duration_never_opens_hardware() {
        let pcm = silent_pcm();
        validate_pcm(&pcm).unwrap();
        assert!(pcm.samples.iter().all(|sample| *sample == 0.0));
        assert_eq!(pcm.samples.len() * std::mem::size_of::<f32>(), 384_000);
        assert_eq!(pcm.summary.media_duration, Duration::from_secs(1));
        for seconds in [0, 1801, u32::MAX] {
            assert_eq!(
                probe_silent(seconds, StopToken::default(), |_| panic!("invalid probe"))
                    .unwrap_err(),
                OutputError::InvalidPcm
            );
        }
    }
    #[test]
    fn paused_queue_keeps_position_and_volume_applies_to_already_queued_audio() {
        let (mut producer, mut renderer) = queue(48_000, 10, None);
        producer.push([0.8, -0.4]).unwrap();
        renderer.state.paused.store(true, Ordering::Release);
        let mut data = [1.0; 2];
        for _ in 0..3 {
            renderer.render(&mut data, 2, Instant::now());
        }
        assert_eq!(data, [0.0; 2]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 0);
        assert_eq!(renderer.state.underrun_frames.load(Ordering::Acquire), 0);
        assert_eq!(renderer.queue.slots(), 1);
        renderer.state.volume.store(50, Ordering::Release);
        renderer.state.paused.store(false, Ordering::Release);
        renderer.render(&mut data, 2, Instant::now());
        assert_eq!(data, [0.4, -0.2]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 1);
        producer.push([0.8, 0.8]).unwrap();
        renderer.state.volume.store(0, Ordering::Release);
        renderer.render(&mut data, 2, Instant::now());
        assert_eq!(data, [0.0; 2]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 2);
    }
    #[test]
    fn live_deadline_expires_even_while_paused() {
        let now = Instant::now();
        let (mut producer, mut renderer) = queue(48_000, 10, Some(now));
        producer.push([0.5, 0.5]).unwrap();
        renderer.state.paused.store(true, Ordering::Release);
        renderer.render(&mut [1.0; 2], 2, now);
        assert!(renderer.state.expired.load(Ordering::Acquire));
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 0);
    }
    #[test]
    fn prebuffered_audio_stays_silent_until_enabled_and_old_generation_cannot_cancel_new() {
        let parent = StopToken::default();
        let old = parent.child();
        let new = parent.child();
        let (mut producer, mut renderer) = queue(48_000, 10, None);
        renderer.cancel = new.clone();
        renderer.state.enabled.store(false, Ordering::Release);
        producer.push([0.5, 0.5]).unwrap();
        let mut samples = [1.0; 2];
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.0; 2]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 0);
        old.cancel();
        assert!(!parent.is_cancelled());
        assert!(!new.is_cancelled());
        renderer.state.enabled.store(true, Ordering::Release);
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.5, 0.5]);
        parent.cancel();
        assert!(new.is_cancelled());
        producer.push([1.0, 1.0]).unwrap();
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.0; 2]);
    }
    #[test]
    fn shared_cancellation_mutes_queue_and_end_limit_can_shrink_after_eof() {
        let (mut producer, mut renderer) = queue(48_000, 100, None);
        let stop = renderer.cancel.clone();
        producer.push([0.5, 0.5]).unwrap();
        stop.cancel();
        let mut samples = [1.0; 4];
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.0; 4]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 0);
        let (mut producer, mut renderer) = queue(48_000, 100, None);
        producer.push([0.5, 0.5]).unwrap();
        producer.push([0.5, 0.5]).unwrap();
        renderer.state.total.store(1, Ordering::Release);
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.5, 0.5, 0.0, 0.0]);
        assert!(renderer.state.ended.load(Ordering::Acquire));
    }
    #[test]
    fn callback_crossing_live_end_does_not_submit_frames_after_deadline() {
        let now = Instant::now();
        let (mut producer, mut renderer) =
            queue(48_000, 100, Some(now + Duration::from_micros(50)));
        for _ in 0..4 {
            producer.push([0.5, 0.5]).unwrap();
        }
        let mut samples = [1.0; 8];
        renderer.render(&mut samples, 2, now);
        assert_eq!(samples, [0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
        assert!(renderer.state.expired.load(Ordering::Acquire));
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 2);
    }
    #[test]
    fn underrun_is_silence_and_does_not_advance_media_position_or_split_stereo_frames() {
        let (mut producer, mut renderer) = queue(48_000, 3, None);
        producer.push([0.2, -0.4]).unwrap();
        let mut samples = [1.0; 6];
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.2, -0.4, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(renderer.state.frames.load(Ordering::Relaxed), 1);
        assert_eq!(renderer.state.underrun_frames.load(Ordering::Relaxed), 2);
        assert!(!renderer.state.ended.load(Ordering::Relaxed));
        assert_eq!(renderer.state.callbacks.load(Ordering::Acquire), 1);
        renderer.render(&mut samples[..2], 2, Instant::now());
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 1);
        assert_eq!(renderer.state.callbacks.load(Ordering::Acquire), 2);
        producer.push([0.6, 0.2]).unwrap();
        renderer.render(&mut samples[..1], 1, Instant::now());
        assert!((samples[0] - 0.4).abs() < 0.00001);
        assert_eq!(renderer.state.frames.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn bounded_queue_rejects_excess_frames_and_sample_end_leaves_only_silence() {
        let (mut producer, mut renderer) = queue(2, 2, None);
        for _ in 0..4 {
            producer.push([0.5, 0.5]).unwrap();
        }
        assert!(producer.push([1.0, 1.0]).is_err());
        let mut samples = [1.0; 8];
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
        assert!(renderer.state.ended.load(Ordering::Relaxed));
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.0; 8]);
        assert_eq!(renderer.state.frames.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn stop_device_failure_and_live_deadline_mute_queued_audio() {
        let now = Instant::now();
        for reason in 0..3 {
            let (mut producer, mut renderer) = queue(48_000, 10, (reason == 2).then_some(now));
            producer.push([1.0, 1.0]).unwrap();
            if reason == 0 {
                renderer.state.stopped.store(true, Ordering::Release);
            }
            if reason == 1 {
                renderer.state.failed.store(true, Ordering::Release);
            }
            let mut samples = [1.0; 4];
            renderer.render(&mut samples, 2, now);
            assert_eq!(samples, [0.0; 4]);
            assert_eq!(renderer.state.frames.load(Ordering::Relaxed), 0);
            assert_eq!(renderer.state.expired.load(Ordering::Relaxed), reason == 2);
            drop(renderer);
            assert!(producer.is_abandoned());
        }
    }
    #[test]
    fn invalid_pcm_and_expired_slot_are_rejected_without_accessing_hardware() {
        let summary = super::super::aac::AacSummary {
            core_sample_rate: 48_000,
            core_channels: 2,
            sample_rate: 48_000,
            channels: 2,
            packets: 1,
            frames: 1,
            sbr_detected: false,
            stereo_expansion: false,
            explicit_ps: false,
            media_duration: Duration::ZERO,
            decode_duration: Duration::ZERO,
            nonzero_samples: 0,
        };
        for samples in [vec![], vec![f32::NAN, 0.0], vec![0.0]] {
            assert_eq!(
                play_pcm(&DecodedPcm { summary, samples }, None).unwrap_err(),
                OutputError::InvalidPcm
            );
        }
        assert_eq!(
            play_pcm(
                &DecodedPcm {
                    summary,
                    samples: vec![0.0, 0.0]
                },
                Some(Duration::ZERO)
            )
            .unwrap_err(),
            OutputError::Deadline
        );
        assert_eq!(probe_tone(0).unwrap_err(), OutputError::InvalidPcm);
        assert_eq!(probe_tone(11).unwrap_err(), OutputError::InvalidPcm);
    }
    fn started(at: Instant) -> OutputStallWatch {
        let mut watch = OutputStallWatch::new();
        watch.start(at);
        watch.poll(at, 1);
        watch
    }
    #[test]
    fn a_long_poll_gap_is_not_a_stall_and_resumes_when_callbacks_move() {
        let start = Instant::now();
        let mut watch = started(start);
        assert_eq!(
            watch.poll(start + Duration::from_secs(70), 1),
            OutputWatch::Suspended
        );
        assert_eq!(watch.reconnecting_for(), Some(Duration::ZERO));
        assert_eq!(
            watch.poll(
                start + Duration::from_secs(70) + Duration::from_millis(100),
                2
            ),
            OutputWatch::Continue
        );
        assert_eq!(watch.reconnecting_for(), None);
        assert_ne!(
            watch.poll(start + Duration::from_secs(74), 2),
            OutputWatch::Stalled
        );
    }
    #[test]
    fn a_poll_gap_then_a_frozen_callback_rebuilds_and_stops_at_the_limit() {
        let start = Instant::now();
        let mut watch = started(start);
        let mut now = start + Duration::from_secs(10);
        assert_eq!(watch.poll(now, 4), OutputWatch::Suspended);
        let mut rebuilt = false;
        loop {
            now += Duration::from_millis(500);
            match watch.poll(now, 4) {
                OutputWatch::Rebuild => rebuilt = true,
                OutputWatch::Expired => break,
                OutputWatch::Continue | OutputWatch::Suspended => {}
                OutputWatch::Stalled => panic!("a suspension must not become a callback stall"),
            }
            assert!(
                now.saturating_duration_since(start) < Duration::from_secs(400),
                "reconnect must end at the limit"
            );
        }
        assert!(rebuilt);
        assert!(watch.elapsed > crate::reconnect::LIMIT);
        assert!(watch.elapsed < crate::reconnect::LIMIT + Duration::from_secs(2));
    }
    #[test]
    fn frozen_callbacks_with_regular_polls_are_still_stalled() {
        let start = Instant::now();
        let mut watch = started(start);
        let mut now = start;
        let mut stalled = false;
        for _ in 0..40 {
            now += Duration::from_millis(100);
            if watch.poll(now, 4) == OutputWatch::Stalled {
                stalled = true;
                break;
            }
        }
        assert!(stalled);
        assert!(now.saturating_duration_since(start) <= Duration::from_secs(4));
        assert!(!watch.rebuilt);
        assert_eq!(watch.reconnecting_for(), None);
    }
}
