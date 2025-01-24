use std::sync::{Arc, Mutex as StdMutex};
use chrono::{DateTime, TimeZone, Utc};
use tokio::sync::RwLock as ARwLock;
use std::path::PathBuf;
use url::Url;
use serde::{Serialize, Deserialize};
use tracing::error;
use git2::{Branch, DiffOptions, Oid, Repository, Signature, StatusOptions, Tree};

use crate::ast::chunk_utils::official_text_hashing_function;
use crate::custom_error::MapErrToString;
use crate::files_correction::get_active_workspace_folder;
use crate::global_context::GlobalContext;
use crate::agentic::generate_commit_message::generate_commit_message_by_diff;
use crate::files_correction::{serialize_path, deserialize_path};

#[derive(Serialize, Deserialize, Debug)]
pub struct CommitInfo {
    pub project_path: Url,
    pub commit_message: String,
    pub file_changes: Vec<FileChange>,
}
impl CommitInfo {
    pub fn get_project_name(&self) -> String {
        self.project_path.to_file_path().ok()
            .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "".to_string())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileChange {
    #[serde(serialize_with = "serialize_path", deserialize_with = "deserialize_path")]
    pub relative_path: PathBuf,
    #[serde(serialize_with = "serialize_path", deserialize_with = "deserialize_path")]
    pub absolute_path: PathBuf,
    pub status: FileChangeStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileDiff {
    pub file_change: FileChange,
    pub content_before: String,
    pub content_after: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum FileChangeStatus {
    ADDED,
    MODIFIED,
    DELETED,
}
impl FileChangeStatus {
    pub fn initial(&self) -> char {
        match self {
            FileChangeStatus::ADDED => 'A',
            FileChangeStatus::MODIFIED => 'M',
            FileChangeStatus::DELETED => 'D',
        }
    }
}

#[derive(Default, Serialize, Deserialize, Clone, Debug)]
pub struct Checkpoint {
    #[serde(serialize_with = "serialize_path", deserialize_with = "deserialize_path")]
    pub workspace_folder: PathBuf,
    pub commit_hash: String,
}

impl Checkpoint {
    pub fn workspace_hash(&self) -> String {
        official_text_hashing_function(&self.workspace_folder.to_string_lossy().to_string())
    }
}

pub fn git_ls_files(repository_path: &PathBuf) -> Option<Vec<PathBuf>> {
    let repository = Repository::open(repository_path)
        .map_err(|e| error!("Failed to open repository: {}", e)).ok()?;

    let mut status_options = StatusOptions::new();
    status_options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_unmodified(true)
        .exclude_submodules(false)
        .include_ignored(false)
        .recurse_ignored_dirs(false);

    let statuses = repository.statuses(Some(&mut status_options))
        .map_err(|e| error!("Failed to get statuses: {}", e)).ok()?;

    let mut files = Vec::new();
    for entry in statuses.iter() {
        let path = String::from_utf8_lossy(entry.path_bytes()).to_string();
        files.push(repository_path.join(path));
    }
    if !files.is_empty() { Some(files) } else { None }
}

/// Similar to git checkout -b <branch_name>
pub fn create_or_checkout_to_branch<'repo>(repository: &'repo Repository, branch_name: &str) -> Result<Branch<'repo>, String> {
    let branch = match repository.find_branch(branch_name, git2::BranchType::Local) {
        Ok(branch) => branch,
        Err(_) => {
            let head_commit = repository.head()
                .and_then(|h| h.peel_to_commit())
                .map_err(|e| format!("Failed to get HEAD commit: {}", e))?;
            repository.branch(branch_name, &head_commit, false)
                .map_err(|e| format!("Failed to create branch: {}", e))?
        }
    };

    // Checkout to the branch
    let object = repository.revparse_single(&("refs/heads/".to_owned() + branch_name))
        .map_err(|e| format!("Failed to revparse single: {}", e))?;
    repository.checkout_tree(&object, None)
        .map_err(|e| format!("Failed to checkout tree: {}", e))?;
    repository.set_head(&format!("refs/heads/{}", branch_name))
      .map_err(|e| format!("Failed to set head: {}", e))?;

    Ok(branch)
}

/// Tries to get tree in given repository, from commit_id if given, otherwise HEAD tree, 
/// or empty tree if there is no HEAD commit
fn get_tree<'repo>(repository: &'repo Repository, commit_id: Option<&str>) -> Result<Tree<'repo>, String> {
    match commit_id {
        Some(commit_id) => {
            repository.revparse_single(commit_id)
                .and_then(|o| o.peel_to_commit())
                .and_then(|c| c.tree())
                .map_err_to_string()
        },
        None => {
            match repository.head().and_then(|h| h.peel_to_commit()) {
                Ok(commit) => commit.tree().map_err_to_string(),
                Err(_) => {
                    repository.treebuilder(None)
                        .and_then(|tb| tb.write())
                        .and_then(|oid| repository.find_tree(oid))
                        .map_err_to_string()
                },
            }
        },
    }
}

