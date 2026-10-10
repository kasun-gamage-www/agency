//! Thin wrapper around the `git` command-line tool.
//!
//! Shelling out (rather than linking libgit2) means the app uses the user's own
//! git configuration, credential helpers and SSH setup for fetch/pull/push.

use std::path::{Path, PathBuf};
use std::process::Command;

const FIELD: char = '\x1f';
const MAX_TEXT_LINES: usize = 20_000;

#[derive(Clone, Debug)]
pub struct CommitInfo {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub refs: String,
    pub subject: String,
}

/// One line of `git log --graph` output: graph art plus, unless it is a
/// connector-only line, the commit drawn on it.
#[derive(Clone, Debug)]
pub struct LogLine {
    pub graph: String,
    pub commit: Option<CommitInfo>,
}

#[derive(Clone, Debug)]
pub struct FileStatus {
    pub path: String,
    pub orig_path: Option<String>,
    /// Index (staged) state, `.` when unchanged, `?` when untracked.
    pub index: char,
    /// Worktree (unstaged) state, `.` when unchanged, `?` when untracked.
    pub worktree: char,
    pub conflicted: bool,
}

impl FileStatus {
    pub fn is_untracked(&self) -> bool {
        self.index == '?'
    }
    pub fn has_staged(&self) -> bool {
        !self.is_untracked() && !self.conflicted && self.index != '.'
    }
    pub fn has_unstaged(&self) -> bool {
        self.is_untracked() || self.conflicted || self.worktree != '.'
    }
}

#[derive(Clone, Debug, Default)]
pub struct RepoStatus {
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<FileStatus>,
}

impl RepoStatus {
    pub fn staged_count(&self) -> usize {
        self.files.iter().filter(|f| f.has_staged()).count()
    }
}

#[derive(Clone, Debug)]
pub struct Branch {
    pub name: String,
    pub is_remote: bool,
    pub is_head: bool,
    pub upstream: String,
    pub track: String,
    pub short_hash: String,
    pub date: String,
    pub subject: String,
}

#[derive(Clone, Debug)]
pub struct Stash {
    pub name: String,
    pub subject: String,
}

fn command(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(["-c", "color.ui=false", "-c", "core.quotepath=false"])
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn exec<S: AsRef<std::ffi::OsStr>>(repo: &Path, args: &[S]) -> Result<(String, String), String> {
    let out = command(repo)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if out.status.success() {
        Ok((stdout, stderr))
    } else {
        let msg = if stderr.trim().is_empty() { stdout } else { stderr };
        Err(msg.trim().to_string())
    }
}

/// Runs git and returns stdout (for commands whose output we parse).
pub fn run<S: AsRef<std::ffi::OsStr>>(repo: &Path, args: &[S]) -> Result<String, String> {
    exec(repo, args).map(|(out, _)| out)
}

/// Runs git and returns stdout+stderr (for operations whose progress git
/// reports on stderr, like fetch/pull/push).
pub fn run_op<S: AsRef<std::ffi::OsStr>>(repo: &Path, args: &[S]) -> Result<String, String> {
    exec(repo, args).map(|(out, err)| {
        let mut s = out.trim().to_string();
        if !err.trim().is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(err.trim());
        }
        s
    })
}

pub fn version() -> Option<String> {
    run(Path::new("."), &["--version"]).ok().map(|s| s.trim().to_string())
}

/// Resolves any path inside a working tree to the repository's top-level directory.
pub fn repo_root(path: &Path) -> Result<PathBuf, String> {
    let out = run(path, &["rev-parse", "--show-toplevel"])?;
    let top = out.trim();
    if top.is_empty() {
        return Err(format!("{} is not inside a git working tree", path.display()));
    }
    #[cfg(windows)]
    let top = top.replace('/', "\\");
    Ok(PathBuf::from(top))
}

