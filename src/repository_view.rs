//! This module contains [`RepositoryView`], which provides the [`Status`]
//! and general overview of the state of a given Git repository.

use std::io::BufReader;
use std::path::Path;
use std::{fs::File, path::PathBuf};

use anyhow::{Result, anyhow};
use git2::{Cred, ErrorCode, FetchOptions, RemoteCallbacks, Repository};
use log::{debug, error, trace};
use serde::{Deserialize, Serialize};
use ssh2_config::{ParseRule, SshConfig};
use submodule_view::SubmoduleView;

use crate::status::Status;

mod submodule_view;

/// A collection of results for a Git repository at a given path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryView {
    /// The directory name of the Git repository.
    pub name: String,
    /// The name of the current, open branch.
    pub branch: String,
    /// The [`Status`] of the working tree.
    pub status: Status,

    /// The parent directory of the `path` field. The value will be `None` if a parent is not found.
    pub parent: Option<String>,
    /// The remote origin URL. The value will be `None` if the URL cannot be found.
    pub url: Option<String>,

    /// The email used in either the local or global config for the repository.
    pub email: Option<String>,
    /// Views of submodules found within the repository.
    pub submodules: Vec<SubmoduleView>,
}

impl RepositoryView {
    /// Generates a collector for a given path.
    pub fn new(
        repo_path: &Path,
        include_email: bool,
        include_submodules: bool,
        fetch_remote: bool,
        fetch_password: String,
    ) -> Result<RepositoryView> {
        debug!(
            "attempting to generate collector for repository_view at path: {}",
            repo_path.display()
        );

        let repo = match Repository::open(repo_path) {
            Ok(repo) => repo,
            Err(e) if e.message() == "unsupported extension name extensions.worktreeconfig" => {
                error!(
                    "skipping error ({e}) until upstream libgit2 issue is resolved: https://github.com/libgit2/libgit2/issues/6044"
                );
                let unknown_report = RepositoryView::finalize(
                    repo_path,
                    None,
                    Status::Unknown,
                    None,
                    None,
                    Vec::with_capacity(0),
                )?;
                return Ok(unknown_report);
            }
            Err(e) => return Err(e.into()),
        };
        let (status, head, remote) = Status::find(&repo)?;

        let submodules = if include_submodules && !repo.is_bare() {
            SubmoduleView::list(&repo)?
        } else {
            Vec::with_capacity(0)
        };

        let branch = match &head {
            Some(head) => head
                .shorthand()
                .ok_or(anyhow!("full shorthand for Git reference is invalid UTF-8"))?,
            None => "HEAD",
        };

        let email = match include_email {
            true => Self::get_email(&repo),
            false => None,
        };

        let url = match remote {
            Some(remote) => remote.url().map(|s| s.to_string()),
            None => None,
        };
        let url_clone = url.clone();
        let url_clone2 = url.clone();
        let binding = url_clone2.unwrap_or("".to_string());
        let host = binding
            .split('@')
            .nth(1)
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");

        // Fetch the remote branch.
        if fetch_remote && url.is_some() && head.is_some() {
            fetch_remote_locally(&repo, url, host, fetch_password)?;
        }

        debug!(
            "finalized collector collection for repository_view at path: {}",
            repo_path.display()
        );
        RepositoryView::finalize(
            repo_path,
            Some(branch.to_string()),
            status,
            url_clone,
            email,
            submodules,
        )
    }

    /// Assemble a [`RepositoryView`] with metadata for a given repository.
    pub fn finalize(
        path: &Path,
        branch: Option<String>,
        status: Status,
        url: Option<String>,
        email: Option<String>,
        submodules: Vec<SubmoduleView>,
    ) -> Result<Self> {
        let name = match path.file_name() {
            Some(s) => match s.to_str() {
                Some(s) => s.to_string(),
                None => {
                    return Err(anyhow!(
                        "could not convert file name (&OsStr) to &str: {path:?}"
                    ));
                }
            },
            None => {
                return Err(anyhow!(
                    "received None (Option<&OsStr>) for file name: {path:?}"
                ));
            }
        };
        let parent = match path.parent() {
            Some(s) => match s.to_str() {
                Some(s) => Some(s.to_string()),
                None => return Err(anyhow!("could not convert path (Path) to &str: {s:?}")),
            },
            None => None,
        };
        let branch = match branch {
            Some(branch) => branch,
            None => "unknown".to_string(),
        };

        Ok(Self {
            name,
            branch,
            status,
            parent,
            url,
            email,
            submodules,
        })
    }

