//! Host-independent glitch engine. Audio is continuously recorded into a history ring buffer, and
//! on every grid step a dice roll decides whether a random glitch event replaces the dry signal.

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

    /// Linearly interpolated read at an absolute (possibly fractional or negative) position.
    fn read(&self, pos: f64) -> [f32; 2] {
        let floor = pos.floor();
        let frac = (pos - floor) as f32;
        let a = (floor as i64 as u64 & self.mask) as usize;
        let b = ((floor as i64 + 1) as u64 & self.mask) as usize;
        let [l, r] = &self.channels;
        [l[a] + (l[b] - l[a]) * frac, r[a] + (r[b] - r[a]) * frac]
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
    },
}

#[derive(Clone, Copy, Debug)]
struct Event {
    kind: Kind,
    /// Absolute history index of the sample at which the event started.
    origin: f64,
    elapsed: u64,
    expected_len: f64,
    steps_left: u32,
    env: f32,
}

impl Event {
    fn render(
        &mut self,
        history: &History,
        input: [f32; 2],
        fade_len: f64,
        gate_coef: f32,
        sample_rate: f32,
    ) -> [f32; 2] {
        let t = self.elapsed as f64;
        let out = match &mut self.kind {
            Kind::Stutter {
                offset,
                speed,
                slice_len,
                shrink,
                repeat_start,
            } => {
                let mut p = (self.elapsed - *repeat_start) as f64;
                if p >= *slice_len {
                    *repeat_start = self.elapsed;
                    p = 0.0;
                    if *shrink {
                        *slice_len = (*slice_len * 0.75).max(MIN_SLICE_SAMPLES * 4.0);
                    }
                }
                let window = (p / fade_len).min((*slice_len - p) / fade_len).min(1.0) as f32;
                let [l, r] = history.read(self.origin - *offset + p * *speed);
                [l * window, r * window]
            }
            Kind::Reverse => history.read(self.origin - 1.0 - t),
            Kind::TapeStop { pos } => {
                let out = history.read(self.origin + *pos);
                let remaining = (1.0 - t / self.expected_len).max(0.0);
                *pos += remaining * remaining.sqrt();
                out
            }
            Kind::Scramble { offset } => history.read(self.origin - *offset + t),
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
            } => {
                let progress = (t / self.expected_len).clamp(0.0, 1.0) as f32;
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
                out
            }
        };
        self.elapsed += 1;
        out
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
        }
    }
}

impl Engine {
    /// Allocates the history buffer. Must be called before processing, never from the audio thread.
    pub fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.history
            .allocate((sample_rate * HISTORY_SECONDS) as usize);
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
    }

    /// Processes one stereo frame and returns the fully wet output. `song_beats` is the transport
    /// position in quarter notes while the host is playing, or `None` to run on an internal clock.
    pub fn process(
        &mut self,
        input: [f32; 2],
        song_beats: Option<f64>,
        tempo: f64,
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
            self.on_step(step, contiguous, synced, now, tempo, settings);
        }

        let fade = (1.0 / self.fade_len) as f32;
        let mut wet = [0.0f32; 2];
        let mut env_sum = 0.0;

        if let Some(ev) = &mut self.active {
            ev.env = (ev.env + fade).min(1.0);
            let out = ev.render(
                &self.history,
                input,
                self.fade_len,
                self.gate_coef,
                self.sample_rate,
            );
            wet = [wet[0] + out[0] * ev.env, wet[1] + out[1] * ev.env];
            env_sum += ev.env;
        }
        if let Some(ev) = &mut self.retiring {
            ev.env -= fade;
            if ev.env <= 0.0 {
                self.retiring = None;
            } else {
                let out = ev.render(
                    &self.history,
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
        self.active.map(|ev| match ev.kind {
            Kind::Stutter { .. } => Mode::Stutter,
            Kind::Reverse => Mode::Reverse,
            Kind::TapeStop { .. } => Mode::TapeStop,
            Kind::Scramble { .. } => Mode::Scramble,
            Kind::Crush { .. } => Mode::Crush,
            Kind::Gate { .. } => Mode::Gate,
            Kind::Rebound { .. } => Mode::Rebound,
        })
    }

    fn on_step(
        &mut self,
        step: i64,
        contiguous: bool,
        synced: bool,
        now: f64,
        tempo: f64,
        s: &Settings,
    ) {
        if let Some(ev) = &mut self.active {
            if contiguous && ev.steps_left > 1 {
                ev.steps_left -= 1;
                return;
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
        self.active = Self::roll(
            &mut rng,
            now,
            tempo,
            self.sample_rate,
            self.history.len(),
            s,
        );
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
        s: &Settings,
    ) -> Option<Event> {
        if rng.next_f32() >= s.chance {
            return None;
        }
        let mode = Self::pick_mode(rng, &s.weights)?;

        let step_len =
            (s.step_beats * 60.0 / tempo.max(1.0) * sample_rate as f64).max(MIN_SLICE_SAMPLES);
        // Bias towards short events, and leave headroom in the history for reverse/scramble reads.
        let budget = (history_len as f64 * 0.45 / step_len).floor().max(1.0) as u32;
        let u = rng.next_f32();
        let steps = (1 + (u * u * s.max_steps.max(1) as f32) as u32)
            .min(s.max_steps.max(1))
            .min(budget);
        let expected_len = steps as f64 * step_len;

        let kind = match mode {
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
                let offset = if speed > 1.0 { slice_len * speed } else { 0.0 };
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
                    offset: back as f64 * step_len,
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
                }
            }
        };

        Some(Event {
            kind,
            origin: now,
            elapsed: 0,
            expected_len,
            steps_left: steps,
            env: 0.0,
        })
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
                engine.process(input(i), beats, tempo, settings)
            })
            .collect()
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
            engine.process(input(i), Some(beats), 120.0, &s);
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
            let out = engine.process([0.5, 0.5], beats, 140.0, &s)[0];
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
