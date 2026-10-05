//! Host-independent glitch engine. Audio is continuously recorded into a history ring buffer, and
//! on every grid step a dice roll decides whether a random glitch event replaces the dry signal.

use std::sync::{Arc, Mutex};

const HISTORY_SECONDS: f32 = 12.0;
const FADE_SECONDS: f32 = 0.003;
const GATE_SMOOTH_SECONDS: f32 = 0.001;
const MIN_SLICE_SAMPLES: f64 = 64.0;
/// Resonance of the low-pass. High enough that each cutoff snap rings.
const REBOUND_Q: f32 = 7.0;
/// Cutoff used when a step slams shut.
const REBOUND_SHUT_HZ: f32 = 40.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Stutter,
    Reverse,
    TapeStop,
    Scramble,
    Crush,
    Gate,
    Rebound,
}

pub const MODES: [Mode; 7] = [
    Mode::Stutter,
    Mode::Reverse,
    Mode::TapeStop,
    Mode::Scramble,
    Mode::Crush,
    Mode::Gate,
    Mode::Rebound,
];

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Probability in `[0, 1]` that a glitch starts on a free grid step.
    pub chance: f32,
    /// Grid step length in quarter notes.
    pub step_beats: f64,
    /// Maximum glitch length in grid steps.
    pub max_steps: u32,
    /// Relative weights for each entry in [`MODES`].
    pub weights: [f32; MODES.len()],
    pub seed: u32,
    /// Derive the randomness from the song position so playback repeats identically.
    pub deterministic: bool,
    /// Probability in `[0, 1]` that crush, gate, or rebound cuts in and out, in rhythm, over a
    /// history effect.
    pub stack: f32,
    /// Probability in `[0, 1]` that a glitch replays the last glitch exactly.
    pub haunt: f32,
}

/// Fraction of the history a single event may span. The ghost recording is sized from this.
const EVENT_BUDGET: f64 = 0.45;

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Stutter => "stutter",
            Mode::Reverse => "reverse",
            Mode::TapeStop => "tape stop",
            Mode::Scramble => "scramble",
            Mode::Crush => "crush",
            Mode::Gate => "gate",
            Mode::Rebound => "rebound",
        }
    }
}

/// Latest glitch, shared with the plugin window. The audio thread only updates it when an event
/// starts, and skips the update if the window is reading it.
#[derive(Clone)]
pub struct Transcript {
    line: Arc<Mutex<String>>,
}

impl Transcript {
    fn new() -> Self {
        Self {
            line: Arc::new(Mutex::new(String::new())),
        }
    }

    pub fn text(&self) -> String {
        self.line
            .lock()
            .map(|line| line.clone())
            .unwrap_or_default()
    }

    fn set(&self, text: String) {
        if let Ok(mut line) = self.line.try_lock() {
            *line = text;
        }
    }
}

/// SplitMix64. Tiny, allocation free, and good enough for musical dice rolls.
#[derive(Clone, Copy, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: u32) -> u32 {
        (((self.next_u64() >> 32) * n as u64) >> 32) as u32
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u32) as usize]
    }
}

struct History {
    channels: [Vec<f32>; 2],
    mask: u64,
    /// Absolute index of the next sample to be written.
    write: u64,
}

impl History {
    fn empty() -> Self {
        Self {
            channels: [Vec::new(), Vec::new()],
            mask: 0,
            write: 0,
        }
    }

    fn allocate(&mut self, len: usize) {
        let len = len.next_power_of_two();
        self.channels = [vec![0.0; len], vec![0.0; len]];
        self.mask = len as u64 - 1;
        self.write = 0;
    }

    fn len(&self) -> usize {
        self.channels[0].len()
    }

    fn clear(&mut self) {
        for channel in &mut self.channels {
            channel.fill(0.0);
        }
        self.write = 0;
    }

    fn push(&mut self, frame: [f32; 2]) {
        let i = (self.write & self.mask) as usize;
        self.channels[0][i] = frame[0];
        self.channels[1][i] = frame[1];
        self.write += 1;
    }

    /// Read at an absolute (possibly fractional or negative) position. Whole-sample positions are
    /// exact; fractional ones use 4-point Hermite, which keeps far more treble than a linear blend.
    fn read(&self, pos: f64) -> [f32; 2] {
        let floor = pos.floor();
        let frac = (pos - floor) as f32;
        let base = floor as i64;
        let at = |offset: i64| ((base + offset) as u64 & self.mask) as usize;
        let [l, r] = &self.channels;
        if frac == 0.0 {
            let i = at(0);
            return [l[i], r[i]];
        }
        let (a, b, c, d) = (at(-1), at(0), at(1), at(2));
        [
            hermite(l[a], l[b], l[c], l[d], frac),
            hermite(r[a], r[b], r[c], r[d], frac),
        ]
    }
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Stutter {
        /// Distance behind `origin` where the captured slice begins.
        offset: f64,
        speed: f64,
        slice_len: f64,
        shrink: bool,
        repeat_start: u64,
    },
    Reverse,
    TapeStop {
        pos: f64,
    },
    Scramble {
        offset: f64,
    },
    Crush {
        levels: f32,
        hold: u32,
        hold_left: u32,
        held: [f32; 2],
    },
    Gate {
        period: f64,
        gain: f32,
    },
    /// Resonant low-pass. Cutoff is held, then snapped, along an open–shut–open curve.
    Rebound {
        floor_hz: f32,
        /// Cutoff holds in this event. Each one is half a grid step.
        divisions: u32,
        /// Bit `i` set means hold `i` slams the filter shut.
        drop_mask: u32,
        ic1eq: [f32; 2],
        ic2eq: [f32; 2],
        level: Leveler,
    },
    /// Haunt: replays the last glitch from the ghost recording.
    Ghost,
}

