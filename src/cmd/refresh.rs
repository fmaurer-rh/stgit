// SPDX-License-Identifier: GPL-2.0-only

//! `stg refresh` implementation.

use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{anyhow, Result};
use bstr::ByteSlice;
use clap::{Arg, ArgGroup, ArgMatches, ValueHint};
use indexmap::IndexSet;

use crate::{
    color::get_color_stdout,
    commit::{CommitMessage, RepositoryCommitExtended},
    hook::run_pre_commit_hook,
    index::TemporaryIndex,
    patchedit,
    patchname::PatchName,
    pathspec,
    signature::SignatureExtended,
    stack::{Error, Stack, StackStateAccess},
    stupid::Stupid,
};

pub(super) fn get_command() -> (&'static str, super::StGitCommand) {
    (
        "refresh",
        super::StGitCommand {
            make,
            run,
            category: super::CommandCategory::PatchManipulation,
        },
    )
}

fn make() -> clap::Command<'static> {
    let app = clap::Command::new("refresh")
        .about("Incorporate worktree changes into current patch")
        .long_about(
            "Include the latest work tree and index changes in the \
             current patch. This command generates a new git commit \
             object for the patch; the old commit is no longer visible.\n\
             \n\
             Refresh will warn if the index is dirty, and require use of \
             either the '--index' or '--force' options to override this \
             check. This is to prevent accidental full refresh when only \
             some changes were staged using git add interative mode.\n\
             \n\
             You may optionally list one or more files or directories \
             relative to the current working directory; if you do, only \
             matching files will be updated.\n\
             \n\
             Behind the scenes, stg refresh first creates a new \
             temporary patch with your updates, and then merges that \
             patch into the patch you asked to have refreshed. If you \
             asked to refresh a patch other than the topmost patch, \
             there can be conflicts; in that case, the temporary patch \
             will be left for you to take care of, for example with stg \
             squash.\n\
             \n\
             The creation of the temporary patch is recorded in a \
             separate entry in the patch stack log; this means that one \
             undo step will undo the merge between the other patch and \
             the temp patch, and two undo steps will additionally get \
             rid of the temp patch.",
        )
        .arg(
            Arg::new("pathspecs")
                .help("Only refresh files matching path")
                .value_name("path")
                .multiple_values(true)
                .allow_invalid_utf8(true),
        )
        .next_help_heading("REFRESH OPTIONS")
        .arg(
            Arg::new("update")
                .long("update")
                .short('u')
                .help("Only update the current patch files"),
        )
        .arg(
            Arg::new("index")
                .long("index")
                .short('i')
                .help("Refresh from index instead of worktree")
                .long_help(
                    "Instead of setting the patch top to the current \
                     contents of the worktree, set it to the current \
                     contents of the index.",
                )
                .conflicts_with_all(&["pathspecs", "update", "submodules", "force"]),
        )
        .arg(
            Arg::new("force")
                .long("force")
                .short('F')
                .help("Force refresh even if index is dirty")
                .long_help(
                    "Instead of warning the user when some work has \
                     already been staged (such as with git add \
                     interactive mode) force a full refresh.",
                ),
        )
        .arg(
            Arg::new("patch")
                .long("patch")
                .short('p')
                .help("Refresh (applied) PATCH instead of the top patch")
                .takes_value(true)
                .value_name("PATCH")
                .value_hint(ValueHint::Other)
                .validator(PatchName::from_str),
        )
        .arg(
            Arg::new("annotate")
                .long("annotate")
                .short('a')
                .help("Annotate the patch log entry with NOTE")
                .takes_value(true)
                .value_name("NOTE")
                .value_hint(ValueHint::Other),
        )
        .arg(
            Arg::new("submodules")
                .long("submodules")
                .short('s')
                .help("Include submodules in patch content")
                .conflicts_with_all(&["update"]),
        )
        .arg(
            Arg::new("no-submodules")
                .long("no-submodules")
                .help("Exclude submodules in patch content"),
        )
        .group(ArgGroup::new("submodule-group").args(&["submodules", "no-submodules"]))
        .arg(
            Arg::new("spill")
                .long("spill")
                .help("OBSOLETE: use 'stg spill'")
                .hide(true),
        );

    patchedit::add_args(app, true, false)
}

