//! Plugin window. Labels use IBM Plex Sans; section titles use the bold cut.

use std::sync::Arc;

use nice_plug::context::gui::GuiContext;
use nice_plug::prelude::*;
use nice_plug_egui::NiceEguiApp;

use crate::{Grid, ManglerParams};

const NEON: egui::Color32 = egui::Color32::from_rgb(57, 255, 92);
const LABEL: egui::Color32 = egui::Color32::from_rgb(214, 224, 210);
const VALUE: egui::Color32 = egui::Color32::from_rgb(176, 255, 196);
const TRACK: egui::Color32 = egui::Color32::from_rgb(28, 36, 30);
const INK: egui::Color32 = egui::Color32::from_rgb(8, 12, 9);

pub struct ManglerEditor {
    params: Arc<ManglerParams>,
    open_state: Option<OpenEditorState>,
}

struct OpenEditorState {
    nice_gui_ctx: GuiContext,
}

impl ManglerEditor {
    pub fn new(params: Arc<ManglerParams>) -> Self {
        Self {
            params,
            open_state: None,
        }
    }
}

impl NiceEguiApp for ManglerEditor {
    fn build(
        &mut self,
        egui_ctx: egui::Context,
        nice_gui_ctx: GuiContext,
        _frame: &mut nice_plug_egui::Frame,
    ) -> Result<(), nice_plug_egui::baseview::HandlerError> {
        install_fonts(&egui_ctx);
        install_theme(&egui_ctx);
        self.open_state = Some(OpenEditorState { nice_gui_ctx });
        Ok(())
    }

    fn editor_closed(&mut self) {
        self.open_state = None;
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut nice_plug_egui::Frame) {
        let Some(state) = self.open_state.as_ref() else {
            return;
        };
        let setter = state.nice_gui_ctx.param_setter();
        let params = Arc::clone(&self.params);

        egui::CentralPanel::default().show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.label(title("Glitch Mangler"));
                    rule(ui, NEON.gamma_multiply(0.45));
                    ui.add_space(10.0);

                    ui.columns(2, |cols| {
                        section_label(&mut cols[0], "Playback");
                        slider(
                            &mut cols[0],
                            &params.chance,
                            &setter,
                            Some("Chance that a glitch starts on each grid step."),
                        );
                        grid_combo(
                            &mut cols[0],
                            &params.grid,
                            &setter,
                            "Length of one step, locked to the host tempo.",
                        );
                        slider(
                            &mut cols[0],
                            &params.max_steps,
                            &setter,
                            Some("Longest a glitch can run, in grid steps."),
                        );
                        slider(
                            &mut cols[0],
                            &params.mix,
                            &setter,
                            Some("Balance between the dry signal and the glitch."),
                        );
                        slider(
                            &mut cols[0],
                            &params.stack,
                            &setter,
                            Some("Chance that Crush, Gate, or Rebound also runs on a Stutter, Reverse, Tape Stop, or Scramble."),
                        );
                        slider(
                            &mut cols[0],
                            &params.seed,
                            &setter,
                            Some("Picks a different pattern. The same seed repeats the same glitches."),
                        );
                        ui_checkbox(
                            &mut cols[0],
                            &params.deterministic,
                            &setter,
                            "Tie the pattern to the song position, so playback and bounce match. With the transport stopped, the plugin follows the host tempo on its own clock.",
                        );

                        section_label(&mut cols[1], "Effects");
                        slider(
                            &mut cols[1],
                            &params.stutter,
                            &setter,
                            Some("Loops a slice of the current step. Sometimes pitched, or shrinking into a roll."),
                        );
                        slider(
                            &mut cols[1],
                            &params.reverse,
                            &setter,
                            Some("Plays the audio just before this step backwards."),
                        );
                        slider(
                            &mut cols[1],
                            &params.tape_stop,
                            &setter,
                            Some("Slows playback down until it stops."),
                        );
                        slider(
                            &mut cols[1],
                            &params.scramble,
                            &setter,
                            Some("Jumps back 1–8 steps and replays that audio."),
                        );
                        slider(
                            &mut cols[1],
                            &params.crush,
                            &setter,
                            Some("Reduces bit depth and sample rate."),
                        );
                        slider(
                            &mut cols[1],
                            &params.gate,
                            &setter,
                            Some("Chops the signal in rhythm with the grid."),
                        );
                        slider(
                            &mut cols[1],
                            &params.rebound,
                            &setter,
                            Some("Cutoff snaps twice per grid step, from open to shut and back. Some steps drop to near silence and ring."),
                        );
                    });
                });
        });
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Plex".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/IBMPlexSans-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "PlexBold".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/IBMPlexSans-Bold.ttf"
        ))),
    );
    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap()
        .insert(0, "Plex".to_owned());
    fonts.font_data.insert(
        "Goblin".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/glitch-goblin-font/GlitchGoblin-2O87v.ttf"
        ))),
    );
    fonts.families.insert(
        egui::FontFamily::Name("Heading".into()),
        vec!["PlexBold".to_owned()],
    );
    fonts.families.insert(
        egui::FontFamily::Name("Title".into()),
        vec!["Goblin".to_owned(), "Plex".to_owned()],
    );
    ctx.set_fonts(fonts);
}

