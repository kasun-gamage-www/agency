//! The egui front end. Rendering code never mutates the workspace directly:
//! it emits `Action`s, which are applied after the frame is laid out.

use crate::git::{self, Branch, FileStatus, LogLine, RepoStatus, Stash};
use crate::store::{Project, Store};
use eframe::egui::{
    self, text::LayoutJob, Color32, FontId, RichText, Sense, TextFormat, TextStyle, TextWrapMode,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

const GRAPH_COLORS: [Color32; 6] = [
    Color32::from_rgb(0x4e, 0x9a, 0xf1),
    Color32::from_rgb(0xe5, 0x7a, 0x2e),
    Color32::from_rgb(0x3f, 0xb9, 0x50),
    Color32::from_rgb(0xc0, 0x5a, 0xd8),
    Color32::from_rgb(0xd9, 0x4f, 0x4f),
    Color32::from_rgb(0x2a, 0xb3, 0xb3),
];
const GREEN: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
const RED: Color32 = Color32::from_rgb(0xe0, 0x55, 0x55);
const YELLOW: Color32 = Color32::from_rgb(0xd8, 0xa6, 0x2a);
const BLUE: Color32 = Color32::from_rgb(0x4e, 0x9a, 0xf1);
const MAGENTA: Color32 = Color32::from_rgb(0xc0, 0x5a, 0xd8);

#[derive(Clone, PartialEq)]
enum Selection {
    None,
    Project(usize),
    Repo(PathBuf),
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    History,
    Changes,
    Branches,
}

#[derive(Clone, Copy)]
enum RemoteOp {
    Fetch,
    Pull,
    Push,
}

impl RemoteOp {
    fn label(self) -> &'static str {
        match self {
            RemoteOp::Fetch => "Fetch",
            RemoteOp::Pull => "Pull",
            RemoteOp::Push => "Push",
        }
    }
}

#[derive(Clone)]
enum Action {
    // Workspace
    Select(Selection),
    OpenNewProject,
    OpenRename(usize),
    OpenManage(usize),
    AddRepos(Option<usize>),
    ScanFolder(Option<usize>),
    RemoveRepo(PathBuf),
    SetRepoInProject(usize, PathBuf, bool),
    CreateProject(String),
    RenameProject(usize, String),
    DeleteProject(usize),
    Remote(PathBuf, RemoteOp),
    RemoteAll(usize, RemoteOp),
    RefreshSummaries(Vec<PathBuf>),
    OpenFolder(PathBuf),
    Confirm(String, String, Box<Action>),
    // Current repository
    ReloadRepo,
    LoadLog,
    SelectCommit(String),
    SelectFile(String, bool),
    Stage(Vec<String>),
    Unstage(Vec<String>),
    Discard(FileStatus),
    Commit,
    Checkout(String),
    CheckoutRemote(String),
    CreateBranch(String),
    DeleteBranch(String, bool),
    Merge(String),
    StashPush,
    StashApply(String, bool),
    StashDrop(String),
}

enum TaskMsg {
    Op { repo: PathBuf, label: String, result: Result<String, String> },
    Summary { repo: PathBuf, result: Result<RepoStatus, String> },
}

struct LogEntry {
    repo: String,
    label: String,
    ok: bool,
    output: String,
}

struct RepoView {
    path: PathBuf,
    tab: Tab,
    log: Vec<LogLine>,
    log_all: bool,
    log_limit: usize,
    log_error: Option<String>,
    filter: String,
    selected_commit: Option<String>,
    commit_detail: Vec<String>,
    status: Option<RepoStatus>,
    status_error: Option<String>,
    selected_file: Option<(String, bool)>,
    file_diff: Vec<String>,
    commit_msg: String,
    amend: bool,
    branches: Vec<Branch>,
    stashes: Vec<Stash>,
    new_branch: String,
}

impl RepoView {
    fn new(path: PathBuf) -> Self {
        let mut v = Self {
            path,
            tab: Tab::History,
            log: Vec::new(),
            log_all: true,
            log_limit: 500,
            log_error: None,
            filter: String::new(),
            selected_commit: None,
            commit_detail: Vec::new(),
            status: None,
            status_error: None,
            selected_file: None,
            file_diff: Vec::new(),
            commit_msg: String::new(),
            amend: false,
            branches: Vec::new(),
            stashes: Vec::new(),
            new_branch: String::new(),
        };
        v.reload();
        v
    }

    fn reload(&mut self) {
        self.load_log();
        self.load_status();
        self.load_branches();
    }

    fn load_log(&mut self) {
        match git::log(&self.path, self.log_all, self.log_limit) {
            Ok(log) => {
                self.log = log;
                self.log_error = None;
            }
            Err(e) => {
                self.log.clear();
                self.log_error = Some(e);
            }
        }
    }

    fn load_status(&mut self) {
        match git::status(&self.path) {
            Ok(s) => {
                self.status = Some(s);
                self.status_error = None;
            }
            Err(e) => {
                self.status = None;
                self.status_error = Some(e);
            }
        }
        // Keep the diff pane in sync, or clear it if the file is no longer in that list.
        if let Some((path, staged)) = self.selected_file.clone() {
            let still_listed = self.status.as_ref().is_some_and(|s| {
                s.files
                    .iter()
                    .any(|f| f.path == path && if staged { f.has_staged() } else { f.has_unstaged() })
            });
            if still_listed {
                self.select_file(path, staged);
            } else {
                self.selected_file = None;
                self.file_diff.clear();
            }
        }
    }

    fn load_branches(&mut self) {
        self.branches = git::branches(&self.path).unwrap_or_default();
        self.stashes = git::stashes(&self.path).unwrap_or_default();
    }

    fn select_commit(&mut self, hash: String) {
        self.commit_detail = git::show(&self.path, &hash).unwrap_or_else(|e| vec![e]);
        self.selected_commit = Some(hash);
    }