pub fn log(repo: &Path, all_branches: bool, limit: usize) -> Result<Vec<LogLine>, String> {
    let limit = format!("-n{limit}");
    let mut args = vec![
        "log",
        "--graph",
        "--date=format:%Y-%m-%d %H:%M",
        "--pretty=format:%x1f%H%x1f%h%x1f%an%x1f%ad%x1f%D%x1f%s",
        &limit,
    ];
    if all_branches {
        args.push("--all");
    }
    let out = match run(repo, &args) {
        Ok(out) => out,
        // A freshly initialised repo has no commits yet; that's not an error.
        Err(e) if e.contains("does not have any commits") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(out
        .lines()
        .map(|line| match line.find(FIELD) {
            Some(idx) => {
                let f: Vec<&str> = line[idx + 1..].splitn(6, FIELD).collect();
                let get = |i: usize| f.get(i).copied().unwrap_or_default().to_string();
                LogLine {
                    graph: line[..idx].to_string(),
                    commit: Some(CommitInfo {
                        hash: get(0),
                        short: get(1),
                        author: get(2),
                        date: get(3),
                        refs: get(4),
                        subject: get(5),
                    }),
                }
            }
            None => LogLine { graph: line.to_string(), commit: None },
        })
        .collect())
}

pub fn show(repo: &Path, hash: &str) -> Result<Vec<String>, String> {
    run(repo, &["show", "--stat", "--patch", "--format=fuller", hash]).map(|s| to_lines(&s))
}

pub fn status(repo: &Path) -> Result<RepoStatus, String> {
    let out = run(
        repo,
        &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"],
    )?;
    let mut st = RepoStatus::default();
    let mut tokens = out.split('\0');
    while let Some(tok) = tokens.next() {
        if tok.is_empty() {
            continue;
        }
        if let Some(header) = tok.strip_prefix("# ") {
            if let Some(v) = header.strip_prefix("branch.head ") {
                st.branch = v.to_string();
            } else if let Some(v) = header.strip_prefix("branch.upstream ") {
                st.upstream = Some(v.to_string());
            } else if let Some(v) = header.strip_prefix("branch.ab ") {
                for part in v.split_whitespace() {
                    if let Some(n) = part.strip_prefix('+') {
                        st.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix('-') {
                        st.behind = n.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        let xy = |s: &str| {
            let mut c = s.chars();
            (c.next().unwrap_or('.'), c.next().unwrap_or('.'))
        };
        match tok.as_bytes()[0] {
            // Ordinary change: 1 XY sub mH mI mW hH hI path
            b'1' => {
                let f: Vec<&str> = tok.splitn(9, ' ').collect();
                if f.len() == 9 {
                    let (x, y) = xy(f[1]);
                    st.files.push(FileStatus {
                        path: f[8].to_string(),
                        orig_path: None,
                        index: x,
                        worktree: y,
                        conflicted: false,
                    });
                }
            }
            // Rename/copy: 2 XY sub mH mI mW hH hI Xscore path \0 origPath
            b'2' => {
                let f: Vec<&str> = tok.splitn(10, ' ').collect();
                let orig = tokens.next().map(str::to_string);
                if f.len() == 10 {
                    let (x, y) = xy(f[1]);
                    st.files.push(FileStatus {
                        path: f[9].to_string(),
                        orig_path: orig,
                        index: x,
                        worktree: y,
                        conflicted: false,
                    });
                }
            }
            // Unmerged: u XY sub m1 m2 m3 mW h1 h2 h3 path
            b'u' => {
                let f: Vec<&str> = tok.splitn(11, ' ').collect();
                if f.len() == 11 {
                    let (x, y) = xy(f[1]);
                    st.files.push(FileStatus {
                        path: f[10].to_string(),
                        orig_path: None,
                        index: x,
                        worktree: y,
                        conflicted: true,
                    });
                }
            }
            b'?' if tok.len() > 2 => st.files.push(FileStatus {
                path: tok[2..].to_string(),
                orig_path: None,
                index: '?',
                worktree: '?',
                conflicted: false,
            }),
            _ => {}
        }
    }
    Ok(st)
}

pub fn file_diff(repo: &Path, path: &str, staged: bool, untracked: bool) -> Vec<String> {
    if untracked {
        let full = repo.join(path);
        return match std::fs::read(&full) {
            Ok(bytes) if bytes.iter().take(8000).any(|b| *b == 0) => {
                vec!["(new binary file)".to_string()]
            }
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let mut lines = vec![format!("new file: {path}"), String::new()];
                lines.extend(to_lines(&text).into_iter().map(|l| format!("+{l}")));
                lines
            }
            Err(e) => vec![format!("cannot read {}: {e}", full.display())],
        };
    }
    let mut args = vec!["diff"];
    if staged {
        args.push("--cached");
    }
    args.extend(["--", path]);
    match run(repo, &args) {
        Ok(out) if out.trim().is_empty() => vec!["(no textual changes)".to_string()],
        Ok(out) => to_lines(&out),
        Err(e) => vec![e],
    }
}

pub fn branches(repo: &Path) -> Result<Vec<Branch>, String> {
    let out = run(
        repo,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(HEAD)%1f%(refname)%1f%(refname:short)%1f%(upstream:short)%1f%(upstream:track)%1f%(objectname:short)%1f%(committerdate:relative)%1f%(contents:subject)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(8, FIELD).collect();
            if f.len() < 8 || f[1].ends_with("/HEAD") {
                return None;
            }
            Some(Branch {
                is_head: f[0] == "*",
                is_remote: f[1].starts_with("refs/remotes/"),
                name: f[2].to_string(),
                upstream: f[3].to_string(),
                track: f[4].to_string(),
                short_hash: f[5].to_string(),
                date: f[6].to_string(),
                subject: f[7].to_string(),
            })
        })
        .collect())
}

pub fn stashes(repo: &Path) -> Result<Vec<Stash>, String> {
    let out = run(repo, &["stash", "list", "--format=%gd%x1f%s"])?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let (name, subject) = l.split_once(FIELD)?;
            Some(Stash { name: name.to_string(), subject: subject.to_string() })
        })
        .collect())
}