fn run(matches: &ArgMatches) -> Result<()> {
    if matches.is_present("spill") {
        return Err(anyhow!(
            "`stg refresh --spill` is obsolete; use `stg spill` instead"
        ));
    }

    let repo = git2::Repository::open_from_env()?;
    let stack = Stack::from_branch(&repo, None)?;
    let config = repo.config()?;

    stack.check_head_top_mismatch()?;

    let patchname = if let Some(patchname) = matches
        .value_of("patch")
        .map(|s| PatchName::from_str(s).expect("clap already validated"))
    {
        if stack.has_patch(&patchname) {
            patchname
        } else {
            return Err(anyhow!("Patch `{patchname}` does not exist"));
        }
    } else if let Some(top_patchname) = stack.applied().last() {
        top_patchname.clone()
    } else {
        return Err(Error::NoAppliedPatches.into());
    };

    let tree_id = assemble_refresh_tree(
        &stack,
        &config,
        matches,
        matches.is_present("update").then(|| &patchname),
    )?;

    let mut log_msg = "refresh ".to_string();
    let opt_annotate = matches.value_of("annotate");

    // Make temp patch
    let temp_commit_id = stack.repo.commit_ex(
        &git2::Signature::make_author(Some(&config), matches)?,
        &git2::Signature::default_committer(Some(&config))?,
        &CommitMessage::from(format!("Refresh of {patchname}")),
        tree_id,
        [stack.branch_head.id()],
    )?;

    let temp_patchname = {
        let len_limit = None;
        let allow = vec![];
        let disallow: Vec<&PatchName> = stack.all_patches().collect();
        PatchName::make("refresh-temp", true, len_limit).uniquify(&allow, &disallow)
    };

    let stack = stack
        .setup_transaction()
        .with_output_stream(get_color_stdout(matches))
        .transact(|trans| trans.new_applied(&temp_patchname, temp_commit_id))
        .execute(&format!(
            "refresh {temp_patchname} (create temporary patch)"
        ))?;

    let mut absorb_success = false;
    stack
        .setup_transaction()
        .use_index_and_worktree(true)
        .with_output_stream(get_color_stdout(matches))
        .transact(|trans| {
            if let Some(pos) = trans.applied().iter().position(|pn| pn == &patchname) {
                // Absorb temp patch into already applied patch
                let to_pop = trans.applied()[pos + 1..].to_vec();
                if to_pop.len() > 1 {
                    let popped_extra = trans.pop_patches(|pn| to_pop.contains(pn))?;
                    assert!(
                        popped_extra.is_empty(),
                        "only requested patches should be popped"
                    );
                    trans.push_patches(&[&temp_patchname], false)?;
                }

                let temp_commit = trans.get_patch_commit(&temp_patchname);

                let mut to_pop = to_pop;
                let top_name = to_pop.pop();
                assert_eq!(top_name.as_ref(), Some(&temp_patchname));

                let (new_patchname, commit_id) = match patchedit::EditBuilder::default()
                    .original_patchname(Some(&patchname))
                    .existing_patch_commit(trans.get_patch_commit(&patchname))
                    .override_tree_id(temp_commit.tree_id())
                    .allow_diff_edit(false)
                    .allow_implicit_edit(false)
                    .allow_template_save(false)
                    .edit(trans, &repo, matches)?
                {
                    patchedit::EditOutcome::Committed {
                        patchname: new_patchname,
                        commit_id,
                    } => (new_patchname, commit_id),
                    patchedit::EditOutcome::TemplateSaved(_) => {
                        panic!("not allowed for refresh")
                    }
                };

                trans.delete_patches(|pn| pn == &temp_patchname)?;
                assert_eq!(Some(&patchname), trans.applied().last());
                trans.update_patch(&patchname, commit_id)?;
                if new_patchname != patchname {
                    trans.rename_patch(&patchname, &new_patchname)?;
                    log_msg.push_str(new_patchname.as_ref());
                } else {
                    log_msg.push_str(patchname.as_ref());
                }
                if let Some(annotation) = opt_annotate {
                    log_msg.push_str("\n\n");
                    log_msg.push_str(annotation);
                }

                trans.push_patches(&to_pop, false)?;
                absorb_success = true;
            } else {
                // Absorb temp patch into unapplied patch
                let popped_extra = trans.pop_patches(|pn| pn == &temp_patchname)?;
                assert!(popped_extra.is_empty());

                // Try to create the new tree of the refreshed patch.
                // This is the same as pushing the temp patch onto the target patch,
                // but without a worktree to spill conflicts to; so if the simple
                // merge fails, the refresh must be aborted.

                let patch_commit = trans.get_patch_commit(&patchname);
                let temp_commit = trans.get_patch_commit(&temp_patchname);
                let base = temp_commit.parent(0)?.tree_id();
                let ours = patch_commit.tree_id();
                let theirs = temp_commit.tree_id();

                if let Some(tree_id) = repo.with_temp_index_file(|temp_index| {
                    let stupid = repo.stupid();
                    let stupid_temp = stupid.with_index_path(temp_index.path().unwrap());
                    stupid_temp.read_tree(ours)?;
                    if stupid_temp.apply_treediff_to_index(base, theirs)? {
                        let tree_id = stupid_temp.write_tree()?;
                        Ok(Some(tree_id))
                    } else {
                        Ok(None)
                    }
                })? {
                    let (new_patchname, commit_id) = match patchedit::EditBuilder::default()
                        .original_patchname(Some(&patchname))
                        .existing_patch_commit(trans.get_patch_commit(&patchname))
                        .override_tree_id(tree_id)
                        .allow_diff_edit(false)
                        .allow_template_save(false)
                        .edit(trans, &repo, matches)?
                    {
                        patchedit::EditOutcome::Committed {
                            patchname: new_patchname,
                            commit_id,
                        } => (new_patchname, commit_id),
                        patchedit::EditOutcome::TemplateSaved(_) => {
                            panic!("not allowed for refresh")
                        }
                    };

                    trans.update_patch(&patchname, commit_id)?;
                    if new_patchname != patchname {
                        trans.rename_patch(&patchname, &new_patchname)?;
                        log_msg.push_str(new_patchname.as_ref());
                    } else {
                        log_msg.push_str(patchname.as_ref());
                    }
                    if let Some(annotation) = opt_annotate {
                        log_msg.push_str("\n\n");
                        log_msg.push_str(annotation);
                    }
                    trans.delete_patches(|pn| pn == &temp_patchname)?;
                    absorb_success = true;
                }
            }
            Ok(())
        })
        .execute(&log_msg)?;

    if !absorb_success {
        println!(
            "The new changes did not apply cleanly to {}. \
             They were saved in {}.",
            &patchname, &temp_patchname,
        );
    }

    Ok(())
}