    fn select_file(&mut self, path: String, staged: bool) {
        let untracked = self
            .status
            .as_ref()
            .and_then(|s| s.files.iter().find(|f| f.path == path))
            .is_some_and(|f| f.is_untracked());
        self.file_diff = git::file_diff(&self.path, &path, staged, untracked);
        self.selected_file = Some((path, staged));
    }
}

pub struct AgencyApp {
    store: Store,
    selection: Selection,
    repo_view: Option<RepoView>,
    summaries: HashMap<PathBuf, Result<RepoStatus, String>>,
    loading: HashSet<PathBuf>,
    running: HashMap<PathBuf, usize>,
    tx: Sender<TaskMsg>,
    rx: Receiver<TaskMsg>,
    ctx: egui::Context,
    activity: Vec<LogEntry>,
    show_activity: bool,
    new_project: Option<String>,
    rename_project: Option<(usize, String)>,
    manage_project: Option<usize>,
    confirm: Option<(String, String, Action)>,
    error: Option<String>,
}

impl AgencyApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = channel();
        let mut app = Self {
            store: Store::load(),
            selection: Selection::None,
            repo_view: None,
            summaries: HashMap::new(),
            loading: HashSet::new(),
            running: HashMap::new(),
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
            activity: Vec::new(),
            show_activity: false,
            new_project: None,
            rename_project: None,
            manage_project: None,
            confirm: None,
            error: None,
        };
        if git::version().is_none() {
            app.error = Some(
                "Could not run `git`. Install Git and make sure it is on your PATH.".to_string(),
            );
        }
        for repo in app.store.repos.clone() {
            app.refresh_summary(repo);
        }
        app
    }

    // ---------------------------------------------------------------- state

    fn save(&mut self) {
        if let Err(e) = self.store.save() {
            self.error = Some(format!("Could not save workspace: {e}"));
        }
    }

    fn report_error(&mut self, msg: String) {
        match &mut self.error {
            Some(existing) => {
                existing.push_str("\n\n");
                existing.push_str(&msg);
            }
            None => self.error = Some(msg),
        }
    }

    fn record(&mut self, repo: &Path, label: &str, result: Result<String, String>) {
        let name = repo_name(repo);
        let (ok, output) = match result {
            Ok(out) => (true, out),
            Err(e) => {
                self.report_error(format!("{label} failed in {name}:\n{e}"));
                (false, e)
            }
        };
        self.activity.push(LogEntry { repo: name, label: label.to_string(), ok, output });
    }

    fn refresh_summary(&mut self, repo: PathBuf) {
        if !self.loading.insert(repo.clone()) {
            return;
        }
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = git::status(&repo);
            let _ = tx.send(TaskMsg::Summary { repo, result });
            ctx.request_repaint();
        });
    }

    fn spawn_remote(&mut self, repo: PathBuf, op: RemoteOp) {
        *self.running.entry(repo.clone()).or_default() += 1;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = match op {
                RemoteOp::Fetch => git::fetch(&repo),
                RemoteOp::Pull => git::pull(&repo),
                RemoteOp::Push => git::push(&repo),
            };
            let _ = tx.send(TaskMsg::Op { repo, label: op.label().to_string(), result });
            ctx.request_repaint();
        });
    }

    fn poll_tasks(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                TaskMsg::Summary { repo, result } => {
                    self.loading.remove(&repo);
                    self.summaries.insert(repo, result);
                }
                TaskMsg::Op { repo, label, result } => {
                    if let Some(n) = self.running.get_mut(&repo) {
                        *n = n.saturating_sub(1);
                    }
                    self.record(&repo, &label, result);
                    self.after_change(&repo);
                }
            }
        }
    }

    fn after_change(&mut self, repo: &Path) {
        if let Some(view) = self.repo_view.as_mut().filter(|v| v.path == repo) {
            view.reload();
        }
        self.refresh_summary(repo.to_path_buf());
    }

    fn select(&mut self, sel: Selection) {
        match &sel {
            Selection::Repo(path) => {
                if self.repo_view.as_ref().map(|v| &v.path) != Some(path) {
                    self.repo_view = Some(RepoView::new(path.clone()));
                }
                self.refresh_summary(path.clone());
            }
            Selection::Project(i) => {
                let repos = self.store.projects.get(*i).map(|p| p.repos.clone()).unwrap_or_default();
                for r in repos {
                    self.refresh_summary(r);
                }
            }
            Selection::None => {}
        }
        self.selection = sel;
    }

    fn add_repos(&mut self, dirs: Vec<PathBuf>, project: Option<usize>) {
        let mut failures = Vec::new();
        for dir in dirs {
            match git::repo_root(&dir) {
                Ok(root) => {
                    self.store.add_repo(root.clone());
                    if let Some(p) = project {
                        self.store.set_repo_in_project(p, &root, true);
                    }
                    self.refresh_summary(root);
                }
                Err(_) => failures.push(dir.display().to_string()),
            }
        }
        self.save();
        if !failures.is_empty() {
            self.report_error(format!("Not git repositories:\n{}", failures.join("\n")));
        }
    }

    // -------------------------------------------------------------- actions

    fn handle(&mut self, action: Action) {
        match action {
            Action::Select(sel) => self.select(sel),
            Action::OpenNewProject => self.new_project = Some(String::new()),
            Action::OpenRename(i) => {
                self.rename_project = self.store.projects.get(i).map(|p| (i, p.name.clone()));
            }
            Action::OpenManage(i) => self.manage_project = Some(i),
            Action::AddRepos(project) => {
                if let Some(dirs) =
                    rfd::FileDialog::new().set_title("Select git repositories").pick_folders()
                {
                    self.add_repos(dirs, project);
                }
            }
            Action::ScanFolder(project) => {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Scan a folder for git repositories")
                    .pick_folder()
                {
                    let found = git::find_repos(&dir, 2);
                    if found.is_empty() {
                        self.report_error(format!("No git repositories found under {}", dir.display()));
                    } else {
                        self.add_repos(found, project);
                    }
                }
            }
            Action::RemoveRepo(path) => {
                self.store.remove_repo(&path);
                self.summaries.remove(&path);
                if self.selection == Selection::Repo(path.clone()) {
                    self.selection = Selection::None;
                    self.repo_view = None;
                }
                self.save();
            }
            Action::SetRepoInProject(i, path, member) => {
                self.store.set_repo_in_project(i, &path, member);
                self.save();
            }
            Action::CreateProject(name) => {
                self.store.projects.push(Project { name, repos: Vec::new() });
                let i = self.store.projects.len() - 1;
                self.save();
                self.select(Selection::Project(i));
                self.manage_project = Some(i);
            }
            Action::RenameProject(i, name) => {
                if let Some(p) = self.store.projects.get_mut(i) {
                    p.name = name;
                }
                self.save();
            }
            Action::DeleteProject(i) => {
                if i < self.store.projects.len() {
                    self.store.projects.remove(i);
                    self.selection = match self.selection.clone() {
                        Selection::Project(j) if j == i => Selection::None,
                        Selection::Project(j) if j > i => Selection::Project(j - 1),
                        other => other,
                    };
                    self.manage_project = None;
                    self.save();
                }
            }
            Action::Remote(path, op) => self.spawn_remote(path, op),
            Action::RemoteAll(i, op) => {
                let repos = self.store.projects.get(i).map(|p| p.repos.clone()).unwrap_or_default();
                for r in repos {
                    self.spawn_remote(r, op);
                }
            }
            Action::RefreshSummaries(paths) => {
                for p in paths {
                    self.refresh_summary(p);
                }
            }
            Action::OpenFolder(path) => open_folder(&path),
            Action::Confirm(title, msg, action) => self.confirm = Some((title, msg, *action)),
            other => self.handle_repo_action(other),
        }
    }

    fn handle_repo_action(&mut self, action: Action) {
        let Some(view) = self.repo_view.as_mut() else { return };
        let repo = view.path.clone();
        let result = match action {
            Action::ReloadRepo => {
                view.reload();
                self.refresh_summary(repo);
                return;
            }
            Action::LoadLog => {
                view.load_log();
                return;
            }
            Action::SelectCommit(hash) => {
                view.select_commit(hash);
                return;
            }
            Action::SelectFile(path, staged) => {
                view.select_file(path, staged);
                return;
            }
            Action::Stage(paths) => ("Stage", git::stage(&repo, &paths)),
            Action::Unstage(paths) => ("Unstage", git::unstage(&repo, &paths)),
            Action::Discard(file) => ("Discard", git::discard(&repo, &file)),
            Action::Commit => {
                let r = git::commit(&repo, view.commit_msg.trim(), view.amend);
                if r.is_ok() {
                    view.commit_msg.clear();
                    view.amend = false;
                }
                ("Commit", r)
            }
            Action::Checkout(b) => ("Checkout", git::checkout(&repo, &b)),
            Action::CheckoutRemote(b) => ("Checkout", git::checkout_remote(&repo, &b)),
            Action::CreateBranch(b) => {
                let r = git::create_branch(&repo, &b);
                if r.is_ok() {
                    view.new_branch.clear();
                }
                ("Create branch", r)
            }
            Action::DeleteBranch(b, force) => ("Delete branch", git::delete_branch(&repo, &b, force)),
            Action::Merge(b) => ("Merge", git::merge(&repo, &b)),
            Action::StashPush => ("Stash", git::stash_push(&repo)),
            Action::StashApply(s, pop) => {
                (if pop { "Stash pop" } else { "Stash apply" }, git::stash_apply(&repo, &s, pop))
            }
            Action::StashDrop(s) => ("Stash drop", git::stash_drop(&repo, &s)),
            _ => return,
        };
        let (label, r) = result;
        self.record(&repo, label, r);
        self.after_change(&repo);
    }

    // ------------------------------------------------------------------- UI

    fn top_bar(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.heading("Agency Desktop");
            ui.separator();
            if ui.button("Add repository…").clicked() {
                acts.push(Action::AddRepos(None));
            }
            if ui.button("Scan folder…").on_hover_text("Add every git repo found in a folder").clicked() {
                acts.push(Action::ScanFolder(None));
            }
            if ui.button("New project").clicked() {
                acts.push(Action::OpenNewProject);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy: usize = self.running.values().sum();
                ui.toggle_value(&mut self.show_activity, format!("Activity ({})", self.activity.len()));
                if busy > 0 {
                    ui.label(format!("{busy} running"));
                    ui.spinner();
                }
            });
        });
    }

    fn activity_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Activity");
            if ui.small_button("Clear").clicked() {
                self.activity.clear();
            }
        });
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for e in &self.activity {
                let mark = if e.ok { RichText::new("OK ").color(GREEN) } else { RichText::new("ERR").color(RED) };
                ui.horizontal(|ui| {
                    ui.label(mark.monospace());
                    ui.strong(&e.repo);
                    ui.label(&e.label);
                });
                if !e.output.is_empty() {
                    ui.label(RichText::new(&e.output).monospace().weak());
                }
            }
        });
    }

    fn summary_text(&self, repo: &Path) -> RichText {
        match self.summaries.get(repo) {
            None => RichText::new("…").weak(),
            Some(Err(_)) => RichText::new("unavailable").color(RED),
            Some(Ok(s)) => {
                let mut t = s.branch.clone();
                if s.ahead > 0 {
                    t.push_str(&format!(" ↑{}", s.ahead));
                }
                if s.behind > 0 {
                    t.push_str(&format!(" ↓{}", s.behind));
                }
                if !s.files.is_empty() {
                    t.push_str(&format!(" +{}", s.files.len()));
                    RichText::new(t).color(YELLOW).small()
                } else {
                    RichText::new(t).weak().small()
                }
            }
        }
    }

    fn repo_entry(&self, ui: &mut egui::Ui, repo: &Path, project: Option<usize>, acts: &mut Vec<Action>) {
        let selected = self.selection == Selection::Repo(repo.to_path_buf());
        ui.horizontal(|ui| {
            let resp = ui
                .selectable_label(selected, repo_name(repo))
                .on_hover_text(repo.display().to_string());
            if resp.clicked() {
                acts.push(Action::Select(Selection::Repo(repo.to_path_buf())));
            }
            resp.context_menu(|ui| {
                for op in [RemoteOp::Fetch, RemoteOp::Pull, RemoteOp::Push] {
                    if ui.button(op.label()).clicked() {
                        acts.push(Action::Remote(repo.to_path_buf(), op));
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button("Open folder").clicked() {
                    acts.push(Action::OpenFolder(repo.to_path_buf()));
                    ui.close_menu();
                }
                if let Some(p) = project {
                    if ui.button("Remove from project").clicked() {
                        acts.push(Action::SetRepoInProject(p, repo.to_path_buf(), false));
                        ui.close_menu();
                    }
                } else if ui.button("Remove from workspace").clicked() {
                    acts.push(Action::Confirm(
                        "Remove repository".into(),
                        format!(
                            "Remove {} from the workspace and all projects?\nFiles on disk are not touched.",
                            repo.display()
                        ),
                        Box::new(Action::RemoveRepo(repo.to_path_buf())),
                    ));
                    ui.close_menu();
                }
            });
            if self.running.get(repo).copied().unwrap_or(0) > 0 {
                ui.spinner();
            }
            ui.label(self.summary_text(repo));
        });
    }

    fn sidebar(&self, ui: &mut egui::Ui, acts: &mut Vec<Action>) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Projects");
                if ui.small_button("+").on_hover_text("New project").clicked() {
                    acts.push(Action::OpenNewProject);
                }
            });
            if self.store.projects.is_empty() {
                ui.weak("No projects yet.");
            }
            for (i, project) in self.store.projects.iter().enumerate() {
                let title = format!("{}  ({})", project.name, project.repos.len());
                egui::CollapsingHeader::new(RichText::new(title).strong())
                    .id_salt(("project", i))
                    .default_open(true)
                    .show(ui, |ui| {
                        let sel = self.selection == Selection::Project(i);
                        if ui.selectable_label(sel, "Overview").clicked() {
                            acts.push(Action::Select(Selection::Project(i)));
                        }
                        for repo in &project.repos {
                            self.repo_entry(ui, repo, Some(i), acts);
                        }
                        ui.horizontal(|ui| {
                            if ui.small_button("Repos…").on_hover_text("Choose repositories").clicked() {
                                acts.push(Action::OpenManage(i));
                            }
                            if ui.small_button("Rename").clicked() {
                                acts.push(Action::OpenRename(i));
                            }
                            if ui.small_button("Delete").clicked() {
                                acts.push(Action::Confirm(
                                    "Delete project".into(),
                                    format!(
                                        "Delete project \"{}\"?\nIts repositories stay in the workspace.",
                                        project.name
                                    ),
                                    Box::new(Action::DeleteProject(i)),
                                ));
                            }
                        });
                    });
            }

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.heading("Repositories");
                if ui.small_button("+").on_hover_text("Add repository").clicked() {
                    acts.push(Action::AddRepos(None));
                }
            });
            if self.store.repos.is_empty() {
                ui.weak("Add a repository to get started.");
            }
            for repo in &self.store.repos {
                self.repo_entry(ui, repo, None, acts);
            }
        });
    }

    fn welcome(&self, ui: &mut egui::Ui, acts: &mut Vec<Action>) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            ui.heading("Welcome to Agency Desktop");
            ui.label("Group related git repositories into projects and work with them side by side.");
            ui.add_space(12.0);
            if ui.button("Add a repository…").clicked() {
                acts.push(Action::AddRepos(None));
            }
            if ui.button("Scan a folder for repositories…").clicked() {
                acts.push(Action::ScanFolder(None));
            }
            if ui.button("Create a project").clicked() {
                acts.push(Action::OpenNewProject);
            }
        });
    }

    fn project_overview(&self, ui: &mut egui::Ui, i: usize, acts: &mut Vec<Action>) {
        let Some(project) = self.store.projects.get(i) else { return };
        ui.horizontal(|ui| {
            ui.heading(&project.name);
            ui.weak(format!("{} repositories", project.repos.len()));
        });
        ui.horizontal(|ui| {
            for op in [RemoteOp::Fetch, RemoteOp::Pull, RemoteOp::Push] {
                if ui.button(format!("{} all", op.label())).clicked() {
                    acts.push(Action::RemoteAll(i, op));
                }
            }
            if ui.button("Refresh").clicked() {
                acts.push(Action::RefreshSummaries(project.repos.clone()));
            }
            ui.separator();
            if ui.button("Add repository…").clicked() {
                acts.push(Action::AddRepos(Some(i)));
            }
            if ui.button("Choose repositories…").clicked() {
                acts.push(Action::OpenManage(i));
            }
        });
        ui.separator();
        if project.repos.is_empty() {
            ui.label("This project has no repositories yet. Use \"Add repository…\" or \"Repos…\" in the sidebar.");
            return;
        }
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("project_grid").striped(true).num_columns(6).spacing([16.0, 6.0]).show(ui, |ui| {
                for h in ["Repository", "Branch", "Sync", "Changes", "Path", ""] {
                    ui.strong(h);
                }
                ui.end_row();
                for repo in &project.repos {
                    if ui.link(RichText::new(repo_name(repo)).strong()).clicked() {
                        acts.push(Action::Select(Selection::Repo(repo.clone())));
                    }
                    match self.summaries.get(repo) {
                        None => {
                            ui.spinner();
                            ui.label("");
                            ui.label("");
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new("unavailable").color(RED)).on_hover_text(e);
                            ui.label("");
                            ui.label("");
                        }
                        Some(Ok(s)) => {
                            ui.monospace(&s.branch);
                            match &s.upstream {
                                Some(up) => {
                                    let text = if s.ahead == 0 && s.behind == 0 {
                                        RichText::new("up to date").color(GREEN)
                                    } else {
                                        RichText::new(format!("↑{} ↓{}", s.ahead, s.behind)).color(YELLOW)
                                    };
                                    ui.label(text).on_hover_text(format!("tracking {up}"));
                                }
                                None => {
                                    ui.weak("no upstream");
                                }
                            }
                            if s.files.is_empty() {
                                ui.label(RichText::new("clean").color(GREEN));
                            } else {
                                ui.label(
                                    RichText::new(format!("{} changed ({} staged)", s.files.len(), s.staged_count()))
                                        .color(YELLOW),
                                );
                            }
                        }
                    }
                    ui.weak(repo.display().to_string());
                    ui.horizontal(|ui| {
                        if self.running.get(repo).copied().unwrap_or(0) > 0 {
                            ui.spinner();
                        }
                        for op in [RemoteOp::Fetch, RemoteOp::Pull, RemoteOp::Push] {
                            if ui.small_button(op.label()).clicked() {
                                acts.push(Action::Remote(repo.clone(), op));
                            }
                        }
                        if ui.small_button("Folder").clicked() {
                            acts.push(Action::OpenFolder(repo.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context, acts: &mut Vec<Action>) {
        if let Some(name) = self.new_project.as_mut() {
            let (mut submit, mut close) = (false, false);
            modal("New project").show(ctx, |ui| {
                ui.label("Project name");
                let resp = ui.text_edit_singleline(name);
                if !resp.has_focus() && name.is_empty() {
                    resp.request_focus();
                }
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    let valid = !name.trim().is_empty();
                    submit = (ui.add_enabled(valid, egui::Button::new("Create")).clicked() || enter) && valid;
                    close = ui.button("Cancel").clicked();
                });
            });
            if submit {
                acts.push(Action::CreateProject(name.trim().to_string()));
            }
            if submit || close {
                self.new_project = None;
            }
        }

        if let Some((i, name)) = self.rename_project.as_mut() {
            let (mut submit, mut close) = (false, false);
            modal("Rename project").show(ctx, |ui| {
                let resp = ui.text_edit_singleline(name);
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    let valid = !name.trim().is_empty();
                    submit = (ui.add_enabled(valid, egui::Button::new("Rename")).clicked() || enter) && valid;
                    close = ui.button("Cancel").clicked();
                });
            });
            if submit {
                acts.push(Action::RenameProject(*i, name.trim().to_string()));
            }
            if submit || close {
                self.rename_project = None;
            }
        }

        if let Some(i) = self.manage_project {
            let mut open = true;
            if let Some(project) = self.store.projects.get(i) {
                egui::Window::new(format!("Repositories in \"{}\"", project.name))
                    .id(egui::Id::new("manage_project"))
                    .open(&mut open)
                    .collapsible(false)
                    .default_width(460.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label("Tick the repositories that belong to this project.");
                        ui.separator();
                        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                            if self.store.repos.is_empty() {
                                ui.weak("The workspace has no repositories yet.");
                            }
                            for repo in &self.store.repos {
                                let mut member = project.repos.contains(repo);
                                let label = format!("{}   {}", repo_name(repo), repo.display());
                                if ui.checkbox(&mut member, label).changed() {
                                    acts.push(Action::SetRepoInProject(i, repo.clone(), member));
                                }
                            }
                        });
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("Add repository…").clicked() {
                                acts.push(Action::AddRepos(Some(i)));
                            }
                            if ui.button("Scan folder…").clicked() {
                                acts.push(Action::ScanFolder(Some(i)));
                            }
                        });
                    });
            } else {
                open = false;
            }
            if !open {
                self.manage_project = None;
            }
        }

        if let Some((title, msg, action)) = &self.confirm {
            let (mut yes, mut no) = (false, false);
            modal(title).show(ctx, |ui| {
                ui.label(msg);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    yes = ui.button(RichText::new("Confirm").color(RED)).clicked();
                    no = ui.button("Cancel").clicked();
                });
            });
            if yes {
                acts.push(action.clone());
            }
            if yes || no {
                self.confirm = None;
            }
        }

        if let Some(err) = &self.error {
            let mut close = false;
            modal("Something went wrong").show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    ui.label(RichText::new(err).monospace());
                });
                ui.add_space(8.0);
                close = ui.button("OK").clicked();
            });
            if close {
                self.error = None;
            }
        }
    }
}