pub fn stage(repo: &Path, paths: &[String]) -> Result<String, String> {
    let mut args = vec!["add", "-A", "--"];
    args.extend(paths.iter().map(String::as_str));
    run_op(repo, &args)
}

pub fn unstage(repo: &Path, paths: &[String]) -> Result<String, String> {
    let mut args = vec!["restore", "--staged", "--"];
    args.extend(paths.iter().map(String::as_str));
    run_op(repo, &args).or_else(|_| {
        // `restore --staged` needs HEAD; before the first commit, drop from the index instead.
        let mut args = vec!["rm", "--cached", "-r", "-q", "--"];
        args.extend(paths.iter().map(String::as_str));
        run_op(repo, &args)
    })
}

pub fn discard(repo: &Path, file: &FileStatus) -> Result<String, String> {
    if file.is_untracked() {
        run_op(repo, &["clean", "-f", "--", &file.path])
    } else {
        run_op(repo, &["restore", "--worktree", "--", &file.path])
    }
}

pub fn commit(repo: &Path, message: &str, amend: bool) -> Result<String, String> {
    let mut args = vec!["commit", "-m", message];
    if amend {
        args.push("--amend");
    }
    run_op(repo, &args)
}

pub fn checkout(repo: &Path, branch: &str) -> Result<String, String> {
    run_op(repo, &["checkout", branch])
}

pub fn checkout_remote(repo: &Path, remote_branch: &str) -> Result<String, String> {
    run_op(repo, &["checkout", "--track", remote_branch])
}

pub fn create_branch(repo: &Path, name: &str) -> Result<String, String> {
    run_op(repo, &["checkout", "-b", name])
}

pub fn delete_branch(repo: &Path, name: &str, force: bool) -> Result<String, String> {
    run_op(repo, &["branch", if force { "-D" } else { "-d" }, name])
}

pub fn merge(repo: &Path, branch: &str) -> Result<String, String> {
    run_op(repo, &["merge", "--no-edit", branch])
}

pub fn fetch(repo: &Path) -> Result<String, String> {
    run_op(repo, &["fetch", "--all", "--prune"])
}

pub fn pull(repo: &Path) -> Result<String, String> {
    run_op(repo, &["pull", "--no-edit"])
}

