#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use crate::backend::{CommitNode, RepoView};
use crate::jj::Repo;
use clap::Parser;
use color_eyre::Result;
use color_eyre::eyre::{ContextCompat, eyre};
use eframe::egui::{self, Color32, Theme};
use egui::epaint::{ColorMode, CubicBezierShape, PathStroke};
use egui::{DragAndDrop, FontId, Margin, Pos2, Rect, RichText, Stroke, StrokeKind, TextEdit, TextStyle, Vec2, Widget};
use jj_lib::backend::CommitId;
use jj_lib::ref_name::RefNameBuf;
use renderdag::{LinkLine, NodeLine};
use std::fmt::Display;
use std::ops::RangeInclusive;
use std::path::PathBuf;

mod backend;
mod egui_formatter;
mod jj;

#[derive(clap::Parser)]
struct Args {
    #[arg(long, default_value = std::env::current_dir().unwrap().into_os_string())]
    repository: PathBuf,
    #[arg(short = 'r', long, value_name = "REVSETS")]
    revisions: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    color_eyre::install()?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1200., 400.]),
        ..Default::default()
    };
    let app = App::load(args)?;
    eframe::run_native(
        "kahva",
        options,
        Box::new(|cc| {
            setup_custom_style(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| eyre!("{e}"))?;
    Ok(())
}

fn setup_custom_style(ctx: &egui::Context) {
    //ctx.set_pixels_per_point(1.2);
    ctx.set_pixels_per_point(1.2);
    ctx.style_mut_of(Theme::Dark, |style| {
        // style.visuals.panel_fill = Color32::from_rgb(11, 11, 22);
        style.visuals.panel_fill = Color32::from_rgb(28, 30, 34);
        *style.text_styles.get_mut(&TextStyle::Body).unwrap() = FontId::proportional(14.0);
        style.interaction.selectable_labels = false;
        // style.debug.show_widget_hits = true;
    });
}

struct App(UiState, RepoView);
impl App {
    fn load(args: Args) -> Result<App> {
        let repo = Repo::detect(&args.repository)?
            .with_context(|| format!("No repo was found at {}", args.repository.display()))?;
        let content = backend::reload(&repo, &args)?;

        let debug = false;
        Ok(App(
            UiState {
                args,
                formatter: egui_formatter::ColorFormatter::for_config(repo.settings().config(), debug)?,
                repo,
                style: AppStyle::default(),
                selected_commits: IndexSet::default(),
                error: None,
                initial_sized: false,
                dirty: false,
            },
            content,
        ))
    }
}

struct UiState {
    args: Args,
    repo: Repo,
    formatter: egui_formatter::ColorFormatter,
    style: AppStyle,

    error: Option<String>,

    initial_sized: bool,
    dirty: bool,
}

impl UiState {
    fn describe(&mut self, commit_id: &CommitId, description: &str) -> Result<()> {
        let commit = self.repo.commit(commit_id)?;
        self.repo.describe(&commit, description)?;
        self.reload();
        Ok(())
    }
    fn reload(&mut self) {
        self.dirty = true;
        self.clear_error();
    }

    fn clear_error(&mut self) {
        self.error = None;
    }
}

struct AppStyle {
    graph_cell_size: Vec2,
    graph_stroke: Stroke,
}

impl Default for AppStyle {
    fn default() -> Self {
        AppStyle {
            graph_cell_size: Vec2::new(16.0, 20.0),
            graph_stroke: Stroke {
                width: 1.,
                color: Color32::from_rgb(104, 148, 187),
            },
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.0.dirty {
            let res = self.0.repo.reload().and(backend::reload(&self.0.repo, &self.0.args));
            if let Some(repo_view) = self.0.catch(res) {
                self.1 = repo_view;
            }
            self.0.dirty = false;
        }
        self.0.update(ctx, &self.1)
    }
}

impl UiState {
    fn update(&mut self, ctx: &egui::Context, content: &RepoView) {
        #[cfg(any())]
        egui::Window::new("Theme")
            .fixed_pos(ctx.used_size().to_pos2())
            .default_open(false)
            .show(ctx, |ui| theme_window(ctx, ui, &mut self.style));

        if let Some(error) = &self.error {
            egui::Area::new(egui::Id::new("error"))
                .anchor(egui::Align2::RIGHT_BOTTOM, [-10., -10.])
                .default_size(Vec2::splat(400.0))
                .show(ctx, |ui| {
                    ui.label(RichText::new(error).color(Color32::from_rgb(255, 0, 51)));
                });
        }

        egui::Area::new(egui::Id::new("controls"))
            .anchor(egui::Align2::RIGHT_TOP, [-10., 10.])
            .show(ctx, |ui| {
                if ui.button("⟳").clicked() {
                    self.reload();
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            for node in &content.nodes {
                let line = &node.row;

                self.draw_line_row(ui, &content, node);

                if let Some(link_row) = &line.link_line {
                    self.draw_line_link(ui, link_row);
                }

                if let Some(term_row) = &line.term_line {
                    let (response, painter) = ui.allocate_painter(
                        self.style.graph_cell_size * Vec2::new(term_row.len() as f32, 1.0),
                        egui::Sense::empty(),
                    );

                    for (i, _) in term_row.iter().enumerate() {
                        let rect = rect_subdiv_x(response.rect, term_row.len(), i);

                        for i in 0..4 {
                            let pos = rect.center_top() + Vec2::DOWN * i as f32 * 3.0;
                            painter.circle_filled(pos + Vec2::X * 0.25, 0.5, self.style.graph_stroke.color);
                        }
                    }
                }
            }
        });

        let used_size = ctx.used_size();
        if !self.initial_sized && used_size.x > 0. && used_size.x < 5000. {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(used_size));
            self.initial_sized = true;
        }
    }
}

#[derive(Debug)]
enum DropPayload {
    Bookmark(RefNameBuf),
}

impl UiState {
    fn draw_line_row(&mut self, ui: &mut egui::Ui, content: &RepoView, node: &CommitNode) {
        let id = node
            .commit_id
            .as_ref()
            .map(egui::Id::new)
            .unwrap_or_else(|| egui::Id::new("todo"));

        let node_line = &node.row.node_line;

        let style = ui.style_mut();
        style.spacing.item_spacing = Vec2::ZERO;
        style.spacing.interact_size = Vec2::ZERO;

        ui.horizontal(|ui| {
            ui.reset_style();

            let (response, painter) = ui.allocate_painter(
                self.style.graph_cell_size * Vec2::new(node_line.len() as f32, 1.0),
                egui::Sense::empty(),
            );
            for (i, line) in node_line.iter().enumerate() {
                let rect = rect_subdiv_x(response.rect, node_line.len(), i);
                if let NodeLine::Blank = line {
                    continue;
                }

                let is_head = i == node_line.len() - 1
                    && node
                        .commit_id
                        .as_ref()
                        .is_some_and(|commit_id| content.heads.contains(commit_id));

                if is_head {
                    painter.line_segment([rect.center(), rect.center_bottom()], self.style.graph_stroke);
                } else {
                    painter.line_segment([rect.center_top(), rect.center_bottom()], self.style.graph_stroke);
                }
                if let NodeLine::Node = line {
                    painter.circle_filled(rect.center() + Vec2::X * 0.25, 3.0, self.style.graph_stroke.color);
                }
            }

            let mut msg = |ui: &mut egui::Ui| {
                node.msg.replay(&mut self.formatter).unwrap();

                let layout = egui::Layout::left_to_right(egui::Align::Center);
                ui.with_layout(layout, |ui| {
                    ui.style_mut().spacing.item_spacing = Vec2::ZERO;
                    let sections = self.formatter.take();

                    for (i, (job, label)) in sections.into_iter().enumerate() {
                        match label.as_deref() {
                            Some("bookmarks") if node.commit_id.is_some() => {
                                let bookmark = RefNameBuf::from(job.text.trim().trim_end_matches("*").to_owned());
                                ui.dnd_drag_source(id.with(i), DropPayload::Bookmark(bookmark), |ui| ui.label(job));
                            }
                            Some("description") => {
                                let desc_id = id.with("description");
                                let is_empty = job.text == "(no description set)";

                                let mut description_text = ui.data_mut(|data| {
                                    data.get_temp_mut_or_insert_with(desc_id, || match is_empty {
                                        true => String::new(),
                                        false => job.text.trim().to_owned(),
                                    })
                                    .clone()
                                });

                                let response = TextEdit::singleline(&mut description_text)
                                    .hint_text("(no description set)")
                                    .frame(false)
                                    .min_size(Vec2::ZERO)
                                    .margin(Margin::symmetric(4, 0))
                                    .clip_text(false)
                                    .ui(ui);

                                if response.lost_focus() {
                                    if job.text.trim() != description_text {
                                        let commit = node.commit_id.as_ref().unwrap();
                                        let res = self.describe(commit, &description_text);
                                        self.catch(res);
                                        ui.data_mut(|data| data.remove_temp::<String>(desc_id));
                                    }
                                } else if response.changed() {
                                    ui.data_mut(|data| data.insert_temp(desc_id, description_text));
                                }
                            }
                            _ => {
                                ui.label(job);
                            }
                        }
                    }
                });
            };

            if let Some(commit_id) = &node.commit_id {
                if DragAndDrop::has_payload_of_type::<DropPayload>(ui.ctx()) {
                    let frame = egui::Frame::dark_canvas(ui.style())
                        .outer_margin(Margin::ZERO)
                        .inner_margin(Margin::ZERO)
                        .corner_radius(0)
                        .stroke(Stroke::NONE);
                    let result = ui.dnd_drop_zone::<DropPayload, _>(frame, msg);
                    if let Some(result) = result.1 {
                        self.handle_drop(commit_id, &result);
                    }
                } else {
                    msg(ui);
                }
                // ui.dnd_drag_source(egui::Id::new(commit_id), node.commit_id.clone(), msg);
            } else {
                msg(ui);
            }
        });
    }

    fn draw_line_link(&mut self, ui: &mut egui::Ui, link_row: &[LinkLine]) {
        let (response, painter) = ui.allocate_painter(
            self.style.graph_cell_size * Vec2::new(link_row.len() as f32, 1.0),
            egui::Sense::empty(),
        );

        let n = link_row.len();
        for (i, cur) in link_row.iter().enumerate() {
            let rect = rect_subdiv_x(response.rect, n, i);
            let first_rect = rect_subdiv_x(response.rect, n, 0);
            let next_rect = rect_subdiv_x(response.rect, n, i + 1);

            if cur.intersects(LinkLine::HORIZONTAL) {
                // painter.line_segment([rect.left_center(), rect.right_center()], stroke);
            }
            if cur.intersects(LinkLine::VERTICAL) {
                painter.line_segment([rect.center_top(), rect.center_bottom()], self.style.graph_stroke);
            }
            if cur.intersects(LinkLine::RIGHT_FORK) {
                painter.add(self.bezier(
                    next_rect.center_top(),
                    rect.center_bottom(),
                    Vec2::Y * self.style.graph_cell_size.y * 0.8,
                ));
            }
            if cur.intersects(LinkLine::RIGHT_MERGE) {
                painter.add(self.bezier(
                    rect.center_top(),
                    next_rect.center_bottom(),
                    Vec2::Y * self.style.graph_cell_size.y * 0.8,
                ));
            }
            if cur.intersects(LinkLine::LEFT_FORK) {
                painter.add(self.bezier(
                    first_rect.center_top(),
                    rect.center_bottom(),
                    Vec2::Y * self.style.graph_cell_size.y * 0.8,
                ));
            }
            if cur.intersects(LinkLine::LEFT_MERGE) {}
        }
    }

    fn bezier(&self, from: Pos2, to: Pos2, delta: Vec2) -> CubicBezierShape {
        CubicBezierShape {
            points: [from, from + delta, to - delta, to],
            closed: false,
            fill: Color32::TRANSPARENT,
            stroke: PathStroke {
                width: self.style.graph_stroke.width,
                color: ColorMode::Solid(self.style.graph_stroke.color),
                kind: StrokeKind::Middle,
            },
        }
    }

    fn catch<T, E: Display>(&mut self, res: Result<T, E>) -> Option<T> {
        if let Err(error) = &res {
            eprintln!("{error}");
            self.error = Some(error.to_string());
        }
        res.ok()
    }

    fn handle_drop(&mut self, commit: &CommitId, payload: &DropPayload) {
        match payload {
            DropPayload::Bookmark(bookmark) => {
                let res = self.repo.move_bookmark(bookmark, commit);
                self.catch(res);
                self.reload();
            }
        }
    }
}

fn rect_subdiv_x(rect: Rect, n_x: usize, i: usize) -> Rect {
    let w = rect.width() / n_x as f32;
    Rect::from_min_size(
        Pos2::new(rect.min.x + w * i as f32, rect.min.y),
        Vec2::new(w, rect.height()),
    )
}

#[allow(dead_code)]
fn theme_window(ctx: &egui::Context, ui: &mut egui::Ui, style: &mut AppStyle) {
    egui::Grid::new("settings").show(ui, |ui| {
        const POSITIVE: RangeInclusive<f32> = 1.0..=f32::MAX;
        ui.label("Size (x)");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut style.graph_cell_size.x).range(POSITIVE));
            ui.add(egui::DragValue::new(&mut style.graph_cell_size.y).range(POSITIVE));
        });
        ui.end_row();
        ui.label("Stroke Width");
        ui.add(
            egui::DragValue::new(&mut style.graph_stroke.width)
                .range(0.1..=5.0)
                .speed(0.01),
        );
        ui.end_row();
        ui.label("Stroke Color");
        ui.color_edit_button_srgba(&mut style.graph_stroke.color);
        ui.end_row();

        ui.label("Background Color");
        let mut bg = ctx.style().visuals.panel_fill;
        if ui.color_edit_button_srgba(&mut bg).changed() {
            ctx.style_mut(|style| style.visuals.panel_fill = bg);
        }
        ui.end_row();

        ui.label("PPP");
        let mut ppp = ctx.pixels_per_point();
        if ui
            .add(egui::DragValue::new(&mut ppp).range(0.1..=5.0).speed(0.01))
            .changed()
        {
            ctx.set_pixels_per_point(ppp);
            ctx.stop_dragging();
        }

        ui.end_row();
        ui.allocate_space(ui.available_size());
    });
}