impl eframe::App for AgencyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_tasks();
        let mut acts = Vec::new();

        egui::TopBottomPanel::top("top").show(ctx, |ui| self.top_bar(ui, &mut acts));
        if self.show_activity {
            egui::TopBottomPanel::bottom("activity")
                .resizable(true)
                .default_height(180.0)
                .show(ctx, |ui| self.activity_panel(ui));
        }
        egui::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| self.sidebar(ui, &mut acts));
        egui::CentralPanel::default().show(ctx, |ui| match self.selection.clone() {
            Selection::None => self.welcome(ui, &mut acts),
            Selection::Project(i) => self.project_overview(ui, i, &mut acts),
            Selection::Repo(_) => {
                if let Some(view) = self.repo_view.as_mut() {
                    let busy = self.running.get(&view.path).copied().unwrap_or(0) > 0;
                    repo_ui(ui, view, busy, &mut acts);
                }
            }
        });
        self.dialogs(ctx, &mut acts);

        for a in acts {
            self.handle(a);
        }
    }
}

// --------------------------------------------------------------- repo view

fn repo_ui(ui: &mut egui::Ui, view: &mut RepoView, busy: bool, acts: &mut Vec<Action>) {
    let path = view.path.clone();
    egui::TopBottomPanel::top("repo_header").show_inside(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading(repo_name(&path));
            if let Some(s) = &view.status {
                ui.label(RichText::new(&s.branch).monospace().color(GREEN).strong());
                if let Some(up) = &s.upstream {
                    ui.weak(format!("→ {up}  ↑{} ↓{}", s.ahead, s.behind));
                } else {
                    ui.weak("(no upstream)");
                }
            }
            if busy {
                ui.spinner();
            }
        });
        ui.weak(path.display().to_string());
        if let Some(e) = &view.status_error {
            ui.colored_label(RED, e);
        }
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                acts.push(Action::ReloadRepo);
            }
            for op in [RemoteOp::Fetch, RemoteOp::Pull, RemoteOp::Push] {
                if ui.add_enabled(!busy, egui::Button::new(op.label())).clicked() {
                    acts.push(Action::Remote(path.clone(), op));
                }
            }
            if ui.button("Stash").on_hover_text("Stash all changes, including untracked files").clicked() {
                acts.push(Action::StashPush);
            }
            if ui.button("Open folder").clicked() {
                acts.push(Action::OpenFolder(path.clone()));
            }
            ui.separator();
            let changes = view.status.as_ref().map_or(0, |s| s.files.len());
            ui.selectable_value(&mut view.tab, Tab::History, "History");
            ui.selectable_value(&mut view.tab, Tab::Changes, format!("Changes ({changes})"));
            ui.selectable_value(&mut view.tab, Tab::Branches, format!("Branches ({})", view.branches.len()));
        });
        ui.add_space(2.0);
    });
    match view.tab {
        Tab::History => history_ui(ui, view, acts),
        Tab::Changes => changes_ui(ui, view, acts),
        Tab::Branches => branches_ui(ui, view, acts),
    }
}