pub fn get_file_changes(repository: &Repository, include_untracked: bool, from_commit: Option<&str>, to_commit: Option<&str>) -> Result<Vec<FileChange>, String> {
    let repository_workdir = repository.workdir()
        .ok_or("Failed to get workdir from repository".to_string())?;

    let old_tree = get_tree(repository, from_commit).map_err_with_prefix("Failed to get old tree:")?;
    let new_tree = if to_commit.is_some() {
        Some(get_tree(repository, to_commit).map_err_with_prefix("Failed to get new tree:")?)
    } else {
        None
    };

    let mut diff_options = git2::DiffOptions::new();
    diff_options.include_untracked(include_untracked).recurse_untracked_dirs(include_untracked);

    let diff = match new_tree { 
        Some(new_tree) => repository
            .diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(&mut diff_options)),
        None => repository
            .diff_tree_to_workdir(Some(&old_tree), Some(&mut diff_options)),
    }.map_err_with_prefix("Failed to get diff:")?;

    let mut file_changes = Vec::new();
    for delta in diff.deltas() {
        let old_paths_maybe = delta.old_file().path()
            .map(|path| (path.to_path_buf(), repository_workdir.join(path)));
        let new_paths_maybe = delta.new_file().path()
            .map(|path| (path.to_path_buf(), repository_workdir.join(path)));

        match delta.status() {
            git2::Delta::Added | git2::Delta::Copied | git2::Delta::Untracked => {
                let (relative_path, absolute_path) = new_paths_maybe
                    .ok_or("Failed to get new file path for file added")?;
                file_changes.push(FileChange {
                    relative_path,
                    absolute_path,
                    status: FileChangeStatus::ADDED,
                });
            },
            git2::Delta::Modified | git2::Delta::Conflicted => {
                let (relative_path, absolute_path) = new_paths_maybe
                    .ok_or("Failed to get new file path for file added")?;
                file_changes.push(FileChange {
                    relative_path,
                    absolute_path,
                    status: FileChangeStatus::MODIFIED,
                });
            },
            git2::Delta::Deleted => {
                let (relative_path, absolute_path) = old_paths_maybe
                    .ok_or("Failed to get old file path for file deleted")?;
                file_changes.push(FileChange {
                    relative_path,
                    absolute_path,
                    status: FileChangeStatus::DELETED,
                });
            },
            git2::Delta::Typechange | git2::Delta::Renamed => {
                if let Some((old_rel_path, old_abs_path)) = old_paths_maybe {
                    file_changes.push(FileChange {
                        relative_path: old_rel_path,
                        absolute_path: old_abs_path,
                        status: FileChangeStatus::DELETED,
                    });
                }
                if let Some((new_rel_path, new_abs_path)) = new_paths_maybe {
                    file_changes.push(FileChange {
                        relative_path: new_rel_path,
                        absolute_path: new_abs_path,
                        status: FileChangeStatus::ADDED,
                    });
                }
            },
            git2::Delta::Unmodified | git2::Delta::Ignored => {},
            git2::Delta::Unreadable => { 
                tracing::error!("Failed to read file: (old) {} (new) {}", 
                    delta.old_file().path().map_or("<none>", |p| p.to_str().unwrap_or("<invalid>")),
                    delta.new_file().path().map_or("<none>", |p| p.to_str().unwrap_or("<invalid>")));
            },
        };
    }

    Ok(file_changes)
}

