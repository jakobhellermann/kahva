#![allow(unused)]

use chrono::TimeZone as _;
use futures_executor::block_on_stream;
use jj_cli::commit_templater::{CommitTemplateLanguage, CommitTemplateLanguageExtension};
use jj_cli::config::{ConfigEnv, config_from_environment, default_config_layers};
use jj_cli::diff_util::{self, UnifiedDiffOptions, show_diff_summary};
use jj_cli::formatter::Formatter;
use jj_cli::revset_util::{self, RevsetExpressionEvaluator};
use jj_cli::template_builder::{self, TemplateLanguage};
use jj_cli::template_parser::{TemplateAliasesMap, TemplateDiagnostics};
use jj_cli::templater::{PropertyPlaceholder, TemplateRenderer};
use jj_lib::annotate::FileAnnotation;
use jj_lib::commit::Commit;
use jj_lib::config::{ConfigGetError, ConfigGetResultExt, ConfigNamePathBuf, StackedConfig};
use jj_lib::conflicts::{ConflictMarkerStyle, MaterializedTreeDiffEntry, materialized_diff_stream};
use jj_lib::copies::CopyRecords;
use jj_lib::id_prefix::IdPrefixContext;
use jj_lib::matchers::{EverythingMatcher, Matcher};
use jj_lib::merged_tree::MergedTree;
use jj_lib::repo::{ReadonlyRepo, Repo as _, StoreFactories};
use jj_lib::repo_path::RepoPathUiConverter;
use jj_lib::revset::{
    self, RevsetAliasesMap, RevsetDiagnostics, RevsetExpression, RevsetExtensions, RevsetIteratorExt, RevsetModifier,
    RevsetParseContext, RevsetWorkspaceContext, UserRevsetExpression,
};
use jj_lib::settings::UserSettings;
use jj_lib::workspace::Workspace;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Result, anyhow, ensure};
use jj_lib::backend::CommitId;

pub struct Repo {
    workspace: Workspace,
    repo: Arc<ReadonlyRepo>,
    settings: UserSettings,

    id_prefix_context: IdPrefixContext,
    path_converter: RepoPathUiConverter,

    revset_aliases_map: RevsetAliasesMap,
    revset_extensions: Arc<RevsetExtensions>,
    template_aliases_map: TemplateAliasesMap,

    immutable_heads_expression: Rc<UserRevsetExpression>,
}

pub struct DiffState<'a> {
    repo: &'a Repo,
    copy_records: CopyRecords,
    from_tree: MergedTree,
    to_tree: MergedTree,
}

impl Repo {
    fn find_root(path: &Path) -> Option<&Path> {
        path.ancestors().find(|path| path.join(".jj").is_dir())
    }

    pub fn workspace_dir(&self) -> &Path {
        self.workspace.workspace_root()
    }

    pub fn detect_cwd() -> Result<Option<Repo>> {
        Repo::detect(&std::env::current_dir()?)
    }

    pub fn detect(cwd: &Path) -> Result<Option<Repo>> {
        let Some(workspace_dir) = Repo::find_root(cwd) else {
            return Ok(None);
        };

        let config_env = ConfigEnv::from_environment();
        let mut config = config_from_environment(default_config_layers());
        // TODO(config): workspace loader
        config_env.reload_user_config(&mut config)?;
        let config = config_env.resolve_config(&config)?;

        let settings = UserSettings::from_config(config)?;
        let working_copy_factories = jj_lib::workspace::default_working_copy_factories();
        let workspace = Workspace::load(
            &settings,
            workspace_dir,
            &StoreFactories::default(),
            &working_copy_factories,
        )?;
        let repo = workspace.repo_loader().load_at_head()?;
        let path_converter = RepoPathUiConverter::Fs {
            cwd: workspace.workspace_root().to_owned(),
            base: workspace.workspace_root().to_owned(),
        };

        let revset_aliases_map = load_revset_aliases(settings.config())?;
        #[allow(clippy::arc_with_non_send_sync)]
        let revset_extensions = Arc::new(RevsetExtensions::new());
        // TODO(config): user disambiguator
        let id_prefix_context = IdPrefixContext::new(Arc::clone(&revset_extensions));
        id_prefix_context.populate(repo.base_repo())?;

        let template_aliases_map = load_template_aliases(settings.config())?;

        let mut this = Repo {
            repo,
            workspace,
            settings,
            path_converter,
            id_prefix_context,
            revset_aliases_map,
            revset_extensions,
            template_aliases_map,
            immutable_heads_expression: RevsetExpression::root(),
        };

        this.immutable_heads_expression =
            revset_util::parse_immutable_heads_expression(&mut RevsetDiagnostics::new(), &this.revset_parse_context())?;

        this.id_prefix_context = match this.load_short_prefixes_expression()? {
            Some(x) => this.id_prefix_context.disambiguate_within(x),
            None => this.id_prefix_context,
        };

        Ok(Some(this))
    }