fn history_ui(ui: &mut egui::Ui, view: &mut RepoView, acts: &mut Vec<Action>) {
    egui::TopBottomPanel::top("history_controls").show_inside(ui, |ui| {
        ui.horizontal(|ui| {
            if ui.checkbox(&mut view.log_all, "All branches").changed() {
                acts.push(Action::LoadLog);
            }
            ui.label("Show");
            let before = view.log_limit;
            egui::ComboBox::from_id_salt("log_limit")
                .selected_text(view.log_limit.to_string())
                .show_ui(ui, |ui| {
                    for n in [200, 500, 2000, 10000] {
                        ui.selectable_value(&mut view.log_limit, n, n.to_string());
                    }
                });
            if view.log_limit != before {
                acts.push(Action::LoadLog);
            }
            ui.label("commits");
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut view.filter)
                    .hint_text("Filter by message, author, hash or ref")
                    .desired_width(280.0),
            );
        });
    });
    egui::TopBottomPanel::bottom("commit_detail")
        .resizable(true)
        .default_height(300.0)
        .show_inside(ui, |ui| {
            if view.selected_commit.is_none() {
                ui.weak("Select a commit to see its details.");
            } else {
                text_view(ui, "commit_detail_text", &view.commit_detail);
            }
        });
    egui::CentralPanel::default().show_inside(ui, |ui| {
        if let Some(e) = &view.log_error {
            ui.colored_label(RED, e);
            return;
        }
        if view.log.is_empty() {
            ui.weak("No commits yet.");
            return;
        }
        let needle = view.filter.trim().to_lowercase();
        let rows: Vec<usize> = if needle.is_empty() {
            (0..view.log.len()).collect()
        } else {
            (0..view.log.len())
                .filter(|&i| {
                    view.log[i].commit.as_ref().is_some_and(|c| {
                        c.subject.to_lowercase().contains(&needle)
                            || c.author.to_lowercase().contains(&needle)
                            || c.hash.starts_with(&needle)
                            || c.refs.to_lowercase().contains(&needle)
                    })
                })
                .collect()
        };
        let font = TextStyle::Monospace.resolve(ui.style());
        let row_h = ui.text_style_height(&TextStyle::Monospace);
        let sel_bg = ui.visuals().selection.bg_fill;
        let text = ui.visuals().text_color();
        let weak = ui.visuals().weak_text_color();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);
            egui::ScrollArea::both()
                .id_salt("history_rows")
                .auto_shrink([false, false])
                .show_rows(ui, row_h, rows.len(), |ui, range| {
                    for &i in &rows[range] {
                        let line = &view.log[i];
                        let selected = line.commit.as_ref().is_some_and(|c| Some(&c.hash) == view.selected_commit.as_ref());
                        let job = log_row(line, &font, selected.then_some(sel_bg), text, weak);
                        let resp = ui.add(egui::Label::new(job).sense(Sense::click()).selectable(false));
                        if let Some(c) = &line.commit {
                            if resp.clicked() {
                                acts.push(Action::SelectCommit(c.hash.clone()));
                            }
                            let hash = c.hash.clone();
                            resp.on_hover_cursor(egui::CursorIcon::PointingHand).context_menu(|ui| {
                                if ui.button("Copy hash").clicked() {
                                    ui.ctx().copy_text(hash.clone());
                                    ui.close_menu();
                                }
                                if ui.button("Check out this commit (detached)").clicked() {
                                    acts.push(Action::Checkout(hash.clone()));
                                    ui.close_menu();
                                }
                            });
                        }
                    }
                });
        });
    });
}