pub fn stage_changes(repository: &Repository, file_changes: &Vec<FileChange>) -> Result<(), String> {
    let mut index = repository.index()
        .map_err(|e| format!("Failed to get index: {}", e))?;

    for file_change in file_changes {
        match file_change.status {
            FileChangeStatus::ADDED | FileChangeStatus::MODIFIED => {
                index.add_path(&file_change.relative_path)
                    .map_err(|e| format!("Failed to add file to index: {}", e))?;
            },
            FileChangeStatus::DELETED => {
                index.remove_path(&file_change.relative_path)
                    .map_err(|e| format!("Failed to remove file from index: {}", e))?;
            },
        }
    }

    index.write()
        .map_err(|e| format!("Failed to write index: {}", e))?;

    Ok(())
}

pub fn get_configured_author_email_and_name(repository: &Repository) -> Result<(String, String), String> {
    let config = repository.config().map_err(|e| format!("Failed to get repository config: {}", e))?;
    let author_email = config.get_string("user.email")
       .map_err(|e| format!("Failed to get author email: {}", e))?;
    let author_name = config.get_string("user.name")
        .map_err(|e| format!("Failed to get author name: {}", e))?;
    Ok((author_email, author_name))
}

pub fn commit(repository: &Repository, branch: &Branch, message: &str, author_name: &str, author_email: &str) -> Result<Oid, String> {

    let mut index = repository.index()
        .map_err(|e| format!("Failed to get index: {}", e))?;
    let tree_id = index.write_tree()
        .map_err(|e| format!("Failed to write tree: {}", e))?;
    let tree = repository.find_tree(tree_id)
        .map_err(|e| format!("Failed to find tree: {}", e))?;

    let signature = Signature::now(author_name, author_email)
        .map_err(|e| format!("Failed to create signature: {}", e))?;

    let branch_ref_name = branch.get().name()
        .ok_or_else(|| "Invalid branch name".to_string())?;

    let parent_commit = if let Some(target) = branch.get().target() {
        repository.find_commit(target)
            .map_err(|e| format!("Failed to find branch commit: {}", e))?
    } else {
        return Err("No parent commits found".to_string());
    };

    repository.commit(
        Some(branch_ref_name), &signature, &signature, message, &tree, &[&parent_commit]
    ).map_err(|e| format!("Failed to create commit: {}", e))
}

pub fn get_datetime_from_commit(repository: &Repository, commit_id: &str) -> Result<DateTime<Utc>, String> {
    let commit = repository.find_commit(Oid::from_str(commit_id).map_err_to_string()?)
        .map_err_to_string()?;

    Utc.timestamp_opt(commit.time().seconds(), 0).single()
        .ok_or_else(|| "Failed to get commit datetime".to_string())
}

fn git_diff<'repo>(repository: &'repo Repository, file_changes: &Vec<FileChange>) -> Result<git2::Diff<'repo>, String> {
    let mut diff_options = DiffOptions::new();
    diff_options.include_untracked(true);
    diff_options.recurse_untracked_dirs(true);
    for file_change in file_changes {
        diff_options.pathspec(&file_change.relative_path);
    }

    let mut sorted_file_changes = file_changes.clone();
    sorted_file_changes.sort_by_key(|fc| {
        std::fs::metadata(&fc.relative_path).map(|meta| meta.len()).unwrap_or(0)
    });

    // Create a new temporary tree, with all changes staged
    let mut index = repository.index().map_err(|e| format!("Failed to get repository index: {}", e))?;
    for file_change in &sorted_file_changes {
        match file_change.status {
            FileChangeStatus::ADDED | FileChangeStatus::MODIFIED => {
                index.add_path(&file_change.relative_path)
                    .map_err(|e| format!("Failed to add file to index: {}", e))?;
            },
            FileChangeStatus::DELETED => {
                index.remove_path(&file_change.relative_path)
                    .map_err(|e| format!("Failed to remove file from index: {}", e))?;
            },
        }
    }
    let oid = index.write_tree().map_err(|e| format!("Failed to write tree: {}", e))?;
    let new_tree = repository.find_tree(oid).map_err(|e| format!("Failed to find tree: {}", e))?;

    let head = repository.head().and_then(|head_ref| head_ref.peel_to_tree())
        .map_err(|e| format!("Failed to get HEAD tree: {}", e))?;

    let diff = repository.diff_tree_to_tree(Some(&head), Some(&new_tree), Some(&mut diff_options))
        .map_err(|e| format!("Failed to generate diff: {}", e))?;

    Ok(diff)
}