    pub fn settings(&self) -> &UserSettings {
        &self.settings
    }
    pub fn inner(&self) -> &dyn jj_lib::repo::Repo {
        self.repo.as_ref()
    }

    pub fn write_log(&self, f: &mut dyn Formatter, commit: &Commit) -> Result<()> {
        let language = self.commit_template_language();
        let template_string = self.settings.get_string("templates.log")?;
        let template = self.parse_template(&language, &template_string, CommitTemplateLanguage::wrap_commit)?;

        template.format(commit, f)?;

        Ok(())
    }

    pub fn settings_commit_template(&self, settings_path: &'static str) -> Result<TemplateRenderer<'_, Commit>> {
        let language = self.commit_template_language();
        let annotate_commit_summary_text = self.settings.get_string(settings_path)?;
        let template = self.parse_template(
            &language,
            &annotate_commit_summary_text,
            CommitTemplateLanguage::wrap_commit,
        )?;

        Ok(template)
    }

    pub fn annotation(&self, starting_commit: &Commit, file_path: &str) -> Result<FileAnnotation> {
        let file_path = self.path_converter.parse_file_path(file_path)?;
        let file_value = starting_commit.tree()?.path_value(&file_path)?;
        let ui_path = self.path_converter.format_file_path(&file_path);
        if file_value.is_absent() {
            return Err(anyhow!("Path does not belong to repository: {ui_path}"));
        }
        if file_value.is_tree() {
            return Err(anyhow!("Path exists but is not a regular file: {ui_path}"));
        }

        let domain = RevsetExpression::all();
        let annotation =
            jj_lib::annotate::get_annotation_for_file(self.repo.as_ref(), starting_commit, &domain, &file_path)?;

        Ok(annotation)
    }

    pub fn log(&self) -> Result<Vec<Commit>> {
        let revset_string = self.settings.get_string("revsets.log")?;
        let revset = self.revset_expression(&revset_string)?.evaluate()?;

        let commits = revset
            .iter()
            .commits(self.repo.store())
            .collect::<Result<Vec<_>, _>>()?;

        Ok(commits)
    }

    pub fn revset_expression(&self, revset_string: &str) -> Result<RevsetExpressionEvaluator<'_>> {
        let mut diagnostics = RevsetDiagnostics::new();
        let context = self.revset_parse_context();
        let (expression, modifier) = revset::parse_with_modifier(&mut diagnostics, revset_string, &context)?;
        let (None | Some(RevsetModifier::All)) = modifier;

        ensure!(diagnostics.is_empty());

        let evaluator = RevsetExpressionEvaluator::new(
            self.repo.as_ref(),
            Arc::clone(&self.revset_extensions),
            &self.id_prefix_context,
            expression,
        );

        Ok(evaluator)
    }

    pub fn revset_single(&self, revset_string: &str) -> Result<Commit> {
        let expression = self.revset_expression(revset_string)?;
        let commit = evaluate_revset_to_single_commit(revset_string, &expression)?;
        Ok(commit)
    }