fn log_row(line: &LogLine, font: &FontId, bg: Option<Color32>, text: Color32, weak: Color32) -> LayoutJob {
    let bg = bg.unwrap_or(Color32::TRANSPARENT);
    let fmt = |color: Color32| TextFormat { font_id: font.clone(), color, background: bg, ..Default::default() };
    let mut job = LayoutJob::default();
    match &line.commit {
        Some(c) => {
            job.append(&format!("{:<10}", c.short), 0.0, fmt(YELLOW));
            job.append(&format!("{:<18}", c.date), 0.0, fmt(weak));
            job.append(&format!("{:<18}", truncate(&c.author, 16)), 0.0, fmt(weak));
        }
        None => job.append(&" ".repeat(46), 0.0, fmt(weak)),
    }
    for (col, ch) in line.graph.chars().enumerate() {
        let color = if ch == ' ' { weak } else { GRAPH_COLORS[(col / 2) % GRAPH_COLORS.len()] };
        job.append(&ch.to_string(), 0.0, fmt(color));
    }
    if let Some(c) = &line.commit {
        for r in c.refs.split(", ").filter(|r| !r.is_empty()) {
            let color = if r.starts_with("HEAD") {
                GREEN
            } else if r.starts_with("tag:") {
                YELLOW
            } else if r.contains('/') {
                MAGENTA
            } else {
                BLUE
            };
            job.append(&format!("[{r}]"), 0.0, fmt(color));
            job.append(" ", 0.0, fmt(text));
        }
        job.append(&c.subject, 0.0, fmt(text));
    }
    job
}