/// Keeps a resonant ring within a few dB of the signal going in, so snaps pop without clipping.
#[derive(Clone, Copy, Debug)]
struct Leveler {
    env_in: f32,
    env_out: f32,
    gain: f32,
}

/// How far above the input level a ring may peak (about +5 dB).
const REBOUND_HEADROOM: f32 = 1.78;

impl Leveler {
    fn new() -> Self {
        Self {
            env_in: 0.0,
            env_out: 0.0,
            gain: 1.0,
        }
    }

    fn apply(&mut self, input: [f32; 2], out: [f32; 2], sample_rate: f32) -> [f32; 2] {
        let release = 1.0 - 1.0 / (sample_rate * 0.04).max(1.0);
        let level_in = input[0].abs().max(input[1].abs());
        let level_out = out[0].abs().max(out[1].abs());
        self.env_in = level_in.max(self.env_in * release);
        self.env_out = level_out.max(self.env_out * release);
        let target = if self.env_out > 1.0e-6 {
            (self.env_in * REBOUND_HEADROOM / self.env_out).min(1.0)
        } else {
            1.0
        };
        // Peaks are held, so the gain only drops on a new peak and then recovers smoothly.
        if target < self.gain {
            self.gain = target;
        } else {
            self.gain += (target - self.gain) * (1.0 - release);
        }
        out.map(|x| x * self.gain)
    }
}

/// A stacked color that cuts in and out on half-step slots, so it lands as a second glitch.
#[derive(Clone, Copy, Debug)]
struct Color {
    kind: Kind,
    mode: Mode,
    /// Slots in this event. Each one is half a grid step.
    slots: u32,
    /// Bit `i` set means the color plays in slot `i`.
    mask: u32,
    gain: f32,
}

/// The output of the most recent glitch, kept so Haunt can replay it.
struct Ghost {
    channels: [Vec<f32>; 2],
    len: usize,
    steps: u32,
    mode: Mode,
    color: Option<Mode>,
    /// Each glitch comes back once, so a high Haunt alternates new glitches and their echoes.
    spent: bool,
}

impl Ghost {
    fn empty() -> Self {
        Self {
            channels: [Vec::new(), Vec::new()],
            len: 0,
            steps: 1,
            mode: Mode::Stutter,
            color: None,
            spent: false,
        }
    }

    fn allocate(&mut self, len: usize) {
        self.channels = [vec![0.0; len], vec![0.0; len]];
        self.len = 0;
    }

    fn capacity(&self) -> usize {
        self.channels[0].len()
    }
}

#[derive(Clone, Copy, Debug)]
struct Event {
    kind: Kind,
    /// Absolute history index of the sample at which the event started.
    origin: f64,
    elapsed: u64,
    expected_len: f64,
    steps_left: u32,
    steps: u32,
    env: f32,
    color: Option<Color>,
}

impl Event {
    fn render(
        &mut self,
        history: &History,
        ghost: &Ghost,
        input: [f32; 2],
        fade_len: f64,
        gate_coef: f32,
        sample_rate: f32,
    ) -> [f32; 2] {
        let elapsed = self.elapsed;
        let expected_len = self.expected_len;
        let origin = self.origin;
        self.elapsed += 1;
        if let Kind::Ghost = self.kind {
            return replay(ghost, elapsed as usize, input, fade_len);
        }
        let carrier = voice(
            &mut self.kind,
            elapsed,
            expected_len,
            origin,
            history,
            input,
            fade_len,
            gate_coef,
            sample_rate,
        );
        let Some(color) = &mut self.color else {
            return carrier;
        };
        // The color runs the whole time so its filter and hold state stay continuous, and only
        // its level is switched.
        let colored = voice(
            &mut color.kind,
            elapsed,
            expected_len,
            origin,
            history,
            carrier,
            fade_len,
            gate_coef,
            sample_rate,
        );
        let progress = (elapsed as f64 / expected_len).clamp(0.0, 1.0);
        let slot = ((progress * color.slots as f64) as u32).min(color.slots - 1);
        let target = if (color.mask & (1u32 << slot)) != 0 {
            1.0
        } else {
            0.0
        };
        color.gain += (target - color.gain) * gate_coef;
        [
            carrier[0] + (colored[0] - carrier[0]) * color.gain,
            carrier[1] + (colored[1] - carrier[1]) * color.gain,
        ]
    }
}