    pub fn commit(&self, commit_id: &CommitId) -> Result<Commit> {
        let commit = self.repo.store().get_commit(commit_id)?;
        Ok(commit)
    }
    pub fn current_commit(&self) -> Result<Commit> {
        let commit_id = self
            .repo
            .view()
            .get_wc_commit_id(self.workspace.workspace_id())
            .ok_or_else(|| anyhow!("workspace has no checked out commit"))?;
        let commit = self.repo.store().get_commit(commit_id)?;

        Ok(commit)
    }

    pub fn diff(&self, commit: &Commit) -> Result<DiffState<'_>> {
        let from_tree = commit.parent_tree(self.repo.as_ref())?;
        let to_tree = commit.tree()?;

        let mut copy_records = CopyRecords::default();
        for parent_id in commit.parent_ids() {
            let records = diff_util::get_copy_records(self.repo.store(), parent_id, commit.id(), &EverythingMatcher)?;
            copy_records.add_records(records)?;
        }

        Ok(DiffState {
            repo: self,
            copy_records,
            from_tree,
            to_tree,
        })
    }

    pub fn path_converter(&self) -> &RepoPathUiConverter {
        &self.path_converter
    }
}

impl DiffState<'_> {
    pub fn diff(&self, matcher: &dyn Matcher) -> Result<Vec<MaterializedTreeDiffEntry>> {
        let diff = self
            .from_tree
            .diff_stream_with_copies(&self.to_tree, matcher, &self.copy_records);
        let diff = block_on_stream(materialized_diff_stream(self.repo.repo.store(), diff)).collect::<Vec<_>>();

        Ok(diff)
    }

    pub fn write_summary(&self, f: &mut dyn Formatter) -> Result<()> {
        let diff = self
            .from_tree
            .diff_stream_with_copies(&self.to_tree, &EverythingMatcher, &self.copy_records);

        show_diff_summary(f, diff, &self.repo.path_converter)?;

        Ok(())
    }

    pub fn write_diff(&self, f: &mut dyn Formatter, matcher: &dyn Matcher) -> Result<()> {
        let diff = self
            .from_tree
            .diff_stream_with_copies(&self.to_tree, matcher, &self.copy_records);

        jj_cli::diff_util::show_git_diff(
            f,
            self.repo.repo.store(),
            diff,
            &UnifiedDiffOptions {
                context: 3,
                line_diff: diff_util::LineDiffOptions {
                    compare_mode: diff_util::LineCompareMode::IgnoreAllSpace,
                },
            },
            ConflictMarkerStyle::Git,
        )?;

        Ok(())
    }
}

impl Repo {
    fn commit_template_language(&self) -> CommitTemplateLanguage<'_> {
        CommitTemplateLanguage::new(
            self.repo.as_ref(),
            &self.path_converter,
            self.workspace.workspace_id(),
            self.revset_parse_context(),
            &self.id_prefix_context,
            self.immutable_expression(),
            ConflictMarkerStyle::Git,
            // self.conflict_marker_style, TODO(config)
            // &self.command.data.commit_template_extensions,
            &[] as &[Arc<dyn CommitTemplateLanguageExtension>],
        )
    }

    fn immutable_expression(&self) -> Rc<UserRevsetExpression> {
        // Negated ancestors expression `~::(<heads> | root())` is slightly
        // easier to optimize than negated union `~(::<heads> | root())`.
        self.immutable_heads_expression.ancestors()
    }

    pub fn parse_template<'a, C: Clone + 'a, L: TemplateLanguage<'a> + ?Sized>(
        &self,
        language: &L,
        template_text: &str,
        wrap_self: impl Fn(PropertyPlaceholder<C>) -> L::Property,
    ) -> Result<TemplateRenderer<'a, C>> {
        let mut diagnostics = TemplateDiagnostics::new();
        let template = template_builder::parse(
            language,
            &mut diagnostics,
            template_text,
            &self.template_aliases_map,
            wrap_self,
        )?;
        ensure!(diagnostics.len() == 0);
        Ok(template)
    }
    fn revset_parse_context(&self) -> RevsetParseContext<'_> {
        let workspace_context = RevsetWorkspaceContext {
            path_converter: &self.path_converter,
            workspace_id: self.workspace.workspace_id(),
        };

        let now = if let Some(timestamp) = self.settings.commit_timestamp() {
            chrono::Local.timestamp_millis_opt(timestamp.timestamp.0).unwrap()
        } else {
            chrono::Local::now()
        };

        RevsetParseContext::new(
            &self.revset_aliases_map,
            self.settings.user_email(),
            now.into(),
            &self.revset_extensions,
            Some(workspace_context),
        )
    }

    fn load_short_prefixes_expression(&self) -> Result<Option<Rc<UserRevsetExpression>>> {
        let revset_string = self
            .settings
            .get_string("revsets.short-prefixes")
            .optional()?
            .map_or_else(|| self.settings.get_string("revsets.log"), Ok)?;
        if revset_string.is_empty() {
            Ok(None)
        } else {
            let mut diagnostics = RevsetDiagnostics::new();
            let (expression, modifier) =
                revset::parse_with_modifier(&mut diagnostics, &revset_string, &self.revset_parse_context())
                    .map_err(|err| anyhow::anyhow!("Invalid `revsets.short-prefixes`: {}", err))?;
            let (None | Some(RevsetModifier::All)) = modifier;
            Ok(Some(expression))
        }
    }
}