fn changes_ui(ui: &mut egui::Ui, view: &mut RepoView, acts: &mut Vec<Action>) {
    let files = view.status.as_ref().map(|s| s.files.clone()).unwrap_or_default();
    let staged: Vec<&FileStatus> = files.iter().filter(|f| f.has_staged()).collect();
    let unstaged: Vec<&FileStatus> = files.iter().filter(|f| f.has_unstaged()).collect();

    egui::SidePanel::left("changes_files")
        .resizable(true)
        .default_width(420.0)
        .show_inside(ui, |ui| {
            egui::TopBottomPanel::bottom("commit_box").show_inside(ui, |ui| {
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::multiline(&mut view.commit_msg)
                        .hint_text("Commit message")
                        .desired_rows(4)
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    let can_commit = !view.commit_msg.trim().is_empty() && (!staged.is_empty() || view.amend);
                    let label = if view.amend {
                        "Amend last commit".to_string()
                    } else {
                        format!("Commit {} file(s)", staged.len())
                    };
                    if ui.add_enabled(can_commit, egui::Button::new(label)).clicked() {
                        acts.push(Action::Commit);
                    }
                    ui.checkbox(&mut view.amend, "Amend");
                });
                ui.add_space(4.0);
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(format!("Staged ({})", staged.len()));
                    if !staged.is_empty() && ui.small_button("Unstage all").clicked() {
                        acts.push(Action::Unstage(staged.iter().map(|f| f.path.clone()).collect()));
                    }
                });
                for f in &staged {
                    file_row(ui, f, true, view.selected_file.as_ref(), acts);
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.strong(format!("Unstaged ({})", unstaged.len()));
                    if !unstaged.is_empty() && ui.small_button("Stage all").clicked() {
                        acts.push(Action::Stage(unstaged.iter().map(|f| f.path.clone()).collect()));
                    }
                });
                for f in &unstaged {
                    file_row(ui, f, false, view.selected_file.as_ref(), acts);
                }
                if files.is_empty() {
                    ui.add_space(10.0);
                    ui.weak("Working tree clean.");
                }
            });
        });
    egui::CentralPanel::default().show_inside(ui, |ui| match &view.selected_file {
        Some((path, staged)) => {
            ui.horizontal(|ui| {
                ui.strong(path);
                ui.weak(if *staged { "staged" } else { "unstaged" });
            });
            ui.separator();
            text_view(ui, "file_diff_text", &view.file_diff);
        }
        None => {
            ui.weak("Select a file to see its diff.");
        }
    });
}

