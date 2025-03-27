use crate::Args;
use crate::jj::Repo;
use color_eyre::Result;
use jj_cli::formatter::{FormatRecorder, Formatter, PlainTextFormatter};
use jj_lib::backend::CommitId;
use jj_lib::config::{ConfigGetError, ConfigGetResultExt};
use jj_lib::graph::{GraphEdge, GraphEdgeType, TopoGroupedGraphIterator};
use jj_lib::settings::UserSettings;
use renderdag::{Ancestor, GraphRow, GraphRowRenderer, Renderer};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;

pub struct CommitNode {
    pub commit_id: Option<CommitId>,
    pub msg: FormatRecorder,
    pub row: GraphRow<(CommitId, bool)>,
}

#[derive(Default)]
pub struct RepoView {
    pub nodes: Vec<CommitNode>,
    #[expect(dead_code)]
    pub parents: HashMap<CommitId, Vec<CommitId>>,
    pub heads: Vec<CommitId>,
}

pub fn reload(repo: &Repo, args: &Args) -> Result<RepoView> {
    let log_revset = match &args.revisions {
        Some(revset) => revset,
        None => &repo
            .settings()
            .get_string("revsets.kahva-log")
            .optional()
            .transpose()
            .unwrap_or_else(|| repo.settings().get_string("revsets.log"))?,
    };

    let prio_revset = repo
        .settings()
        .get_string("revsets.log-graph-prioritize")
        .optional()?
        .unwrap_or_else(|| "present(@)".to_owned());
    let prio_revset = repo.revset_expression(&prio_revset)?;

    // let log_template = repo.settings_commit_template("templates.log")?;
    let log_template = repo.parse_commit_template("builtin_log_oneline")?;
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

    let mut parents: HashMap<CommitId, Vec<CommitId>> = HashMap::default();

    for node in iter {
        let (commit_id, edges) = node?;
        parents
            .entry(commit_id.clone())
            .or_default()
            .extend(edges.iter().map(|edge| edge.target.clone()));

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
        let key = (commit_id.clone(), false);
        let commit = repo.commit(&key.0)?;

        let mut node_out = Vec::new();
        let mut f = PlainTextFormatter::new(&mut node_out);
        node_template.format(&Some(commit.clone()), &mut f)?;
        let _node_symbol = String::from_utf8(node_out)?;
        let node_symbol = "o";

        let row = graph.next_row(
            key,
            graphlog_edges.iter().map(convert_graph_edge_into_ancestor).collect(),
            node_symbol.into(),
            String::new(),
        );
        let mut f = FormatRecorder::new();
        log_template.format(&commit, &mut f)?;
        nodes.push(CommitNode {
            commit_id: Some(commit_id.clone()),
            msg: f,
            row,
        });

        for elided_target in elided_targets {
            let elided_key = (elided_target.clone(), true);
            let real_key = (elided_key.0.clone(), false);
            let edges = [GraphEdge::direct(real_key)];

            let mut node_out = Vec::new();
            let mut f = PlainTextFormatter::new(&mut node_out);
            node_template.format(&Some(commit.clone()), &mut f)?;
            let _node_symbol = String::from_utf8(node_out)?;
            let node_symbol = "o";

            let edges = edges.iter().map(convert_graph_edge_into_ancestor).collect();
            let row = graph.next_row(
                elided_key,
                edges,
                node_symbol.to_owned(),
                "(elided revisions)".to_owned(),
            );
            let mut f = FormatRecorder::new();
            f.push_label("elided")?;
            f.write_all(b"(elided revisions)")?;
            f.pop_label()?;
            nodes.push(CommitNode {
                commit_id: None,
                msg: f,
                row,
            });
        }
    }

    let heads = parents
        .keys()
        .filter(|&commit| !parents.values().flatten().any(|x| x == commit))
        .cloned()
        .collect();

    Ok(RepoView { nodes, parents, heads })
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