/// Similar to `git diff`, from specified file changes.
pub fn git_diff_as_string(repository: &Repository, file_changes: &Vec<FileChange>, max_size: usize) -> Result<String, String> {
    let diff = git_diff(repository, file_changes)?;

    let mut diff_str = String::new();
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        let line_content = std::str::from_utf8(line.content()).unwrap_or("");
        if diff_str.len() + line_content.len() < max_size {
            diff_str.push(line.origin());
            diff_str.push_str(line_content);
            if diff_str.len() > max_size {
                diff_str.truncate(max_size - 4);
                diff_str.push_str("...\n");
            }
        }
        true
    }).map_err(|e| format!("Failed to print diff: {}", e))?;

    Ok(diff_str)
}

pub async fn get_commit_information_from_current_changes(gcx: Arc<ARwLock<GlobalContext>>) -> Vec<CommitInfo>
{
    let mut commits = Vec::new();

    let workspace_vcs_roots: Arc<StdMutex<Vec<PathBuf>>> = {
        let cx_locked = gcx.write().await;
        cx_locked.documents_state.workspace_vcs_roots.clone()
    };

    let vcs_roots_locked = workspace_vcs_roots.lock().unwrap();
    tracing::info!("get_commit_information_from_current_changes() vcs_roots={:?}", vcs_roots_locked);
    for project_path in vcs_roots_locked.iter() {
        let repository = match git2::Repository::open(project_path) {
            Ok(repo) => repo,
            Err(e) => { tracing::warn!("{}", e); continue; }
        };

        let file_changes = match get_file_changes(&repository, true, None, None) {
            Ok(changes) if changes.is_empty() => { continue; }
            Ok(changes) => changes,
            Err(e) => { tracing::warn!("{}", e); continue; }
        };

        commits.push(CommitInfo {
            project_path: Url::from_file_path(project_path).ok().unwrap_or_else(|| Url::parse("file:///").unwrap()),
            commit_message: "".to_string(),
            file_changes,
        });
    }

    commits
}

pub async fn generate_commit_messages(gcx: Arc<ARwLock<GlobalContext>>, commits: Vec<CommitInfo>) -> Vec<CommitInfo> {
    const MAX_DIFF_SIZE: usize = 4096;
    let mut commits_with_messages = Vec::new();
    for commit in commits {
        let project_path = commit.project_path.to_file_path().ok().unwrap_or_default();

        let repository = match git2::Repository::open(&project_path) {
            Ok(repo) => repo,
            Err(e) => { error!("{}", e); continue; }
        };

        let diff = match git_diff_as_string(&repository, &commit.file_changes, MAX_DIFF_SIZE) {
            Ok(d) if d.is_empty() => { continue; }
            Ok(d) => d,
            Err(e) => { error!("{}", e); continue; }
        };

        let commit_msg = match generate_commit_message_by_diff(gcx.clone(), &diff, &None).await {
            Ok(msg) => msg,
            Err(e) => { error!("{}", e); continue; }
        };

        commits_with_messages.push(CommitInfo {
            project_path: commit.project_path,
            commit_message: commit_msg,
            file_changes: commit.file_changes,
        });
    }

    commits_with_messages
}

pub fn open_or_initialize_repo(workdir: &PathBuf, git_dir_path: &PathBuf) -> Result<Repository, String> {
    match git2::Repository::open(&git_dir_path) {
        Ok(repo) => {
            repo.set_workdir(&workdir, false).map_err_to_string()?;
            Ok(repo)
        },
        Err(not_found_err) if not_found_err.code() == git2::ErrorCode::NotFound => {
            let repo = git2::Repository::init(&git_dir_path).map_err_to_string()?;
            repo.set_workdir(&workdir, false).map_err_to_string()?;

            {
                let tree_id = {
                    let mut index = repo.index().map_err_to_string()?;
                    index.write_tree().map_err_to_string()?
                };
                let tree = repo.find_tree(tree_id).map_err_to_string()?;
                let signature = git2::Signature::now("Refact Agent", "agent@refact.ai")
                    .map_err_to_string()?;
                repo.commit(Some("HEAD"), &signature, &signature, "Initial commit", &tree, &[])
                    .map_err_to_string()?;
            }

            Ok(repo)
        },
        Err(e) => Err(e.to_string()),
    }
}