fn file_row(
    ui: &mut egui::Ui,
    f: &FileStatus,
    staged: bool,
    selected: Option<&(String, bool)>,
    acts: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        if staged {
            if ui.small_button(" - ").on_hover_text("Unstage").clicked() {
                acts.push(Action::Unstage(vec![f.path.clone()]));
            }
        } else if ui.small_button(" + ").on_hover_text("Stage").clicked() {
            acts.push(Action::Stage(vec![f.path.clone()]));
        }
        let code = if f.conflicted {
            'U'
        } else if staged {
            f.index
        } else {
            f.worktree
        };
        ui.label(RichText::new(code.to_string()).monospace().strong().color(status_color(code)));
        let is_sel = selected.is_some_and(|(p, s)| p == &f.path && *s == staged);
        let mut label = f.path.clone();
        if let Some(orig) = &f.orig_path {
            label = format!("{orig} → {label}");
        }
        let resp = ui.selectable_label(is_sel, label).on_hover_text("Right-click for more");
        if resp.clicked() {
            acts.push(Action::SelectFile(f.path.clone(), staged));
        }
        resp.context_menu(|ui| {
            if staged {
                if ui.button("Unstage").clicked() {
                    acts.push(Action::Unstage(vec![f.path.clone()]));
                    ui.close_menu();
                }
            } else {
                if ui.button(if f.conflicted { "Mark resolved (stage)" } else { "Stage" }).clicked() {
                    acts.push(Action::Stage(vec![f.path.clone()]));
                    ui.close_menu();
                }
                if !f.conflicted && ui.button(RichText::new("Discard changes…").color(RED)).clicked() {
                    let what = if f.is_untracked() { "Delete untracked file" } else { "Discard all unstaged changes to" };
                    acts.push(Action::Confirm(
                        "Discard changes".into(),
                        format!("{what} {}?\nThis cannot be undone.", f.path),
                        Box::new(Action::Discard(f.clone())),
                    ));
                    ui.close_menu();
                }
            }
            if ui.button("Copy path").clicked() {
                ui.ctx().copy_text(f.path.clone());
                ui.close_menu();
            }
        });
    });
}