/// Plays the recorded glitch back. Past its end, it fades to the dry input.
fn replay(ghost: &Ghost, at: usize, input: [f32; 2], fade_len: f64) -> [f32; 2] {
    if at >= ghost.len {
        return input;
    }
    let left = (ghost.len - at) as f64;
    let blend = (left / fade_len).min(1.0) as f32;
    let frame = [ghost.channels[0][at], ghost.channels[1][at]];
    [
        input[0] + (frame[0] - input[0]) * blend,
        input[1] + (frame[1] - input[1]) * blend,
    ]
}

fn voice(
    kind: &mut Kind,
    elapsed: u64,
    expected_len: f64,
    origin: f64,
    history: &History,
    input: [f32; 2],
    fade_len: f64,
    gate_coef: f32,
    sample_rate: f32,
) -> [f32; 2] {
    let t = elapsed as f64;
    match kind {
        Kind::Stutter {
            offset,
            speed,
            slice_len,
            shrink,
            repeat_start,
        } => {
            let mut p = (elapsed - *repeat_start) as f64;
            if p >= *slice_len {
                *repeat_start = elapsed;
                p = 0.0;
                if *shrink {
                    *slice_len = (*slice_len * 0.75).max(MIN_SLICE_SAMPLES * 4.0);
                }
            }
            let window = (p / fade_len).min((*slice_len - p) / fade_len).min(1.0) as f32;
            let [l, r] = history.read(origin - *offset + p * *speed);
            [l * window, r * window]
        }
        Kind::Reverse => history.read(origin - 1.0 - t),
        Kind::TapeStop { pos } => {
            let out = history.read(origin + *pos);
            let remaining = (1.0 - t / expected_len).max(0.0);
            *pos += remaining * remaining.sqrt();
            out
        }
        Kind::Scramble { offset } => history.read(origin - *offset + t),
        Kind::Crush {
            levels,
            hold,
            hold_left,
            held,
        } => {
            if *hold_left == 0 {
                *held = input.map(|x| (x * *levels).round() / *levels);
                *hold_left = *hold;
            }
            *hold_left -= 1;
            *held
        }
        Kind::Gate { period, gain } => {
            let target = if t % *period < *period * 0.5 {
                1.0
            } else {
                0.0
            };
            *gain += (target - *gain) * gate_coef;
            input.map(|x| x * *gain)
        }
        Kind::Rebound {
            floor_hz,
            divisions,
            drop_mask,
            ic1eq,
            ic2eq,
            level,
        } => {
            let progress = (t / expected_len).clamp(0.0, 1.0) as f32;
            let n = *divisions as f32;
            let index = ((progress * n).floor() as u32).min(*divisions - 1);
            let shut = (*drop_mask & (1u32 << index)) != 0;
            // Sample the open–shut–open curve once per hold, so the cutoff jumps instead of gliding.
            let bounce = if shut {
                1.0
            } else {
                let center = (index as f32 + 0.5) / n;
                (center * std::f32::consts::PI).sin()
            };
            let open_hz = sample_rate * 0.42;
            let floor = if shut { REBOUND_SHUT_HZ } else { *floor_hz };
            let cutoff = open_hz * (floor / open_hz).powf(bounce);
            let mut out = [0.0; 2];
            for ch in 0..2 {
                out[ch] = svf_low(
                    input[ch],
                    cutoff,
                    REBOUND_Q,
                    sample_rate,
                    &mut ic1eq[ch],
                    &mut ic2eq[ch],
                );
            }
            level.apply(input, out, sample_rate)
        }
        Kind::Ghost => input,
    }
}

pub struct Engine {
    sample_rate: f32,
    history: History,
    active: Option<Event>,
    retiring: Option<Event>,
    fade_len: f64,
    gate_coef: f32,
    last_step: Option<i64>,
    free_beats: f64,
    rng: Rng,
    transcript: Transcript,
    transcript_dirty: bool,
    /// The last finished glitch, which Haunt replays.
    ghost: Ghost,
    /// The glitch being recorded now. Swapped into `ghost` when it ends.
    recording: Ghost,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            sample_rate: 44_100.0,
            history: History::empty(),
            active: None,
            retiring: None,
            fade_len: 1.0,
            gate_coef: 1.0,
            last_step: None,
            free_beats: 0.0,
            rng: Rng::new(0x5EED),
            transcript: Transcript::new(),
            transcript_dirty: false,
            ghost: Ghost::empty(),
            recording: Ghost::empty(),
        }
    }
}