fn determine_refresh_paths(
    repo: &git2::Repository,
    pathspecs: Option<clap::OsValues>,
    patch_commit: Option<&git2::Commit>,
    use_submodules: bool,
    force: bool,
) -> Result<IndexSet<PathBuf>> {
    let mut status_opts = git2::StatusOptions::new();
    status_opts.show(git2::StatusShow::IndexAndWorkdir);
    status_opts.exclude_submodules(!use_submodules);

    if let Some(pathspecs) = pathspecs {
        let workdir = repo.workdir().expect("not a bare repository");
        let curdir = std::env::current_dir()?;

        for pathspec in pathspecs {
            let norm_pathspec =
                pathspec::normalize_pathspec(workdir, &curdir, Path::new(pathspec))?;
            status_opts.pathspec(norm_pathspec);
        }
    }

    let mut refresh_paths: IndexSet<PathBuf> = repo
        .statuses(Some(&mut status_opts))?
        .iter()
        .map(|entry| PathBuf::from(path_from_bytes(entry.path_bytes())))
        .collect();

    if let Some(patch_commit) = patch_commit {
        // Restrict update to the paths that were already part of the patch.
        let patch_tree = patch_commit.tree()?;
        let parent_tree = patch_commit.parent(0)?.tree()?;
        let mut diff_opts = git2::DiffOptions::new();
        diff_opts.ignore_submodules(!use_submodules);
        diff_opts.force_binary(true); // Less expensive(?)

        let mut patch_paths: IndexSet<PathBuf> = IndexSet::new();

        repo.diff_tree_to_tree(Some(&parent_tree), Some(&patch_tree), Some(&mut diff_opts))?
            .foreach(
                &mut |delta, _| {
                    if let Some(old_path) = delta.old_file().path() {
                        patch_paths.insert(old_path.to_owned());
                    }
                    if let Some(new_path) = delta.new_file().path() {
                        patch_paths.insert(new_path.to_owned());
                    }
                    true
                },
                None,
                None,
                None,
            )?;

        // Set intersection to determine final subset of paths.
        refresh_paths.retain(|path| patch_paths.contains(path));
    }

    // Ensure no conflicts in the files to be refreshed.
    if repo
        .index()?
        .conflicts()?
        .filter_map(|maybe_entry| maybe_entry.ok())
        .any(|conflict| {
            if let (Some(our), Some(their)) = (&conflict.our, &conflict.their) {
                refresh_paths.contains(path_from_bytes(&our.path))
                    || (their.path != our.path
                        && refresh_paths.contains(path_from_bytes(&their.path)))
            } else if let Some(our) = conflict.our {
                refresh_paths.contains(path_from_bytes(&our.path))
            } else if let Some(their) = conflict.their {
                refresh_paths.contains(path_from_bytes(&their.path))
            } else {
                false
            }
        })
    {
        return Err(Error::OutstandingConflicts.into());
    }

    // Ensure worktree and index states are valid for the given options.
    // Forcing means changes will be taken from both the index and worktree.
    // If not forcing, all changes must be either in the index or worktree,
    // but not both.
    if !force {
        let mut status_opts = git2::StatusOptions::new();
        status_opts.show(git2::StatusShow::Index);
        status_opts.exclude_submodules(!use_submodules);
        let is_index_clean = repo.statuses(Some(&mut status_opts))?.is_empty();

        if !is_index_clean {
            let mut status_opts = git2::StatusOptions::new();
            status_opts.show(git2::StatusShow::Workdir);
            status_opts.exclude_submodules(!use_submodules);
            let is_worktree_clean = repo.statuses(Some(&mut status_opts))?.is_empty();

            if !is_worktree_clean {
                return Err(anyhow!(
                    "The index is dirty; consider using `--index` or `--force`",
                ));
            }
        }
    }

    Ok(refresh_paths)
}

