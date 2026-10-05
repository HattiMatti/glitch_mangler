use nice_plug::prelude::*;
use std::sync::Arc;

mod engine;

use engine::{Engine, Settings};

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
enum Grid {
    #[name = "1/4"]
    Quarter,
    #[name = "1/8"]
    Eighth,
    #[name = "1/8T"]
    EighthTriplet,
    #[name = "1/16"]
    Sixteenth,
    #[name = "1/16T"]
    SixteenthTriplet,
    #[name = "1/32"]
    ThirtySecond,
}

impl Grid {
    fn beats(self) -> f64 {
        match self {
            Grid::Quarter => 1.0,
            Grid::Eighth => 0.5,
            Grid::EighthTriplet => 1.0 / 3.0,
            Grid::Sixteenth => 0.25,
            Grid::SixteenthTriplet => 1.0 / 6.0,
            Grid::ThirtySecond => 0.125,
        }
    }
}

#[derive(Params)]
struct ManglerParams {
    #[id = "chance"]
    chance: FloatParam,
    #[id = "grid"]
    grid: EnumParam<Grid>,
    #[id = "length"]
    max_steps: IntParam,
    #[id = "mix"]
    mix: FloatParam,
    #[id = "seed"]
    seed: IntParam,
    #[id = "lock"]
    deterministic: BoolParam,

    #[id = "w_stutter"]
    stutter: FloatParam,
    #[id = "w_reverse"]
    reverse: FloatParam,
    #[id = "w_tape"]
    tape_stop: FloatParam,
    #[id = "w_scramble"]
    scramble: FloatParam,
    #[id = "w_crush"]
    crush: FloatParam,
    #[id = "w_gate"]
    gate: FloatParam,
}

fn percent(name: &str, default: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Linear { min: 0.0, max: 1.0 })
        .with_unit("%")
        .with_value_to_string(formatters::v2s_f32_percentage(0))
        .with_string_to_value(formatters::s2v_f32_percentage())
}

impl Default for ManglerParams {
    fn default() -> Self {
        Self {
            chance: percent("Chance", 0.35),
            grid: EnumParam::new("Grid", Grid::Sixteenth),
            max_steps: IntParam::new("Max Length", 4, IntRange::Linear { min: 1, max: 16 })
                .with_unit(" steps"),
            mix: percent("Mix", 1.0).with_smoother(SmoothingStyle::Linear(20.0)),
            seed: IntParam::new("Seed", 1, IntRange::Linear { min: 0, max: 999 }),
            deterministic: BoolParam::new("Lock to Song", true),

            stutter: percent("Stutter", 1.0),
            reverse: percent("Reverse", 0.5),
            tape_stop: percent("Tape Stop", 0.4),
            scramble: percent("Scramble", 0.5),
            crush: percent("Crush", 0.3),
            gate: percent("Gate", 0.3),
        }
    }
}

impl ManglerParams {
    fn settings(&self) -> Settings {
        Settings {
            chance: self.chance.value(),
            step_beats: self.grid.value().beats(),
            max_steps: self.max_steps.value() as u32,
            weights: [
                self.stutter.value(),
                self.reverse.value(),
                self.tape_stop.value(),
                self.scramble.value(),
                self.crush.value(),
                self.gate.value(),
            ],
            seed: self.seed.value() as u32,
            deterministic: self.deterministic.value(),
        }
    }
}

#[derive(Default)]
struct GlitchMangler {
    params: Arc<ManglerParams>,
    engine: Engine,
    sample_rate: f32,
}

impl Plugin for GlitchMangler {
    const NAME: &'static str = "Glitch Mangler";
    const VENDOR: &'static str = "hattimatti";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            ..AudioIOLayout::const_default()
        },
    ];

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn activate(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl ActivateContext<Self>,
    ) -> bool {
        if self.sample_rate != buffer_config.sample_rate {
            self.sample_rate = buffer_config.sample_rate;
            self.engine.prepare(buffer_config.sample_rate);
        }
        true
    }

    fn reset(&mut self) {
        self.engine.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let transport = context.transport();
        let tempo = transport.tempo.unwrap_or(120.0);
        let block_beats = if transport.playing { transport.pos_beats() } else { None };
        let beats_per_sample = tempo / 60.0 / self.sample_rate as f64;
        let settings = self.params.settings();

        for (i, mut frame) in buffer.iter_samples().enumerate() {
            let mix = self.params.mix.smoothed.next();
            let song_beats = block_beats.map(|b| b + i as f64 * beats_per_sample);

            let left = frame.get_mut(0).map_or(0.0, |x| *x);
            let right = frame.get_mut(1).map_or(left, |x| *x);
            let wet = self.engine.process([left, right], song_beats, tempo, &settings);

            let dry = [left, right];
            for (ch, sample) in frame.iter_mut().enumerate().take(2) {
                *sample = dry[ch] + (wet[ch] - dry[ch]) * mix;
            }
        }

        ProcessStatus::Normal
    }
}

impl ClapPlugin for GlitchMangler {
    const CLAP_ID: &'static str = "com.hattimatti.glitch-mangler";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Tempo-synced random stutter, reverse, tape stop, scramble, crush and gate");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Glitch,
        ClapFeature::Stereo,
        ClapFeature::Mono,
    ];

    fn remote_controls(&self, context: &mut impl RemoteControlsContext) {
        let p = &self.params;
        context.add_section("Mangler", |section| {
            section.add_page("Main", |page| {
                page.add_param(&p.chance);
                page.add_param(&p.grid);
                page.add_param(&p.max_steps);
                page.add_param(&p.mix);
                page.add_param(&p.seed);
                page.add_param(&p.deterministic);
                page.add_spacer();
                page.add_spacer();
            });
            section.add_page("Effects", |page| {
                page.add_param(&p.stutter);
                page.add_param(&p.reverse);
                page.add_param(&p.tape_stop);
                page.add_param(&p.scramble);
                page.add_param(&p.crush);
                page.add_param(&p.gate);
                page.add_spacer();
                page.add_spacer();
            });
        });
    }
}

nice_export_clap!(GlitchMangler);