impl Engine {
    /// Allocates the history and ghost buffers. Must be called before processing, never from the
    /// audio thread.
    pub fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.history
            .allocate((sample_rate * HISTORY_SECONDS) as usize);
        let ghost_len = (self.history.len() as f64 * EVENT_BUDGET).ceil() as usize + 1;
        self.ghost.allocate(ghost_len);
        self.recording.allocate(ghost_len);
        self.fade_len = (sample_rate * FADE_SECONDS).max(1.0) as f64;
        self.gate_coef = 1.0 - (-1.0 / (sample_rate * GATE_SMOOTH_SECONDS)).exp();
        self.reset();
    }

    pub fn reset(&mut self) {
        self.history.clear();
        self.active = None;
        self.retiring = None;
        self.last_step = None;
        self.free_beats = 0.0;
        self.transcript.set(String::new());
        self.transcript_dirty = true;
        self.ghost.len = 0;
        self.recording.len = 0;
    }

    pub fn share_transcript(&self) -> Transcript {
        self.transcript.clone()
    }

    /// True once after the transcript changes. The plugin uses this to ask the window to repaint.
    pub fn take_transcript_dirty(&mut self) -> bool {
        std::mem::take(&mut self.transcript_dirty)
    }

    /// Processes one stereo frame and returns the fully wet output. `song_beats` is the transport
    /// position in quarter notes while the host is playing, or `None` to run on an internal clock.
    /// `meter` is the time signature `(numerator, denominator)`.
    pub fn process(
        &mut self,
        input: [f32; 2],
        song_beats: Option<f64>,
        tempo: f64,
        meter: (i32, i32),
        settings: &Settings,
    ) -> [f32; 2] {
        self.history.push(input);
        let now = self.history.write as f64 - 1.0;

        let synced = song_beats.is_some();
        let beats = song_beats.unwrap_or(self.free_beats);
        self.free_beats = beats + tempo / 60.0 / self.sample_rate as f64;

        let step = (beats / settings.step_beats).floor() as i64;
        if self.last_step != Some(step) {
            let contiguous = self.last_step == Some(step - 1);
            self.last_step = Some(step);
            self.on_step(step, contiguous, synced, now, tempo, beats, meter, settings);
        }

        let fade = (1.0 / self.fade_len) as f32;
        let mut wet = [0.0f32; 2];
        let mut env_sum = 0.0;

        if let Some(ev) = &mut self.active {
            ev.env = (ev.env + fade).min(1.0);
            let at = ev.elapsed as usize;
            let out = ev.render(
                &self.history,
                &self.ghost,
                input,
                self.fade_len,
                self.gate_coef,
                self.sample_rate,
            );
            wet = [wet[0] + out[0] * ev.env, wet[1] + out[1] * ev.env];
            env_sum += ev.env;
            // Replays are not recorded, so the ghost stays the last glitch that actually happened.
            if !matches!(ev.kind, Kind::Ghost) && at < self.recording.capacity() {
                self.recording.channels[0][at] = out[0];
                self.recording.channels[1][at] = out[1];
                self.recording.len = at + 1;
            }
        }
        if let Some(ev) = &mut self.retiring {
            ev.env -= fade;
            if ev.env <= 0.0 {
                self.retiring = None;
            } else {
                let out = ev.render(
                    &self.history,
                    &self.ghost,
                    input,
                    self.fade_len,
                    self.gate_coef,
                    self.sample_rate,
                );
                wet = [wet[0] + out[0] * ev.env, wet[1] + out[1] * ev.env];
                env_sum += ev.env;
            }
        }

        let dry = (1.0 - env_sum).max(0.0);
        [wet[0] + input[0] * dry, wet[1] + input[1] * dry]
    }

    #[cfg(test)]
    fn active_mode(&self) -> Option<Mode> {
        self.active.and_then(|ev| mode_of(&ev.kind))
    }

    fn on_step(
        &mut self,
        step: i64,
        contiguous: bool,
        synced: bool,
        now: f64,
        tempo: f64,
        beats: f64,
        meter: (i32, i32),
        s: &Settings,
    ) {
        if let Some(ev) = &mut self.active {
            if contiguous && ev.steps_left > 1 {
                ev.steps_left -= 1;
                return;
            }
            if !matches!(ev.kind, Kind::Ghost) && self.recording.len > 0 {
                std::mem::swap(&mut self.ghost, &mut self.recording);
                self.ghost.spent = false;
            }
            self.retiring = self.active.take();
        }

        let deterministic = s.deterministic && synced;
        let mut rng = if deterministic {
            Rng::new(((s.seed as u64) << 32) ^ (step as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93))
        } else {
            self.rng
        };
        // Throw away the first output so that nearby seeds/steps don't produce correlated rolls.
        rng.next_u64();
        let ghost =
            (self.ghost.len > 0 && !self.ghost.spent).then_some((self.ghost.len, self.ghost.steps));
        self.active = Self::roll(
            &mut rng,
            now,
            tempo,
            self.sample_rate,
            self.history.len(),
            ghost,
            s,
        );
        if let Some(ev) = &self.active {
            let (bar, beat) = song_place(beats, meter.0, meter.1);
            let (mode, color, haunted) = match mode_of(&ev.kind) {
                Some(mode) => {
                    let color = ev.color.as_ref().map(|color| color.mode);
                    self.recording.len = 0;
                    self.recording.steps = ev.steps;
                    self.recording.mode = mode;
                    self.recording.color = color;
                    (mode, color, false)
                }
                None => {
                    self.ghost.spent = true;
                    (self.ghost.mode, self.ghost.color, true)
                }
            };
            let line = describe(bar, beat, ev.steps, mode, color, haunted);
            self.transcript.set(line);
            self.transcript_dirty = true;
        }
        if !deterministic {
            self.rng = rng;
        }
    }

    fn roll(
        rng: &mut Rng,
        now: f64,
        tempo: f64,
        sample_rate: f32,
        history_len: usize,
        ghost: Option<(usize, u32)>,
        s: &Settings,
    ) -> Option<Event> {
        if rng.next_f32() >= s.chance {
            return None;
        }
        let mode = Self::pick_mode(rng, &s.weights)?;

        let step_len =
            (s.step_beats * 60.0 / tempo.max(1.0) * sample_rate as f64).max(MIN_SLICE_SAMPLES);
        // Bias towards short events, and leave headroom in the history for reverse/scramble reads.
        let budget = (history_len as f64 * EVENT_BUDGET / step_len)
            .floor()
            .max(1.0) as u32;
        let u = rng.next_f32();
        let steps = (1 + (u * u * s.max_steps.max(1) as f32) as u32)
            .min(s.max_steps.max(1))
            .min(budget);
        let expected_len = steps as f64 * step_len;

        let kind = Self::kind_for(mode, rng, step_len, expected_len, history_len, steps);
        // A history effect can also be crushed, gated, or filtered. Color-only hits are left alone,
        // and a stack of 0 skips the roll so existing patterns stay put.
        let color = if matches!(
            mode,
            Mode::Stutter | Mode::Reverse | Mode::TapeStop | Mode::Scramble
        ) && s.stack > 0.0
            && rng.next_f32() < s.stack
        {
            Self::pick_color(rng, &s.weights).map(|color_mode| {
                let kind =
                    Self::kind_for(color_mode, rng, step_len, expected_len, history_len, steps);
                let slots = (steps * 2).clamp(2, 32);
                let all = if slots == 32 {
                    u32::MAX
                } else {
                    (1u32 << slots) - 1
                };
                // Alternating slots land on or off the beat; a random mask stutters the color.
                let mut mask = match rng.below(3) {
                    0 => 0xAAAA_AAAA,
                    1 => 0x5555_5555,
                    _ => rng.next_u64() as u32,
                } & all;
                if mask == 0 || mask == all {
                    mask = 0xAAAA_AAAA & all;
                }
                Color {
                    kind,
                    mode: color_mode,
                    slots,
                    mask,
                    gain: 0.0,
                }
            })
        } else {
            None
        };
        // Rolled after the kind is built, and skipped at 0, so an existing pattern stays put.
        if s.haunt > 0.0
            && rng.next_f32() < s.haunt
            && let Some((len, ghost_steps)) = ghost
        {
            return Some(Event {
                kind: Kind::Ghost,
                origin: now,
                elapsed: 0,
                expected_len: len as f64,
                steps_left: ghost_steps,
                steps: ghost_steps,
                env: 0.0,
                color: None,
            });
        }

        Some(Event {
            kind,
            origin: now,
            elapsed: 0,
            expected_len,
            steps_left: steps,
            steps,
            env: 0.0,
            color,
        })
    }

    fn kind_for(
        mode: Mode,
        rng: &mut Rng,
        step_len: f64,
        expected_len: f64,
        history_len: usize,
        steps: u32,
    ) -> Kind {
        match mode {
            Mode::Stutter => {
                let slice_len = (step_len / rng.pick(&[1.0, 2.0, 2.0, 3.0, 4.0, 4.0, 6.0, 8.0]))
                    .max(MIN_SLICE_SAMPLES);
                let speed = match rng.below(10) {
                    0 => 0.5,
                    1 => 2.0,
                    _ => 1.0,
                };
                // Faster-than-realtime repeats would read audio that hasn't been recorded yet, so
                // those capture the slice that just went by instead.
                let offset = if speed > 1.0 {
                    (slice_len * speed).round()
                } else {
                    0.0
                };
                Kind::Stutter {
                    offset,
                    speed,
                    slice_len,
                    shrink: rng.below(4) == 0,
                    repeat_start: 0,
                }
            }
            Mode::Reverse => Kind::Reverse,
            Mode::TapeStop => Kind::TapeStop { pos: 0.0 },
            Mode::Scramble => {
                let max_back = ((history_len as f64 * 0.9 - expected_len) / step_len)
                    .floor()
                    .max(1.0) as u32;
                let back = 1 + rng.below(8.min(max_back));
                Kind::Scramble {
                    offset: (back as f64 * step_len).round(),
                }
            }
            Mode::Crush => Kind::Crush {
                levels: (1u32 << (1 + rng.below(7))) as f32,
                hold: rng.pick(&[1, 2, 4, 6, 8, 12, 16, 32]),
                hold_left: 0,
                held: [0.0; 2],
            },
            Mode::Gate => Kind::Gate {
                period: step_len / rng.pick(&[1.0, 2.0, 3.0, 4.0]),
                gain: 1.0,
            },
            Mode::Rebound => {
                // Two holds per grid step, so a one-step glitch snaps twice and a long one gets a run.
                let divisions = (steps * 2).clamp(1, 32);
                let mut drop_mask = 0u32;
                // A few holds in the middle slam shut. The first and last stay on the curve so the
                // event still opens and closes on the dry tone.
                let room = divisions.saturating_sub(2);
                let drops = if room == 0 {
                    0
                } else {
                    (1 + rng.below((divisions / 8).max(1))).min(room)
                };
                for _ in 0..drops {
                    let step = 1 + rng.below(room);
                    drop_mask |= 1u32 << step;
                }
                Kind::Rebound {
                    floor_hz: 140.0 * 2.0f32.powf(rng.next_f32() * 2.3),
                    divisions,
                    drop_mask,
                    ic1eq: [0.0; 2],
                    ic2eq: [0.0; 2],
                    level: Leveler::new(),
                }
            }
        }
    }

    fn pick_color(rng: &mut Rng, weights: &[f32; MODES.len()]) -> Option<Mode> {
        const COLORS: [Mode; 3] = [Mode::Crush, Mode::Gate, Mode::Rebound];
        let color_weights = COLORS.map(|mode| {
            MODES
                .iter()
                .zip(weights)
                .find(|(candidate, _)| **candidate == mode)
                .map(|(_, weight)| weight.max(0.0))
                .unwrap_or(0.0)
        });
        let total: f32 = color_weights.iter().sum();
        if total <= 0.0 {
            return None;
        }
        let mut target = rng.next_f32() * total;
        for (mode, weight) in COLORS.iter().zip(color_weights) {
            target -= weight;
            if target < 0.0 {
                return Some(*mode);
            }
        }
        Some(COLORS[COLORS.len() - 1])
    }

    fn pick_mode(rng: &mut Rng, weights: &[f32; MODES.len()]) -> Option<Mode> {
        let total: f32 = weights.iter().map(|w| w.max(0.0)).sum();
        if total <= 0.0 {
            return None;
        }
        let mut target = rng.next_f32() * total;
        for (mode, w) in MODES.iter().zip(weights) {
            target -= w.max(0.0);
            if target < 0.0 {
                return Some(*mode);
            }
        }
        Some(MODES[MODES.len() - 1])
    }
}