pub(crate) fn assemble_refresh_tree(
    stack: &Stack,
    config: &git2::Config,
    matches: &ArgMatches,
    limit_to_patchname: Option<&PatchName>,
) -> Result<git2::Oid> {
    let repo = stack.repo;
    let opt_submodules = matches.is_present("submodules");
    let opt_nosubmodules = matches.is_present("no-submodules");
    let use_submodules = if !opt_submodules && !opt_nosubmodules {
        config.get_bool("stgit.refreshsubmodules").unwrap_or(false)
    } else {
        opt_submodules
    };
    let opt_pathspecs = matches.values_of_os("pathspecs");
    let is_path_limiting = limit_to_patchname.is_some() || opt_pathspecs.is_some();

    let refresh_paths = if matches.is_present("index") {
        // When refreshing from the index, no path limiting may be used.
        assert!(!is_path_limiting);
        IndexSet::new()
    } else {
        let maybe_patch_commit = limit_to_patchname.map(|pn| stack.get_patch_commit(pn));
        determine_refresh_paths(
            repo,
            opt_pathspecs,
            maybe_patch_commit,
            use_submodules,
            matches.is_present("force"),
        )?
    };

    let tree_id = {
        let paths: &IndexSet<PathBuf> = &refresh_paths;
        let mut default_index = stack.repo.index()?;

        // N.B. using temp index is necessary for the cases where there are conflicts in the
        // default index. I.e. by using a temp index, a subset of paths without conflicts
        // may be formed into a coherent tree while leaving the default index as-is.
        let tree_id_result = if is_path_limiting {
            let head_tree = stack.branch_head.tree()?;
            let tree_id_result = stack.repo.with_temp_index(|temp_index| {
                temp_index.read_tree(&head_tree)?;
                temp_index.add_all(paths, git2::IndexAddOption::DEFAULT, None)?;
                Ok(temp_index.write_tree()?)
            });

            default_index.update_all(paths, None)?;
            tree_id_result
        } else {
            if !paths.is_empty() {
                default_index.update_all(paths, None)?;
            }
            Ok(default_index.write_tree()?)
        };
        default_index.write()?;
        tree_id_result
    }?;

    let tree_id = if matches.is_present("no-verify") {
        tree_id
    } else {
        run_pre_commit_hook(repo, matches.is_present("edit"))?;
        // Re-read index from filesystem because pre-commit hook may have modified it
        let mut index = repo.index()?;
        index.read(false)?;
        index.write_tree()?
    };

    Ok(tree_id)
}

fn path_from_bytes(b: &[u8]) -> &Path {
    b.to_path().expect("paths on Windows must be utf8")
}