fn branches_ui(ui: &mut egui::Ui, view: &mut RepoView, acts: &mut Vec<Action>) {
    egui::CentralPanel::default().show_inside(ui, |ui| {
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("New branch from HEAD:");
                let resp = ui.add(egui::TextEdit::singleline(&mut view.new_branch).desired_width(220.0));
                let name = view.new_branch.trim().to_string();
                let valid = !name.is_empty() && !name.contains(char::is_whitespace);
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.add_enabled(valid, egui::Button::new("Create & check out")).clicked() || enter) && valid {
                    acts.push(Action::CreateBranch(name));
                }
            });
            ui.add_space(8.0);

            let head = view.branches.iter().find(|b| b.is_head).map(|b| b.name.clone());
            ui.heading("Local branches");
            egui::Grid::new("local_branches").striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                for b in view.branches.iter().filter(|b| !b.is_remote) {
                    let name = if b.is_head {
                        RichText::new(format!("● {}", b.name)).strong().color(GREEN)
                    } else {
                        RichText::new(&b.name)
                    };
                    ui.label(name.monospace());
                    let track = if b.upstream.is_empty() {
                        String::new()
                    } else {
                        format!("{} {}", b.upstream, b.track)
                    };
                    ui.weak(track);
                    ui.monospace(&b.short_hash);
                    ui.weak(&b.date);
                    ui.label(truncate(&b.subject, 60));
                    ui.horizontal(|ui| {
                        if !b.is_head {
                            if ui.small_button("Check out").clicked() {
                                acts.push(Action::Checkout(b.name.clone()));
                            }
                            if let Some(h) = &head {
                                if ui.small_button("Merge").on_hover_text(format!("Merge {} into {h}", b.name)).clicked() {
                                    acts.push(Action::Confirm(
                                        "Merge branch".into(),
                                        format!("Merge {} into {h}?", b.name),
                                        Box::new(Action::Merge(b.name.clone())),
                                    ));
                                }
                            }
                            if ui.small_button("Delete").clicked() {
                                acts.push(Action::Confirm(
                                    "Delete branch".into(),
                                    format!("Delete local branch {}?\n(Fails if it isn't fully merged.)", b.name),
                                    Box::new(Action::DeleteBranch(b.name.clone(), false)),
                                ));
                            }
                        }
                    });
                    ui.end_row();
                }
            });

            ui.add_space(12.0);
            ui.heading("Remote branches");
            egui::Grid::new("remote_branches").striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                for b in view.branches.iter().filter(|b| b.is_remote) {
                    ui.label(RichText::new(&b.name).monospace().color(MAGENTA));
                    ui.monospace(&b.short_hash);
                    ui.weak(&b.date);
                    ui.label(truncate(&b.subject, 60));
                    ui.horizontal(|ui| {
                        if ui.small_button("Check out").on_hover_text("Create a local tracking branch").clicked() {
                            acts.push(Action::CheckoutRemote(b.name.clone()));
                        }
                        if let Some(h) = &head {
                            if ui.small_button("Merge").on_hover_text(format!("Merge {} into {h}", b.name)).clicked() {
                                acts.push(Action::Confirm(
                                    "Merge branch".into(),
                                    format!("Merge {} into {h}?", b.name),
                                    Box::new(Action::Merge(b.name.clone())),
                                ));
                            }
                        }
                    });
                    ui.end_row();
                }
            });

            ui.add_space(12.0);
            ui.heading(format!("Stashes ({})", view.stashes.len()));
            egui::Grid::new("stashes").striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                for s in &view.stashes {
                    ui.monospace(&s.name);
                    ui.label(&s.subject);
                    ui.horizontal(|ui| {
                        if ui.small_button("Apply").clicked() {
                            acts.push(Action::StashApply(s.name.clone(), false));
                        }
                        if ui.small_button("Pop").clicked() {
                            acts.push(Action::StashApply(s.name.clone(), true));
                        }
                        if ui.small_button("Drop").clicked() {
                            acts.push(Action::Confirm(
                                "Drop stash".into(),
                                format!("Drop {} ({})?", s.name, s.subject),
                                Box::new(Action::StashDrop(s.name.clone())),
                            ));
                        }
                    });
                    ui.end_row();
                }
            });
        });
    });
}

// ----------------------------------------------------------------- helpers

/// Virtualised, colourised view of diff / `git show` output.
fn text_view(ui: &mut egui::Ui, id: &str, lines: &[String]) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.text_style_height(&TextStyle::Monospace);
    let text = ui.visuals().text_color();
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);
        egui::ScrollArea::both()
            .id_salt(id)
            .auto_shrink([false, false])
            .show_rows(ui, row_h, lines.len(), |ui, range| {
                for line in &lines[range] {
                    let color = if line.starts_with("+++") || line.starts_with("---") {
                        text
                    } else if line.starts_with('+') {
                        GREEN
                    } else if line.starts_with('-') {
                        RED
                    } else if line.starts_with("@@") {
                        BLUE
                    } else if line.starts_with("commit ") || line.starts_with("diff ") {
                        YELLOW
                    } else {
                        text
                    };
                    ui.label(RichText::new(line).font(font.clone()).color(color));
                }
            });
    });
}

fn modal(title: &str) -> egui::Window<'static> {
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
}

fn status_color(code: char) -> Color32 {
    match code {
        'A' | '?' => GREEN,
        'D' => RED,
        'M' | 'T' => YELLOW,
        'R' | 'C' => BLUE,
        _ => MAGENTA,
    }
}

fn repo_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn open_folder(path: &Path) {
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    let _ = std::process::Command::new(program).arg(path).spawn();
}
