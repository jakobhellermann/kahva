#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use std::borrow::Cow;
use std::path::Path;

use anyhow::Result;
use eframe::egui::{self, Color32, Theme};
use egui::epaint::{ColorMode, CubicBezierShape, PathStroke};
use egui::{Pos2, Rect, Stroke, StrokeKind, Vec2};
use jj_cli::formatter::{FormatRecorder, PlainTextFormatter};
use jj_lib::backend::CommitId;
use jj_lib::config::{ConfigGetError, ConfigGetResultExt};
use jj_lib::graph::{GraphEdge, GraphEdgeType, TopoGroupedGraphIterator};
use jj_lib::settings::UserSettings;
use renderdag::{Ancestor, GraphRow, GraphRowRenderer, LinkLine, NodeLine, Renderer};

mod egui_formatter;
mod jj;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([100.0, 100.0]),
        ..Default::default()
    };
    eframe::run_native("kahva", options, Box::new(|cc| Ok(Box::new(MyApp::new(cc)))))
}

fn setup_custom_style(ctx: &egui::Context) {
    ctx.set_pixels_per_point(1.2);
    ctx.style_mut_of(Theme::Dark, |style| {
        style.visuals.panel_fill = Color32::from_rgb(43, 43, 46);
    });
}

struct CommitNode {
    commit_id: CommitId,
    msg: FormatRecorder,
    row: GraphRow<(CommitId, bool)>,
}

//

fn load() -> Result<MyApp> {
    let repo = jj::Repo::detect(Path::new("/home/jakob/dev/jj/jj"))?.unwrap();
    let log_revset = repo.settings().get_string("revsets.log")?;

    let prio_revset = repo.settings().get_string("revsets.log-graph-prioritize")?;
    let prio_revset = repo.revset_expression(&prio_revset)?;

    let log_template = repo.settings_commit_template("templates.log")?;
    let node_template = repo.parse_commit_opt_template(&get_node_template(repo.settings())?)?;
    let use_elided_nodes = repo.settings().get_bool("ui.log-synthetic-elided-nodes")?;

    let revset = repo.revset_expression(&log_revset)?.evaluate()?;
    let has_commit = revset.containing_fn();
    let mut iter = TopoGroupedGraphIterator::new(revset.iter_graph());

    for prio in prio_revset.evaluate_to_commit_ids()? {
        let prio = prio?;
        if has_commit(&prio)? {
            iter.prioritize_branch(prio);
        }
    }

    let mut nodes = Vec::new();

    let mut graph = GraphRowRenderer::new();

    for node in iter {
        let (commit_id, edges) = node?;

        let mut graphlog_edges = vec![];
        let mut missing_edge_id = None;
        let mut elided_targets = vec![];
        for edge in edges {
            match edge.edge_type {
                GraphEdgeType::Missing => {
                    missing_edge_id = Some(edge.target);
                }
                GraphEdgeType::Direct => {
                    graphlog_edges.push(GraphEdge::direct((edge.target, false)));
                }
                GraphEdgeType::Indirect => {
                    if use_elided_nodes {
                        elided_targets.push(edge.target.clone());
                        graphlog_edges.push(GraphEdge::direct((edge.target, true)));
                    } else {
                        graphlog_edges.push(GraphEdge::indirect((edge.target, false)));
                    }
                }
            }
        }
        if let Some(missing_edge_id) = missing_edge_id {
            graphlog_edges.push(GraphEdge::missing((missing_edge_id, false)));
        }
        let buffer = vec![];
        let key = (commit_id.clone(), false);
        let commit = repo.commit(&key.0)?;

        let mut node_out = Vec::new();
        let mut f = PlainTextFormatter::new(&mut node_out);
        node_template.format(&Some(commit.clone()), &mut f)?;
        let _node_symbol = String::from_utf8(node_out)?;
        let node_symbol = "o";

        let edges = graphlog_edges.iter().map(convert_graph_edge_into_ancestor).collect();
        let row = graph.next_row(key, edges, node_symbol.into(), String::from_utf8_lossy(&buffer).into());

        let mut f = FormatRecorder::new();
        log_template.format(&commit, &mut f)?;

        nodes.push(CommitNode { commit_id, msg: f, row });
        /*for elided_target in elided_targets {
            let elided_key = (elided_target, true);
            let real_key = (elided_key.0.clone(), false);
            let edges = [GraphEdge::direct(real_key)];
            let mut buffer = vec![];
            let within_graph = with_content_format.sub_width(graph.width(&elided_key, &edges));
            within_graph.write(ui.new_formatter(&mut buffer).as_mut(), |formatter| {
                writeln!(formatter.labeled("elided"), "(elided revisions)")
            })?;
            let node_symbol = format_template(ui, &None, &node_template);
            graph.add_node(
                &elided_key,
                &edges,
                &node_symbol,
                &String::from_utf8_lossy(&buffer),
            )?;
        }*/
    }

    /*for commit in iter {
        let commit = repo.commit(&commit?.0)?;
        let mut out = Vec::new();
        let mut f = PlainTextFormatter::new(&mut out);
        log_template.format(&commit, &mut f)?;
        let node = CommitNode {
            commit_id: commit.id().to_owned(),
            msg: String::from_utf8(out)?,
        };

        nodes.push(node);
    }*/

    Ok(MyApp {
        content: Content { nodes },
        formatter: egui_formatter::ColorFormatter::for_config(repo.settings().config(), false)?,
    })
}

struct MyApp {
    formatter: egui_formatter::ColorFormatter,
    content: Content,
}

#[derive(Default)]
struct Content {
    nodes: Vec<CommitNode>,
}

impl MyApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_custom_style(&cc.egui_ctx);
        load().unwrap()
    }
}