fn load_revset_aliases(stacked_config: &StackedConfig) -> Result<RevsetAliasesMap> {
    let table_name = ConfigNamePathBuf::from_iter(["revset-aliases"]);
    let mut aliases_map = RevsetAliasesMap::new();
    // Load from all config layers in order. 'f(x)' in default layer should be
    // overridden by 'f(a)' in user.
    for layer in stacked_config.layers() {
        let table = match layer.look_up_table(&table_name) {
            Ok(Some(table)) => table,
            Ok(None) => continue,
            Err(item) => {
                return Err(ConfigGetError::Type {
                    name: table_name.to_string(),
                    error: format!("Expected a table, but is {}", item.type_name()).into(),
                    source_path: layer.path.clone(),
                }
                .into());
            }
        };
        for (decl, item) in table.iter() {
            let _ = item
                .as_str()
                .ok_or_else(|| format!("Expected a string, but is {}", item.type_name()))
                .and_then(|v| aliases_map.insert(decl, v).map_err(|e| e.to_string()));
        }
    }
    Ok(aliases_map)
}

fn load_template_aliases(stacked_config: &StackedConfig) -> Result<TemplateAliasesMap> {
    let table_name = ConfigNamePathBuf::from_iter(["template-aliases"]);
    let mut aliases_map = TemplateAliasesMap::new();
    // Load from all config layers in order. 'f(x)' in default layer should be
    // overridden by 'f(a)' in user.
    for layer in stacked_config.layers() {
        let table = match layer.look_up_table(&table_name) {
            Ok(Some(table)) => table,
            Ok(None) => continue,
            Err(item) => {
                return Err(ConfigGetError::Type {
                    name: table_name.to_string(),
                    error: format!("Expected a table, but is {}", item.type_name()).into(),
                    source_path: layer.path.clone(),
                }
                .into());
            }
        };
        for (decl, item) in table.iter() {
            let _ = item
                .as_str()
                .ok_or_else(|| format!("Expected a string, but is {}", item.type_name()))
                .and_then(|v| aliases_map.insert(decl, v).map_err(|e| e.to_string()));
        }
    }
    Ok(aliases_map)
}

pub(super) fn evaluate_revset_to_single_commit(
    revision_str: &str,
    expression: &RevsetExpressionEvaluator<'_>,
) -> Result<Commit> {
    let mut iter = expression.evaluate_to_commits()?.fuse();
    match (iter.next(), iter.next()) {
        (Some(commit), None) => Ok(commit?),
        (None, _) => Err(anyhow!(r#"Revset "{revision_str}" didn't resolve to any revisions"#)),
        (Some(_), Some(_)) => Err(anyhow!("{revision_str} resolved to multiple commits")),
    }
}