fn install_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = INK;
    visuals.window_fill = egui::Color32::from_rgb(14, 20, 15);
    visuals.window_stroke = egui::Stroke::new(1.0, NEON.gamma_multiply(0.65));
    visuals.window_corner_radius = egui::CornerRadius::same(4);
    visuals.override_text_color = Some(LABEL);
    visuals.selection.bg_fill = NEON;
    visuals.selection.stroke = egui::Stroke::new(1.0, NEON);
    visuals.hyperlink_color = NEON;
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.bg_fill = egui::Color32::from_rgb(18, 26, 20);
        widget.weak_bg_fill = widget.bg_fill;
        widget.fg_stroke.color = LABEL;
        widget.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(46, 72, 50));
    }
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, NEON);
    visuals.widgets.active.bg_fill = NEON.gamma_multiply(0.25);
    visuals.widgets.active.fg_stroke.color = INK;
    ctx.set_visuals(visuals);
}

fn title(text: &str) -> egui::RichText {
    egui::RichText::new(text)
        .family(egui::FontFamily::Name("Title".into()))
        .size(36.0)
        .color(egui::Color32::from_rgb(244, 255, 246))
}

fn heading(text: &str, size: f32) -> egui::RichText {
    egui::RichText::new(text)
        .family(egui::FontFamily::Name("Heading".into()))
        .size(size)
        .color(egui::Color32::from_rgb(244, 255, 246))
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(heading(text, 16.0));
    rule(ui, egui::Color32::from_rgb(42, 68, 46));
    ui.add_space(4.0);
}

fn rule(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, color),
    );
}

fn slider(ui: &mut egui::Ui, param: &impl Param, setter: &ParamSetter, tip: Option<&str>) {
    ui.add_space(8.0);
    let name = ui.label(egui::RichText::new(param.name()).size(13.0).color(LABEL));
    if let Some(tip) = tip {
        name.on_hover_cursor(egui::CursorIcon::Help)
            .on_hover_text(tip);
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        neon_slider(ui, param, setter);
        ui.label(
            egui::RichText::new(
                param.normalized_value_to_string(param.modulated_normalized_value(), true),
            )
            .size(12.0)
            .color(VALUE),
        );
    });
}

/// Filled neon track with a small square handle.
fn neon_slider(ui: &mut egui::Ui, param: &impl Param, setter: &ParamSetter) {
    let width = (ui.available_width() - 64.0).max(48.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, 16.0), egui::Sense::click_and_drag());

    if response.double_clicked() {
        setter.begin_set_parameter(param);
        setter.set_parameter(param, param.default_plain_value());
        setter.end_set_parameter(param);
    } else {
        if response.drag_started() {
            setter.begin_set_parameter(param);
        }
        if (response.drag_started() || response.dragged())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let normalized = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            setter.set_parameter(param, param.preview_plain(normalized));
        }
        if response.drag_stopped() {
            setter.end_set_parameter(param);
        }
    }

    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(if response.dragged() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        });
    }

    let normalized = param.modulated_normalized_value().clamp(0.0, 1.0);
    let painter = ui.painter();
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 4.0));
    painter.rect_filled(track, 2.0, TRACK);

    if normalized > 0.0 {
        let mut fill = track;
        fill.set_right(track.left() + track.width() * normalized);
        painter.rect_filled(fill, 2.0, NEON);
    }

    let half = 6.0;
    let x =
        (rect.left() + rect.width() * normalized).clamp(rect.left() + half, rect.right() - half);
    let thumb =
        egui::Rect::from_center_size(egui::pos2(x, rect.center().y), egui::vec2(12.0, 12.0));
    painter.rect_filled(thumb, 2.0, INK);
    painter.rect_stroke(
        thumb,
        2.0,
        egui::Stroke::new(1.5, NEON),
        egui::StrokeKind::Outside,
    );
}

fn grid_combo(ui: &mut egui::Ui, param: &EnumParam<Grid>, setter: &ParamSetter, tip: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(param.name()).size(13.0).color(LABEL))
        .on_hover_cursor(egui::CursorIcon::Help)
        .on_hover_text(tip);
    let current = param.value().to_index();
    egui::ComboBox::from_id_salt("grid")
        .selected_text(Grid::variants()[current])
        .show_ui(ui, |ui| {
            for (index, name) in Grid::variants().iter().enumerate() {
                if ui.selectable_label(index == current, *name).clicked() {
                    setter.begin_set_parameter(param);
                    setter.set_parameter(param, Grid::from_index(index));
                    setter.end_set_parameter(param);
                }
            }
        });
}

fn ui_checkbox(ui: &mut egui::Ui, param: &BoolParam, setter: &ParamSetter, tip: &str) {
    ui.add_space(10.0);
    let mut value = param.value();
    let toggle = ui
        .checkbox(&mut value, param.name())
        .on_hover_cursor(egui::CursorIcon::Help)
        .on_hover_text(tip);
    if toggle.changed() {
        setter.begin_set_parameter(param);
        setter.set_parameter(param, value);
        setter.end_set_parameter(param);
    }
}