pub fn checkout_head_and_branch_to_commit(repo: &Repository, branch_name: &str, commit_hash: &str) -> Result<(), String> {
    let commit = Oid::from_str(commit_hash)
        .and_then(|oid| repo.find_commit(oid))
        .map_err_with_prefix("Failed to find commit:")?;
    let tree = commit.tree().map_err_to_string()?;

    let mut branch_ref = create_or_checkout_to_branch(repo, branch_name)?.into_reference();
    branch_ref.set_target(commit.id(),"Restoring checkpoint")
        .map_err_with_prefix("Failed to update branch reference:")?;

    repo.checkout_tree(&tree.into_object(), None)
        .map_err_with_prefix("Failed  to checkout tree:")?;

    repo.set_head(&format!("refs/heads/{}", branch_name))
        .map_err_with_prefix("Failed to set HEAD:")?;

    Ok(())
}

pub async fn create_workspace_checkpoint(
    gcx: Arc<ARwLock<GlobalContext>>,
    prev_checkpoint: Option<&Checkpoint>,
    chat_id: &str,
) -> Result<(Checkpoint, Vec<FileChange>, Repository, DateTime<Utc>), String> {
    let cache_dir = gcx.read().await.cache_dir.clone();
    let workspace_folder = get_active_workspace_folder(gcx.clone()).await
        .ok_or_else(|| "No active workspace folder".to_string())?;
    let workspace_folder_hash = official_text_hashing_function(&workspace_folder.to_string_lossy().to_string());

    if let Some(prev_checkpoint) = prev_checkpoint {
        if prev_checkpoint.workspace_hash() != workspace_folder_hash {
            return Err("Can not create checkpoint for different workspace folder".to_string());
        }
    }

    let shadow_repo_path  = cache_dir.join("shadow_git").join(&workspace_folder_hash);
    let repo = open_or_initialize_repo(&workspace_folder, &shadow_repo_path)
        .map_err_with_prefix("Failed to open or init repo:")?;

    let (checkpoint, file_changes) = {
        let branch = create_or_checkout_to_branch(&repo, &format!("refact-{chat_id}"))?;
        let commit_oid_from_branch = branch.get().target().map(|oid| oid.to_string());
        let file_changes = get_file_changes(&repo, true, commit_oid_from_branch.as_deref(), None)?;
        stage_changes(&repo, &file_changes)?;

        let commit_oid = commit(&repo, &branch, &format!("Auto commit for chat {chat_id}"), "Refact Agent", "agent@refact.ai")?;
        (Checkpoint {workspace_folder, commit_hash: commit_oid.to_string()}, file_changes)
    };
    let commit_datetime = get_datetime_from_commit(&repo, &checkpoint.commit_hash)?;

    Ok((checkpoint, file_changes, repo, commit_datetime))
}

pub async fn restore_workspace_checkpoint(
    gcx: Arc<ARwLock<GlobalContext>>, checkpoint_to_restore: &Checkpoint, chat_id: &str
) -> Result<(Checkpoint, Vec<FileChange>, DateTime<Utc>), String> {

    let (checkpoint_for_undo, _, repo, reverted_to) = 
        create_workspace_checkpoint(gcx.clone(), Some(checkpoint_to_restore), chat_id).await?;
    
    let files_changed = get_file_changes(&repo, true, 
        Some(&checkpoint_to_restore.commit_hash), Some(&checkpoint_for_undo.commit_hash))?;

    checkout_head_and_branch_to_commit(&repo, &format!("refact-{chat_id}"), &checkpoint_to_restore.commit_hash)?;

    Ok((checkpoint_for_undo, files_changed, reverted_to))
}