    /// Find the "user.email" value in the local or global Git config. The
    /// [`Repository::config()`] method will look for a local config first and fallback to
    /// global, as needed. Absorb and log any and all errors as the email field is non-critical to
    /// the final results.
    fn get_email(repository: &Repository) -> Option<String> {
        let config = match repository.config() {
            Ok(v) => v,
            Err(e) => {
                trace!("ignored error: {e}");
                return None;
            }
        };
        let mut entries = match config.entries(None) {
            Ok(v) => v,
            Err(e) => {
                trace!("ignored error: {e}");
                return None;
            }
        };

        // Greedily find our "user.email" value. Return the first result found.
        while let Some(entry) = entries.next() {
            match entry {
                Ok(entry) => {
                    if let Some(name) = entry.name() {
                        if name == "user.email" {
                            if let Some(value) = entry.value() {
                                return Some(value.to_string());
                            }
                        }
                    }
                }
                Err(e) => debug!("ignored error: {e}"),
            }
        }
        None
    }
}

fn fetch_remote_locally(
    repo: &Repository,
    url: Option<String>,
    host: &str,
    fetch_password: String,
) -> Result<()> {
    let (remote, _) = match repo.find_remote("origin") {
        Ok(origin) => (Some(origin), Some("origin".to_string())),
        Err(e) if e.code() == ErrorCode::NotFound => Status::choose_remote_greedily(&repo)?,
        Err(e) => return Err(e.into()),
    };
    let mut some_remote = remote.unwrap();
    let current_head = match repo.head() {
        Ok(head) => Some(head),
        Err(ref e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
            None
        }
        Err(e) => return Err(e.into()),
    };
    let some_head = current_head.unwrap();
    let short_remote_branch_name = some_head.shorthand().unwrap();
    let mut callbacks = RemoteCallbacks::new();
    let mut fetch_options = FetchOptions::new();
    let remote_url = url.unwrap().clone();
    let is_https = remote_url.starts_with("https://");
    if !is_https {
        debug!("fetching remote {} with ssh key", remote_url);
        callbacks.credentials(|_url, username_from_url, _allowed_types| {
            let host_to_check = host.to_string();
            let default_config_path = std::env::var("HOME").unwrap() + "/.ssh/config";
            let mut reader = BufReader::new(
                File::open(default_config_path).expect("Could not open configuration file"),
            );

            let config = SshConfig::default()
                .parse(&mut reader, ParseRule::STRICT)
                .expect("Failed to parse configuration");

            // Get the host from the remote url that is in format "git@host:owner/repo"
            // query() returns default params when there's no rule for the host
            let params = config.query(host_to_check);

            // Compose Default key by combining env variable $HOME and "/.ssh/config"
            let default_key_path = std::env::var("HOME").unwrap() + "/.ssh/id_rsa";
            let mut ssh_key_path = default_key_path.as_str();
            let default_key_file = PathBuf::from(ssh_key_path);
            // default params from ssh config

            // Get the ssh_key_path as string from the first entry from config "IdentityFile" if exists
            let binding = params
                .identity_file
                .or(Some(vec![default_key_file]))
                .to_owned()
                .unwrap()
                .to_owned();
            if let Some(identity_file) = binding.first() {
                ssh_key_path = identity_file.to_str().unwrap();
            }
            // in case there are multiple entries, get the first one
            debug!("ssh_key_path: {}", ssh_key_path);
            let pass = if fetch_password.is_empty() {
                None
            } else {
                Some(fetch_password.as_str())
            };

            return Cred::ssh_key(
                username_from_url.unwrap(),
                None,
                Path::new(ssh_key_path),
                pass,
            );
        });
    }
    fetch_options.remote_callbacks(callbacks);
    Ok(
        if let Err(e) =
            some_remote.fetch(&[&short_remote_branch_name], Some(&mut fetch_options), None)
        {
            let remote_url = some_remote.url().unwrap_or("unknown");
            debug!(
                "assuming unmerged; could not fetch remote branch {} from {} (ignored error: {})",
                short_remote_branch_name, remote_url, e
            );
            // return Ok(false);
        } else {
            debug!(
                "fetched remote branch {} from {}",
                short_remote_branch_name, remote_url
            );
            // return Ok(true);
        },
    )
}