/// The mode a kind was rolled from. A ghost replay has none of its own.
fn mode_of(kind: &Kind) -> Option<Mode> {
    Some(match kind {
        Kind::Stutter { .. } => Mode::Stutter,
        Kind::Reverse => Mode::Reverse,
        Kind::TapeStop { .. } => Mode::TapeStop,
        Kind::Scramble { .. } => Mode::Scramble,
        Kind::Crush { .. } => Mode::Crush,
        Kind::Gate { .. } => Mode::Gate,
        Kind::Rebound { .. } => Mode::Rebound,
        Kind::Ghost => return None,
    })
}

/// Bar numbers follow the host, starting at 1. Beat is the count within the bar.
fn song_place(beats: f64, numerator: i32, denominator: i32) -> (i32, i32) {
    let numerator = numerator.max(1) as f64;
    let denominator = denominator.max(1) as f64;
    let beats_per_bar = numerator / denominator * 4.0;
    let beat_len = 4.0 / denominator;
    let bar_index = (beats / beats_per_bar).floor();
    let into = beats - bar_index * beats_per_bar;
    let bar = bar_index as i32 + 1;
    let beat = (into / beat_len).floor() as i32 + 1;
    (bar, beat.max(1))
}

fn describe(
    bar: i32,
    beat: i32,
    steps: u32,
    mode: Mode,
    color: Option<Mode>,
    haunt: bool,
) -> String {
    let step_word = if steps == 1 { "step" } else { "steps" };
    let replay = if haunt { "haunt of " } else { "" };
    let mut line = format!(
        "bar {bar} · beat {beat} · {replay}{} · {steps} {step_word}",
        mode.label()
    );
    if let Some(color) = color {
        line.push_str(" · ");
        line.push_str(color.label());
    }
    line
}

