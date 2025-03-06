#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use std::path::Path;

use anyhow::Result;
use eframe::egui::{self, Color32, Theme};
use egui::TextFormat;
use egui::text::LayoutJob;
use jj_cli::formatter::{FormatRecorder, PlainTextFormatter};
use jj_lib::backend::CommitId;
use jj_lib::graph::{GraphEdge, GraphEdgeType, TopoGroupedGraphIterator};
use renderdag::{Ancestor, GraphRow, GraphRowRenderer, LinkLine, Renderer};

mod egui_formatter;
mod jj;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([400.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native("kahva", options, Box::new(|cc| Ok(Box::new(MyApp::new(cc)))))
}

fn setup_custom_style(ctx: &egui::Context) {
    ctx.style_mut_of(Theme::Dark, |style| {
        style.visuals.panel_fill = Color32::from_rgb(43, 43, 46);
    });
}

struct CommitNode {
    commit_id: CommitId,
    msg: FormatRecorder,
    row: GraphRow<(CommitId, bool)>,
}

fn load() -> Result<Vec<CommitNode>> {
    let repo = jj::Repo::detect(Path::new("/home/jakob/.personal/contrib/jj"))?.unwrap();
    let log_revset = repo.settings().get_string("revsets.log")?;

    let prio_revset = repo.settings().get_string("revsets.log-graph-prioritize")?;
    let prio_revset = repo.revset_expression(&prio_revset)?;

    let log_template = repo.settings_commit_template("templates.log")?;
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

        // let within_graph = with_content_format.sub_width(graph.width(&key, &graphlog_edges));
        /*within_graph.write(ui.new_formatter(&mut buffer).as_mut(), |formatter| {
            template.format(&commit, formatter)
        })?;
        if !buffer.ends_with(b"\n") {
            buffer.push(b'\n');
        }*/
        /*if let Some(renderer) = &diff_renderer {
            let mut formatter = ui.new_formatter(&mut buffer);
            renderer.show_patch(
                ui,
                formatter.as_mut(),
                &commit,
                matcher.as_ref(),
                within_graph.width(),
            )?;
        }*/

        // let node_symbol = format_template(ui, &Some(commit), &node_template);
        let node_symbol = "x";
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

    Ok(nodes)
}

struct MyApp {
    nodes: Vec<CommitNode>,
}

impl MyApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_custom_style(&cc.egui_ctx);

        Self { nodes: load().unwrap() }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            for node in &self.nodes {
                ui.horizontal(|ui| {
                    for line in &node.row.node_line {
                        let l = match line {
                            renderdag::NodeLine::Blank => " ",
                            renderdag::NodeLine::Ancestor => ".",
                            renderdag::NodeLine::Parent => "| ",
                            renderdag::NodeLine::Node => "o",
                        };
                        ui.label(l);
                    }

                    ui.dnd_drag_source(egui::Id::new(&node.commit_id), "a", |ui| {
                        let mut out = Vec::new();
                        let mut f = PlainTextFormatter::new(&mut out);
                        node.msg.replay(&mut f).unwrap();
                        let msg = String::from_utf8(out).unwrap();
                        // ui.label(msg);

                        let mut text = LayoutJob::default();
                        text.append(&msg, 0.0, TextFormat::default());
                        ui.label(text)
                    });
                });

                let out = &mut String::new();
                if let Some(link_row) = &node.row.link_line {
                    let mut link_line = String::new();
                    let any_horizontal = link_row.iter().any(|cur| cur.intersects(LinkLine::HORIZONTAL));
                    let mut iter = link_row
                        .iter()
                        .copied()
                        .chain(std::iter::once(LinkLine::empty()))
                        .peekable();
                    while let Some(cur) = iter.next() {
                        let next = match iter.peek() {
                            Some(&v) => v,
                            None => break,
                        };
                        // Draw the parent/ancestor line.
                        if cur.intersects(LinkLine::HORIZONTAL) {
                            if cur.intersects(LinkLine::CHILD | LinkLine::ANY_FORK_OR_MERGE) {
                                link_line.push('+');
                            } else {
                                link_line.push('-');
                            }
                        } else if cur.intersects(LinkLine::VERTICAL) {
                            if cur.intersects(LinkLine::ANY_FORK_OR_MERGE) && any_horizontal {
                                link_line.push('+');
                            } else if cur.intersects(LinkLine::VERT_PARENT) {
                                link_line.push('|');
                            } else {
                                link_line.push('.');
                            }
                        } else if cur.intersects(LinkLine::ANY_MERGE) && any_horizontal {
                            link_line.push('\'');
                        } else if cur.intersects(LinkLine::ANY_FORK) && any_horizontal {
                            link_line.push('.');
                        } else {
                            link_line.push(' ');
                        }

                        // Draw the connecting line.
                        if cur.intersects(LinkLine::HORIZONTAL) {
                            link_line.push('-');
                        } else if cur.intersects(LinkLine::RIGHT_MERGE) {
                            if next.intersects(LinkLine::LEFT_FORK) && !any_horizontal {
                                link_line.push('\\');
                            } else {
                                link_line.push('-');
                            }
                        } else if cur.intersects(LinkLine::RIGHT_FORK) {
                            if next.intersects(LinkLine::LEFT_MERGE) && !any_horizontal {
                                link_line.push('/');
                            } else {
                                link_line.push('-');
                            }
                        } else {
                            link_line.push(' ');
                        }
                    }
                    /*if let Some(msg) = message_lines.next() {
                        link_line.push(' ');
                        link_line.push_str(msg);
                    }*/
                    out.push_str(link_line.trim_end());
                    if !out.is_empty() {
                        ui.label(std::mem::take(out));
                    }
                }
            }
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(ctx.used_size()));
    }
}
fn convert_graph_edge_into_ancestor<K: Clone>(e: &GraphEdge<K>) -> Ancestor<K> {
    match e.edge_type {
        GraphEdgeType::Direct => Ancestor::Parent(e.target.clone()),
        GraphEdgeType::Indirect => Ancestor::Ancestor(e.target.clone()),
        GraphEdgeType::Missing => Ancestor::Anonymous,
    }
}