pub fn push(repo: &Path) -> Result<String, String> {
    let has_upstream = run(repo, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_ok();
    if has_upstream {
        run_op(repo, &["push"])
    } else {
        // First push of a local branch: publish it to origin and track it.
        run_op(repo, &["push", "-u", "origin", "HEAD"])
    }
}

pub fn stash_push(repo: &Path) -> Result<String, String> {
    run_op(repo, &["stash", "push", "--include-untracked"])
}

pub fn stash_apply(repo: &Path, name: &str, pop: bool) -> Result<String, String> {
    run_op(repo, &["stash", if pop { "pop" } else { "apply" }, name])
}

pub fn stash_drop(repo: &Path, name: &str) -> Result<String, String> {
    run_op(repo, &["stash", "drop", name])
}

/// Finds git working trees at `dir` and up to `depth` levels below it.
pub fn find_repos(dir: &Path, depth: usize) -> Vec<PathBuf> {
    if dir.join(".git").exists() {
        return vec![dir.to_path_buf()];
    }
    if depth == 0 {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut subdirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    subdirs.sort();
    subdirs.iter().flat_map(|d| find_repos(d, depth - 1)).collect()
}

fn to_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .take(MAX_TEXT_LINES)
        .map(|l| l.replace('\t', "    "))
        .collect();
    if text.lines().nth(MAX_TEXT_LINES).is_some() {
        lines.push(format!("… output truncated after {MAX_TEXT_LINES} lines"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agency-desktop-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "Test User"],
            &["config", "user.email", "test@example.com"],
            &["config", "commit.gpgsign", "false"],
        ] {
            run(&dir, args).unwrap();
        }
        dir
    }

    #[test]
    fn end_to_end_operations() {
        let repo = scratch_repo("e2e");
        assert!(log(&repo, true, 50).unwrap().is_empty(), "unborn repo has empty log");

        fs::write(repo.join("a.txt"), "one\n").unwrap();
        fs::write(repo.join("b file.txt"), "two\n").unwrap();
        let st = status(&repo).unwrap();
        assert_eq!(st.branch, "main");
        assert_eq!(st.files.len(), 2);
        assert!(st.files.iter().all(|f| f.is_untracked()));

        stage(&repo, &["a.txt".into(), "b file.txt".into()]).unwrap();
        unstage(&repo, &["b file.txt".into()]).unwrap(); // unborn-branch fallback path
        stage(&repo, &["b file.txt".into()]).unwrap();
        assert_eq!(status(&repo).unwrap().staged_count(), 2);
        commit(&repo, "first commit", false).unwrap();

        create_branch(&repo, "feature/x").unwrap();
        fs::write(repo.join("a.txt"), "one\nchanged\n").unwrap();
        stage(&repo, &["a.txt".into()]).unwrap();
        commit(&repo, "feature work", false).unwrap();
        checkout(&repo, "main").unwrap();
        merge(&repo, "feature/x").unwrap();

        // Rename + unstaged modification + untracked file.
        run(&repo, &["mv", "b file.txt", "c file.txt"]).unwrap();
        fs::write(repo.join("a.txt"), "dirty\n").unwrap();
        fs::write(repo.join("new.txt"), "new\n").unwrap();
        let st = status(&repo).unwrap();
        let renamed = st.files.iter().find(|f| f.path == "c file.txt").expect("rename listed");
        assert_eq!(renamed.index, 'R');
        assert_eq!(renamed.orig_path.as_deref(), Some("b file.txt"));
        let a = st.files.iter().find(|f| f.path == "a.txt").unwrap();
        assert!(a.has_unstaged() && !a.has_staged());
        assert!(file_diff(&repo, "a.txt", false, false).iter().any(|l| l == "+dirty"));
        assert!(file_diff(&repo, "new.txt", false, true).iter().any(|l| l == "+new"));

        discard(&repo, a).unwrap();
        let untracked = st.files.iter().find(|f| f.path == "new.txt").unwrap();
        discard(&repo, untracked).unwrap();
        assert!(!repo.join("new.txt").exists());

        stash_push(&repo).unwrap();
        let stashes = stashes(&repo).unwrap();
        assert_eq!(stashes.len(), 1);
        stash_apply(&repo, &stashes[0].name, true).unwrap();
        assert!(stashes_is_empty(&repo));

        let lines = log(&repo, true, 50).unwrap();
        let commits: Vec<_> = lines.iter().filter_map(|l| l.commit.as_ref()).collect();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "feature work");
        assert!(commits[0].refs.contains("HEAD -> main"));
        assert!(lines.iter().all(|l| !l.graph.is_empty()));
        assert!(show(&repo, &commits[0].hash).unwrap().iter().any(|l| l.starts_with("commit ")));

        let br = branches(&repo).unwrap();
        assert!(br.iter().any(|b| b.name == "main" && b.is_head && !b.is_remote));
        assert!(br.iter().any(|b| b.name == "feature/x" && !b.is_head));
        delete_branch(&repo, "feature/x", false).unwrap();

        assert_eq!(repo_root(&repo).unwrap().file_name(), repo.file_name());
        assert_eq!(find_repos(repo.parent().unwrap(), 1).iter().filter(|p| **p == repo).count(), 1);
        let _ = fs::remove_dir_all(&repo);
    }

    fn stashes_is_empty(repo: &Path) -> bool {
        stashes(repo).unwrap().is_empty()
    }
}