const GRAPH_CELL_SIZE: Vec2 = Vec2::new(16.0, 16.0);
const GRAPH_STROKE: Stroke = Stroke {
    width: 1.,
    color: Color32::WHITE,
};

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let content = std::mem::take(&mut self.content);

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut prev_link_line = None;
            let mut first = true;
            for node in &content.nodes {
                self.draw_line_row(ui, &node, prev_link_line, first);

                if let Some(link_row) = &node.row.link_line {
                    self.draw_line_link(ui, link_row);
                }

                if let Some(term_row) = &node.row.term_line {
                    let (response, painter) = ui.allocate_painter(
                        GRAPH_CELL_SIZE * Vec2::new(term_row.len() as f32, 1.0),
                        egui::Sense::empty(),
                    );

                    for (i, _) in term_row.iter().enumerate() {
                        let rect = rect_subdiv_x(response.rect, term_row.len(), i);

                        for i in 0..4 {
                            let pos = rect.center_top() + Vec2::DOWN * i as f32 * 3.0;
                            painter.circle_filled(pos + Vec2::X * 0.25, 0.5, GRAPH_STROKE.color);
                        }
                    }
                }

                prev_link_line = node.row.link_line.as_deref();
                first = false;
            }
        });

        self.content = content;
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(ctx.used_size()));
    }
}
impl MyApp {
    fn draw_line_row(
        &mut self,
        ui: &mut egui::Ui,
        node: &CommitNode,
        prev_link_line: Option<&[LinkLine]>,
        first: bool,
    ) {
        let node_line = &node.row.node_line;

        let style = ui.style_mut();
        style.spacing.item_spacing = Vec2::ZERO;
        style.spacing.interact_size = Vec2::ZERO;

        ui.horizontal(|ui| {
            ui.reset_style();

            let (response, painter) = ui.allocate_painter(
                GRAPH_CELL_SIZE * Vec2::new(node_line.len() as f32, 1.0),
                egui::Sense::empty(),
            );
            for (i, line) in node_line.iter().enumerate() {
                let rect = rect_subdiv_x(response.rect, node_line.len(), i);
                if let NodeLine::Blank = line {
                    continue;
                }

                let is_head = match prev_link_line.and_then(|l| l.get(i)) {
                    None if first => true,
                    None => false,
                    Some(link_line) => !link_line.intersects(LinkLine::ANY_FORK | LinkLine::VERT_PARENT),
                };

                if is_head {
                    painter.line_segment([rect.center(), rect.center_bottom()], GRAPH_STROKE);
                } else {
                    painter.line_segment([rect.center_top(), rect.center_bottom()], GRAPH_STROKE);
                }
                if let NodeLine::Node = line {
                    painter.circle_filled(rect.center() + Vec2::X * 0.25, 3.0, GRAPH_STROKE.color);
                }
            }

            ui.dnd_drag_source(egui::Id::new(&node.commit_id), node.commit_id.clone(), |ui| {
                node.msg.replay(&mut self.formatter).unwrap();
                ui.label(self.formatter.take());
            })
        });
    }

    fn draw_line_link(&mut self, ui: &mut egui::Ui, link_row: &[LinkLine]) {
        let (response, painter) = ui.allocate_painter(
            GRAPH_CELL_SIZE * Vec2::new(link_row.len() as f32, 1.0),
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
                painter.line_segment([rect.center_top(), rect.center_bottom()], GRAPH_STROKE);
            }
            if cur.intersects(LinkLine::RIGHT_FORK) {
                painter.add(bezier(
                    next_rect.center_top(),
                    rect.center_bottom(),
                    Vec2::Y * GRAPH_CELL_SIZE.y * 0.8,
                ));
            }
            if cur.intersects(LinkLine::RIGHT_MERGE) {
                painter.add(bezier(
                    rect.center_top(),
                    next_rect.center_bottom(),
                    Vec2::Y * GRAPH_CELL_SIZE.y * 0.8,
                ));
            }
            if cur.intersects(LinkLine::LEFT_FORK) {
                painter.add(bezier(
                    first_rect.center_top(),
                    rect.center_bottom(),
                    Vec2::Y * GRAPH_CELL_SIZE.y * 0.8,
                ));
            }
            if cur.intersects(LinkLine::LEFT_MERGE) {}
        }
    }
}

fn convert_graph_edge_into_ancestor<K: Clone>(e: &GraphEdge<K>) -> Ancestor<K> {
    match e.edge_type {
        GraphEdgeType::Direct => Ancestor::Parent(e.target.clone()),
        GraphEdgeType::Indirect => Ancestor::Ancestor(e.target.clone()),
        GraphEdgeType::Missing => Ancestor::Anonymous,
    }
}

fn get_node_template(settings: &UserSettings) -> Result<Cow<'static, str>, ConfigGetError> {
    let symbol = settings.get_string("templates.log_node").optional()?;
    Ok(symbol.map(Cow::Owned).unwrap_or(Cow::Borrowed("builtin_log_node")))
}

fn rect_subdiv_x(rect: Rect, n_x: usize, i: usize) -> Rect {
    let w = rect.width() / n_x as f32;
    Rect::from_min_size(
        Pos2::new(rect.min.x + w * i as f32, rect.min.y),
        Vec2::new(w, rect.height()),
    )
}

fn bezier(from: Pos2, to: Pos2, delta: Vec2) -> CubicBezierShape {
    CubicBezierShape {
        points: [from, from + delta, to - delta, to],
        closed: false,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke {
            width: GRAPH_STROKE.width,
            color: ColorMode::Solid(GRAPH_STROKE.color),
            kind: StrokeKind::Middle,
        },
    }
}