/// Unity-gain low-pass of a Cytomic state-variable filter. `ic1eq` / `ic2eq` are the integrator
/// states and must persist across samples.
fn svf_low(
    input: f32,
    freq: f32,
    q: f32,
    sample_rate: f32,
    ic1eq: &mut f32,
    ic2eq: &mut f32,
) -> f32 {
    let freq = freq.clamp(40.0, sample_rate * 0.45);
    let g = (std::f32::consts::PI * freq / sample_rate).tan();
    let k = 1.0 / q.max(0.5);
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    let a3 = g * a2;

    let v3 = input - *ic2eq;
    let v1 = a1 * *ic1eq + a2 * v3;
    let v2 = *ic2eq + a2 * *ic1eq + a3 * v3;
    *ic1eq = flush_denormal(2.0 * v1 - *ic1eq);
    *ic2eq = flush_denormal(2.0 * v2 - *ic2eq);
    v2
}

fn hermite(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    let c1 = 0.5 * (c - a);
    let c2 = a - 2.5 * b + 2.0 * c - 0.5 * d;
    let c3 = 0.5 * (d - a) + 1.5 * (b - c);
    ((c3 * t + c2) * t + c1) * t + b
}

fn flush_denormal(x: f32) -> f32 {
    if x.abs() < 1.0e-15 { 0.0 } else { x }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn settings() -> Settings {
        Settings {
            chance: 1.0,
            step_beats: 0.25,
            max_steps: 4,
            weights: [1.0; MODES.len()],
            seed: 7,
            deterministic: true,
            stack: 0.0,
            haunt: 0.0,
        }
    }

    fn input(i: usize) -> [f32; 2] {
        let x = (i as f32 * 0.013).sin() * 0.8;
        [x, -x]
    }

    fn render(settings: &Settings, synced: bool, len: usize) -> Vec<[f32; 2]> {
        let mut engine = Engine::default();
        engine.prepare(SR);
        let tempo = 140.0;
        (0..len)
            .map(|i| {
                let beats = synced.then(|| i as f64 * tempo / 60.0 / SR as f64);
                engine.process(input(i), beats, tempo, (4, 4), settings)
            })
            .collect()
    }

    fn bright(i: usize) -> [f32; 2] {
        // Saw-like chord: three detuned notes, 12 harmonics each, peak about 0.8.
        let t = i as f32 / SR;
        let mut x = 0.0;
        for f in [220.0f32, 261.6, 329.6] {
            for h in 1..=12 {
                x += (std::f32::consts::TAU * f * h as f32 * t).sin() / h as f32;
            }
        }
        let x = x * 0.16;
        [x, x]
    }

    fn probe(s: &Settings) -> (f32, f32) {
        let mut engine = Engine::default();
        engine.prepare(SR);
        let mut peak_in = 0.0f32;
        let mut peak_out = 0.0f32;
        let mut hf_in = 0.0f32;
        let mut hf_out = 0.0f32;
        let mut prev_in = 0.0f32;
        let mut prev_out = 0.0f32;
        for i in 0..SR as usize * 8 {
            let x = bright(i);
            let beats = Some(i as f64 * 140.0 / 60.0 / SR as f64);
            let y = engine.process(x, beats, 140.0, (4, 4), s);
            peak_in = peak_in.max(x[0].abs());
            peak_out = peak_out.max(y[0].abs());
            hf_in += (x[0] - prev_in).powi(2);
            hf_out += (y[0] - prev_out).powi(2);
            prev_in = x[0];
            prev_out = y[0];
        }
        (peak_out / peak_in, (hf_out / hf_in).sqrt())
    }

    #[test]
    #[ignore]
    fn probe_levels() {
        let names = [
            "stutter", "reverse", "tape", "scramble", "crush", "gate", "rebound",
        ];
        for (i, name) in names.iter().enumerate() {
            let mut weights = [0.0; MODES.len()];
            weights[i] = 1.0;
            let (peak, hf) = probe(&Settings {
                weights,
                ..settings()
            });
            println!("{name:9} peak x{peak:.2} treble x{hf:.2}");
        }
        let combos = [
            (
                "stack",
                Settings {
                    stack: 1.0,
                    ..settings()
                },
            ),
            (
                "haunt",
                Settings {
                    haunt: 1.0,
                    ..settings()
                },
            ),
            (
                "stk+hnt",
                Settings {
                    stack: 1.0,
                    haunt: 1.0,
                    ..settings()
                },
            ),
            (
                "1/32x16",
                Settings {
                    step_beats: 0.125,
                    max_steps: 16,
                    stack: 1.0,
                    haunt: 1.0,
                    ..settings()
                },
            ),
            (
                "1/4x16",
                Settings {
                    step_beats: 1.0,
                    max_steps: 16,
                    stack: 1.0,
                    haunt: 1.0,
                    ..settings()
                },
            ),
            (
                "all-max",
                Settings {
                    stack: 1.0,
                    haunt: 1.0,
                    max_steps: 16,
                    ..settings()
                },
            ),
        ];
        for (name, s) in combos {
            let (peak, hf) = probe(&s);
            println!("{name:9} peak x{peak:.2} treble x{hf:.2}");
        }
    }

    #[test]
    fn rebound_rings_without_clipping() {
        let weights = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let (peak, _) = probe(&Settings {
            weights,
            ..settings()
        });
        assert!(peak <= REBOUND_HEADROOM + 0.05, "peak x{peak}");
        let (peak, _) = probe(&Settings {
            stack: 1.0,
            haunt: 1.0,
            max_steps: 16,
            ..settings()
        });
        assert!(peak <= REBOUND_HEADROOM + 0.05, "peak x{peak}");
    }

    #[test]
    fn output_is_finite_and_bounded() {
        for seed in 0..20 {
            let s = Settings { seed, ..settings() };
            for frame in render(&s, true, SR as usize * 4) {
                for x in frame {
                    // A snapped high-Q low-pass can ring well above the test tone (peak 0.8).
                    // 8.0 still catches a filter that has run away.
                    assert!(x.is_finite() && x.abs() <= 8.0, "seed {seed}: {x}");
                }
            }
        }
    }

    #[test]
    fn stack_colors_a_history_glitch() {
        let weights = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let plain = Settings {
            weights,
            stack: 0.0,
            ..settings()
        };
        let stacked = Settings {
            weights,
            stack: 1.0,
            ..settings()
        };
        let plain_audio = render(&plain, true, SR as usize);
        let stacked_audio = render(&stacked, true, SR as usize);
        assert_ne!(plain_audio, stacked_audio);
        assert_eq!(stacked_audio, render(&stacked, true, SR as usize));
    }

    #[test]
    fn stack_color_cuts_in_and_out() {
        let s = Settings {
            weights: [1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            stack: 1.0,
            max_steps: 16,
            ..settings()
        };
        let mut stacked = 0;
        for seed in 0..400u64 {
            let mut rng = Rng::new(seed);
            let Some(ev) = Engine::roll(&mut rng, 0.0, 120.0, SR, 1 << 19, None, &s) else {
                continue;
            };
            if let Some(color) = ev.color {
                stacked += 1;
                let all = if color.slots == 32 {
                    u32::MAX
                } else {
                    (1u32 << color.slots) - 1
                };
                assert!(color.mask != 0 && color.mask != all, "{color:?}");
                assert_eq!(color.mask & !all, 0);
            }
        }
        assert!(stacked > 0);
    }

    #[test]
    fn haunt_replays_the_last_glitch() {
        let mut engine = Engine::default();
        engine.prepare(SR);
        let s = Settings {
            haunt: 1.0,
            ..settings()
        };
        let mut replays = 0;
        let mut checked = 0;
        for i in 0..SR as usize * 4 {
            let beats = Some(i as f64 * 140.0 / 60.0 / SR as f64);
            let out = engine.process(input(i), beats, 140.0, (4, 4), &s);
            if let Some(ev) = &engine.active
                && matches!(ev.kind, Kind::Ghost)
            {
                if ev.elapsed == 1 {
                    replays += 1;
                }
                let at = ev.elapsed as usize - 1;
                if engine.retiring.is_none()
                    && ev.env >= 1.0
                    && at + engine.fade_len as usize + 1 < engine.ghost.len
                {
                    assert!((out[0] - engine.ghost.channels[0][at]).abs() < 1.0e-6);
                    checked += 1;
                }
            }
        }
        assert!(replays > 0 && checked > 0);
        assert!(engine.ghost.spent || engine.ghost.len > 0);
        assert!(engine.share_transcript().text().starts_with("bar "));
        let haunted = render(&s, true, SR as usize * 2);
        assert_ne!(haunted, render(&settings(), true, SR as usize * 2));
        assert_eq!(haunted, render(&s, true, SR as usize * 2));
        for seed in 0..8 {
            let s = Settings {
                seed,
                haunt: 1.0,
                stack: 1.0,
                ..settings()
            };
            for frame in render(&s, true, SR as usize * 2) {
                for x in frame {
                    assert!(x.is_finite() && x.abs() <= 8.0, "seed {seed}: {x}");
                }
            }
        }
    }

    #[test]
    fn transcript_names_the_glitch() {
        assert_eq!(
            describe(17, 3, 3, Mode::Reverse, Some(Mode::Crush), true),
            "bar 17 · beat 3 · haunt of reverse · 3 steps · crush"
        );
        assert_eq!(
            describe(2, 1, 1, Mode::Gate, None, false),
            "bar 2 · beat 1 · gate · 1 step"
        );
        let mut engine = Engine::default();
        engine.prepare(SR);
        let s = settings();
        for i in 0..SR as usize {
            let beats = i as f64 * 120.0 / 60.0 / SR as f64;
            engine.process(input(i), Some(beats), 120.0, (4, 4), &s);
        }
        let line = engine.share_transcript().text();
        assert!(line.starts_with("bar "), "{line}");
        assert!(line.contains("step"), "{line}");
        assert!(
            MODES.iter().any(|mode| line.contains(mode.label())),
            "{line}"
        );
    }

    #[test]
    fn deterministic_mode_repeats_exactly() {
        let s = settings();
        assert_eq!(
            render(&s, true, SR as usize * 2),
            render(&s, true, SR as usize * 2)
        );
    }

    #[test]
    fn zero_chance_is_transparent() {
        let s = Settings {
            chance: 0.0,
            ..settings()
        };
        for (i, frame) in render(&s, false, 10_000).into_iter().enumerate() {
            assert_eq!(frame, input(i));
        }
    }

    #[test]
    fn every_mode_gets_picked() {
        let mut engine = Engine::default();
        engine.prepare(SR);
        let s = settings();
        let mut seen = [false; MODES.len()];
        for i in 0..SR as usize * 20 {
            let beats = i as f64 * 2.0 / SR as f64;
            engine.process(input(i), Some(beats), 120.0, (4, 4), &s);
            if let Some(mode) = engine.active_mode() {
                seen[MODES.iter().position(|m| *m == mode).unwrap()] = true;
            }
        }
        assert!(seen.iter().all(|s| *s), "{seen:?}");
    }

    #[test]
    fn no_hard_clicks_at_event_boundaries() {
        // A constant input should never jump by more than the fade allows, except inside crush,
        // gate, and rebound, which deliberately reshape the signal. The history is filled first so
        // reads into the past don't hit the silence from before the input started.
        let s = Settings {
            weights: [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
            ..settings()
        };
        let mut engine = Engine::default();
        engine.prepare(SR);
        let warmup = engine.history.len() + 1;
        let mut prev = 0.0f32;
        for i in 0..warmup + SR as usize * 8 {
            let beats = Some(i as f64 * 140.0 / 60.0 / SR as f64);
            let out = engine.process([0.5, 0.5], beats, 140.0, (4, 4), &s)[0];
            if i > warmup {
                assert!(
                    (out - prev).abs() < 0.05,
                    "jump of {} at sample {i}",
                    out - prev
                );
            }
            prev = out;
        }
    }
}
