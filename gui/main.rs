use gtk::{gio, glib};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const APP_ID: &str = "de.simplehf.SimpleHF";
const REPOSITORY_URL: &str = "https://github.com/openresearchtools/simplehf";
const MIN_WINDOW_WIDTH: i32 = 800;
const MIN_WINDOW_HEIGHT: i32 = 600;

fn show_about(window: &adw::ApplicationWindow) {
    let dialog = gtk::AboutDialog::builder()
        .transient_for(window)
        .modal(true)
        .program_name("SimpleHF")
        .version(env!("CARGO_PKG_VERSION"))
        .copyright("© 2026 openresearchtools")
        .comments("Select and download files from Hugging Face model repositories")
        .website(REPOSITORY_URL)
        .website_label("SimpleHF on GitHub")
        .logo_icon_name(APP_ID)
        .license_type(gtk::License::MitX11)
        .authors(["openresearchtools"])
        .build();
    dialog.add_credit_section(
        "Third-party software",
        &[
            "rust-hf-downloader — Johannes Bertens (MIT)\nhttps://github.com/JohannesBertens/rust-hf-downloader",
        ],
    );
    dialog.add_credit_section(
        "Dependency licenses",
        &[
            "Complete per-crate notices: /usr/share/doc/simplehf/cargo-licenses/",
            "SimpleHF notices: /usr/share/doc/simplehf/THIRD_PARTY_NOTICES.md",
        ],
    );
    dialog.present();
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RepoFile {
    path: String,
    size: Option<u64>,
    sha256: Option<String>,
}

#[derive(Clone, Default)]
struct Node {
    name: String,
    path: String,
    size: u64,
    file: Option<RepoFile>,
    selected: bool,
    expanded: bool,
    children: Vec<Node>,
}

impl Node {
    fn directory(name: String, path: String) -> Self {
        Self {
            name,
            path,
            selected: true,
            ..Default::default()
        }
    }
    fn insert(&mut self, parts: &[&str], full_path: &str, file: &RepoFile) {
        if parts.is_empty() {
            return;
        }
        if parts.len() == 1 {
            self.children.push(Node {
                name: parts[0].to_string(),
                path: full_path.to_string(),
                size: file.size.unwrap_or(0),
                file: Some(file.clone()),
                selected: true,
                ..Default::default()
            });
            return;
        }
        let consumed = full_path.len() - parts[1..].join("/").len() - 1;
        let current_path = &full_path[..consumed];
        let position = self
            .children
            .iter()
            .position(|child| child.file.is_none() && child.name == parts[0]);
        let index = match position {
            Some(index) => index,
            None => {
                self.children.push(Node::directory(
                    parts[0].to_string(),
                    current_path.to_string(),
                ));
                self.children.len() - 1
            }
        };
        self.children[index].insert(&parts[1..], full_path, file);
    }
    fn finish(&mut self) -> u64 {
        for child in &mut self.children {
            child.finish();
        }
        self.children
            .sort_by(|a, b| match (a.file.is_none(), b.file.is_none()) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });
        if self.file.is_none() {
            self.size = self.children.iter().map(|child| child.size).sum();
        }
        self.size
    }
    fn set_selected(&mut self, selected: bool) {
        self.selected = selected;
        for child in &mut self.children {
            child.set_selected(selected);
        }
    }
    fn selection(&self) -> (usize, usize) {
        if self.file.is_some() {
            return (usize::from(self.selected), 1);
        }
        self.children
            .iter()
            .map(Node::selection)
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    }
    fn selected_size(&self) -> u64 {
        if self.file.is_some() {
            return if self.selected { self.size } else { 0 };
        }
        self.children.iter().map(Node::selected_size).sum()
    }
    fn collect(&self, output: &mut Vec<RepoFile>) {
        if let Some(file) = &self.file {
            if self.selected {
                output.push(file.clone());
            }
        }
        for child in &self.children {
            child.collect(output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> RepoFile {
        RepoFile {
            path: path.into(),
            size: Some(size),
            sha256: None,
        }
    }

    #[test]
    fn repository_tree_preserves_nested_paths_and_sizes() {
        let mut root = Node::directory("root".into(), String::new());
        for item in [
            file("config.json", 10),
            file("weights/a/model.safetensors", 90),
        ] {
            let parts: Vec<_> = item.path.split('/').collect();
            root.insert(&parts, &item.path, &item);
        }
        assert_eq!(root.finish(), 100);
        assert_eq!(root.children[0].name, "weights");
        assert_eq!(root.children[0].children[0].path, "weights/a");
        let mut selected = Vec::new();
        root.collect(&mut selected);
        assert_eq!(
            selected.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            ["weights/a/model.safetensors", "config.json"]
        );
    }

    #[test]
    fn selecting_a_folder_controls_only_its_descendants() {
        let mut root = Node::directory("root".into(), String::new());
        for item in [file("one/a.bin", 1), file("two/b.bin", 1)] {
            let parts: Vec<_> = item.path.split('/').collect();
            root.insert(&parts, &item.path, &item);
        }
        root.finish();
        root.children[0].set_selected(false);
        assert_eq!(root.selection(), (1, 2));
        assert_eq!(root.selected_size(), 1);
        let mut selected = Vec::new();
        root.collect(&mut selected);
        assert_eq!(selected[0].path, "two/b.bin");
    }

    #[test]
    fn durations_are_compact_and_human_readable() {
        assert_eq!(format_duration(9.6), "10s");
        assert_eq!(format_duration(125.0), "2m 5s");
        assert_eq!(format_duration(7380.0), "2h 3m");
    }
}

#[derive(Clone, Default)]
struct Repository {
    id: String,
    root: Node,
    gated: bool,
}

#[derive(Default)]
struct AppState {
    repository: Option<Repository>,
    jobs: Vec<Rc<RefCell<Job>>>,
    current_job: Option<usize>,
}

#[derive(Clone)]
struct FileProgress {
    path: String,
    status: String,
    downloaded: u64,
    total: u64,
    speed: f64,
    error: Option<String>,
    last_bytes: u64,
    last_update: Instant,
}

struct Job {
    status: String,
    files: Vec<FileProgress>,
    row_status: gtk::Label,
    row_progress: gtk::ProgressBar,
    row_pause: gtk::Button,
    row_cancel: gtk::Button,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum EngineEvent {
    Job {
        status: String,
        error: Option<String>,
    },
    File {
        index: usize,
        status: String,
        downloaded: u64,
        total: u64,
        error: Option<String>,
    },
}

#[derive(Deserialize)]
struct SearchModel {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    #[serde(default)]
    gated: Value,
}

fn client(token: &str) -> Result<reqwest::Client, String> {
    let mut headers = HeaderMap::new();
    if !token.trim().is_empty() {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token.trim()))
                .map_err(|e| e.to_string())?,
        );
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .user_agent("SimpleHF/0.2")
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|e| e.to_string())
}

async fn search_hub(query: String, token: String) -> Result<Vec<SearchModel>, String> {
    let response = client(&token)?
        .get("https://huggingface.co/api/models")
        .query(&[
            ("search", query),
            ("limit", "50".into()),
            ("sort", "downloads".into()),
            ("direction", "-1".into()),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    response.json().await.map_err(|e| e.to_string())
}

async fn load_repository(id: String, token: String) -> Result<Repository, String> {
    let pieces: Vec<_> = id.trim().trim_matches('/').split('/').collect();
    if pieces.len() != 2
        || pieces
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err("Use organization/model".into());
    }
    let id = format!("{}/{}", pieces[0], pieces[1]);
    let http = client(&token)?;
    let metadata: Value = http
        .get(format!("https://huggingface.co/api/models/{id}"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let mut url = Some(format!(
        "https://huggingface.co/api/models/{id}/tree/main?recursive=true&expand=false"
    ));
    let mut files = Vec::new();
    while let Some(page) = url.take() {
        let response = http
            .get(page)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        let link = response
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let entries: Vec<Value> = response.json().await.map_err(|e| e.to_string())?;
        for entry in entries {
            if entry["type"] != "file" {
                continue;
            }
            let Some(path) = entry["path"].as_str() else {
                continue;
            };
            files.push(RepoFile {
                path: path.to_string(),
                size: entry["size"].as_u64(),
                sha256: entry
                    .get("lfs")
                    .and_then(|lfs| lfs.get("oid"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        for part in link.split(',') {
            if part.contains("rel=\"next\"") {
                if let (Some(start), Some(end)) = (part.find('<'), part.find('>')) {
                    url = Some(part[start + 1..end].to_string());
                }
            }
        }
    }
    let mut root = Node::directory(id.clone(), String::new());
    root.expanded = true;
    for file in files {
        let parts: Vec<_> = file.path.split('/').collect();
        root.insert(&parts, &file.path, &file);
    }
    root.finish();
    Ok(Repository {
        id,
        root,
        gated: metadata["gated"].as_bool().unwrap_or(false) || metadata["gated"].as_str().is_some(),
    })
}

fn format_bytes(value: f64) -> String {
    let mut amount = value;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if amount < 1024.0 || unit == "TiB" {
            return if unit == "B" {
                format!("{amount:.0} {unit}")
            } else {
                format!("{amount:.1} {unit}")
            };
        }
        amount /= 1024.0;
    }
    unreachable!()
}

fn format_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn send_engine_command(input: &Arc<Mutex<Option<ChildStdin>>>, command: &str) -> bool {
    input
        .lock()
        .ok()
        .and_then(|mut input| {
            let input = input.as_mut()?;
            serde_json::to_writer(&mut *input, &serde_json::json!({"command": command})).ok()?;
            input.write_all(b"\n").ok()?;
            input.flush().ok()?;
            Some(())
        })
        .is_some()
}

fn keep_paned_proportion(paned: &gtk::Paned, initial: f64) {
    let proportion = Rc::new(Cell::new(initial));
    let adjusting = Rc::new(Cell::new(false));
    {
        let proportion = proportion.clone();
        let adjusting = adjusting.clone();
        paned.connect_position_notify(move |paned| {
            if adjusting.get() {
                return;
            }
            let maximum = paned.max_position();
            if maximum > 0 {
                proportion.set(paned.position() as f64 / maximum as f64);
            }
        });
    }
    paned.connect_max_position_notify(move |paned| {
        let maximum = paned.max_position();
        if maximum <= 0 {
            return;
        }
        adjusting.set(true);
        paned.set_position((maximum as f64 * proportion.get()).round() as i32);
        adjusting.set(false);
    });
}

fn clear_list(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn show_error(window: &adw::ApplicationWindow, message: &str) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(window)
        .modal(true)
        .text("SimpleHF")
        .secondary_text(message)
        .buttons(gtk::ButtonsType::Close)
        .build();
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
}

fn node_mut<'a>(mut node: &'a mut Node, indices: &[usize]) -> &'a mut Node {
    for index in indices {
        node = &mut node.children[*index];
    }
    node
}

fn render_tree(
    list: &gtk::ListBox,
    state: Rc<RefCell<AppState>>,
    selection_label: &gtk::Label,
    download: &gtk::Button,
) {
    clear_list(list);
    let Some(repository) = state.borrow().repository.clone() else {
        return;
    };
    fn add_nodes(
        list: &gtk::ListBox,
        nodes: &[Node],
        depth: i32,
        prefix: Vec<usize>,
        ancestor_has_next: Vec<bool>,
        state: Rc<RefCell<AppState>>,
        label: gtk::Label,
        download: gtk::Button,
    ) {
        for (index, node) in nodes.iter().enumerate() {
            let is_last = index + 1 == nodes.len();
            let mut indices = prefix.clone();
            indices.push(index);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_margin_start(8 + depth * 24);
            row.set_margin_end(8);
            row.set_margin_top(4);
            row.set_margin_bottom(4);
            if depth > 0 {
                let mut guide = String::new();
                for has_next in &ancestor_has_next {
                    guide.push_str(if *has_next { "│   " } else { "    " });
                }
                guide.push_str(if is_last { "└─" } else { "├─" });
                let guide = gtk::Label::new(Some(&guide));
                guide.add_css_class("dim-label");
                guide.set_xalign(0.0);
                row.append(&guide);
            }
            if node.file.is_none() {
                let expand = gtk::Button::from_icon_name(if node.expanded {
                    "pan-down-symbolic"
                } else {
                    "pan-end-symbolic"
                });
                expand.add_css_class("flat");
                let state2 = state.clone();
                let list2 = list.clone();
                let label2 = label.clone();
                let download2 = download.clone();
                let path = indices.clone();
                expand.connect_clicked(move |_| {
                    if let Some(repo) = &mut state2.borrow_mut().repository {
                        let item = node_mut(&mut repo.root, &path);
                        item.expanded = !item.expanded;
                    }
                    render_tree(&list2, state2.clone(), &label2, &download2);
                });
                row.append(&expand);
            } else {
                row.append(&gtk::Image::from_icon_name("text-x-generic-symbolic"));
            }
            let check = gtk::CheckButton::new();
            let (selected, total) = node.selection();
            check.set_active(selected == total);
            check.set_inconsistent(selected > 0 && selected < total);
            let state2 = state.clone();
            let list2 = list.clone();
            let label2 = label.clone();
            let download2 = download.clone();
            let path = indices.clone();
            check.connect_toggled(move |check| {
                if let Some(repo) = &mut state2.borrow_mut().repository {
                    node_mut(&mut repo.root, &path).set_selected(check.is_active());
                }
                render_tree(&list2, state2.clone(), &label2, &download2);
            });
            row.append(&check);
            let icon = gtk::Image::from_icon_name(if node.file.is_none() {
                "folder-symbolic"
            } else {
                "text-x-generic-symbolic"
            });
            row.append(&icon);
            let name = gtk::Label::new(Some(&node.name));
            name.set_xalign(0.0);
            name.set_hexpand(true);
            name.set_tooltip_text(Some(&node.path));
            row.append(&name);
            let size = gtk::Label::new(Some(&format_bytes(node.size as f64)));
            size.add_css_class("dim-label");
            row.append(&size);
            list.append(&row);
            if node.file.is_none() && node.expanded {
                let mut child_ancestor_has_next = ancestor_has_next.clone();
                if depth > 0 {
                    child_ancestor_has_next.push(!is_last);
                }
                add_nodes(
                    list,
                    &node.children,
                    depth + 1,
                    indices,
                    child_ancestor_has_next,
                    state.clone(),
                    label.clone(),
                    download.clone(),
                );
            }
        }
    }
    add_nodes(
        list,
        &repository.root.children,
        0,
        vec![],
        vec![],
        state.clone(),
        selection_label.clone(),
        download.clone(),
    );
    let (selected, total) = repository.root.selection();
    selection_label.set_text(&format!(
        "{selected} of {total} files selected • {} selected • {} repository",
        format_bytes(repository.root.selected_size() as f64),
        format_bytes(repository.root.size as f64),
    ));
    download.set_sensitive(selected > 0);
}

fn poll_result<T: 'static>(
    rx: mpsc::Receiver<Result<T, String>>,
    callback: impl FnOnce(Result<T, String>) + 'static,
) {
    let callback = Rc::new(RefCell::new(Some(callback)));
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(result) => {
            if let Some(callback) = callback.borrow_mut().take() {
                callback(result);
            }
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(_) => glib::ControlFlow::Break,
    });
}

fn spawn_request<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) -> mpsc::Receiver<Result<T, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
            .and_then(|rt| rt.block_on(future));
        let _ = tx.send(result);
    });
    rx
}

fn render_details(list: &gtk::ListBox, job: &Job) {
    clear_list(list);
    for file in &job.files {
        let eta = if file.speed > 0.0 && file.downloaded < file.total {
            format!(
                " • {} remaining",
                format_duration((file.total - file.downloaded) as f64 / file.speed)
            )
        } else {
            String::new()
        };
        let row = gtk::ListBoxRow::new();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        let title = gtk::Label::new(Some(&file.path));
        title.set_xalign(0.0);
        title.set_tooltip_text(Some(&file.path));
        title.add_css_class("heading");
        content.append(&title);
        let subtitle = gtk::Label::new(Some(&format!(
            "{} • {} / {}{}{}{}",
            file.status,
            format_bytes(file.downloaded as f64),
            format_bytes(file.total as f64),
            if file.speed > 0.0 {
                format!(" • {}/s", format_bytes(file.speed))
            } else {
                String::new()
            },
            eta,
            file.error
                .as_ref()
                .map(|e| format!(" • {e}"))
                .unwrap_or_default()
        )));
        subtitle.set_xalign(0.0);
        subtitle.add_css_class("dim-label");
        content.append(&subtitle);
        let progress = gtk::ProgressBar::new();
        progress.set_fraction(if file.total > 0 {
            file.downloaded as f64 / file.total as f64
        } else {
            0.0
        });
        content.append(&progress);
        row.set_child(Some(&content));
        list.append(&row);
    }
}

fn start_download(
    repo: Repository,
    files: Vec<RepoFile>,
    destination: PathBuf,
    token: String,
    state: Rc<RefCell<AppState>>,
    jobs_list: gtk::ListBox,
    details: gtk::ListBox,
) {
    let row = gtk::ListBoxRow::new();
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
    box_.set_margin_start(10);
    box_.set_margin_end(10);
    box_.set_margin_top(8);
    box_.set_margin_bottom(8);
    let title = gtk::Label::new(Some(&repo.id));
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    title.set_tooltip_text(Some(&repo.id));
    title.add_css_class("heading");
    let pause = gtk::Button::from_icon_name("media-playback-pause-symbolic");
    pause.add_css_class("flat");
    pause.set_tooltip_text(Some("Pause download"));
    let cancel = gtk::Button::from_icon_name("process-stop-symbolic");
    cancel.add_css_class("flat");
    cancel.set_tooltip_text(Some("Cancel download"));
    let title_line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    title.set_hexpand(true);
    title_line.append(&title);
    title_line.append(&pause);
    title_line.append(&cancel);
    let status = gtk::Label::new(Some("Queued"));
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    let progress = gtk::ProgressBar::new();
    box_.append(&title_line);
    box_.append(&status);
    box_.append(&progress);
    row.set_child(Some(&box_));
    jobs_list.append(&row);
    let process: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let control_input: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(None));
    let job = Rc::new(RefCell::new(Job {
        status: "Queued".into(),
        files: files
            .iter()
            .map(|f| FileProgress {
                path: f.path.clone(),
                status: "Pending".into(),
                downloaded: 0,
                total: f.size.unwrap_or(0),
                speed: 0.0,
                error: None,
                last_bytes: 0,
                last_update: Instant::now(),
            })
            .collect(),
        row_status: status,
        row_progress: progress,
        row_pause: pause.clone(),
        row_cancel: cancel.clone(),
    }));
    {
        let input = control_input.clone();
        let paused = Rc::new(Cell::new(false));
        pause.connect_clicked(move |button| {
            let next = !paused.get();
            if send_engine_command(&input, if next { "pause" } else { "resume" }) {
                paused.set(next);
                button.set_icon_name(if next {
                    "media-playback-start-symbolic"
                } else {
                    "media-playback-pause-symbolic"
                });
                button.set_tooltip_text(Some(if next {
                    "Resume download"
                } else {
                    "Pause download"
                }));
            }
        });
    }
    {
        let process = process.clone();
        cancel.connect_clicked(move |button| {
            if let Some(child) = process
                .lock()
                .ok()
                .and_then(|mut slot| slot.as_mut().map(|child| child.id()))
            {
                let _ = Command::new("kill")
                    .arg("-TERM")
                    .arg(child.to_string())
                    .status();
            }
            button.set_sensitive(false);
        });
    }
    let index = state.borrow().jobs.len();
    state.borrow_mut().jobs.push(job.clone());
    state.borrow_mut().current_job = Some(index);
    jobs_list.select_row(Some(&row));
    render_details(&details, &job.borrow());
    let (tx, rx) = mpsc::channel::<EngineEvent>();
    let process_for_worker = process.clone();
    let input_for_worker = control_input.clone();
    std::thread::spawn(move || {
        let engine = std::env::var("SIMPLEHF_ENGINE")
            .map(PathBuf::from)
            .ok()
            .filter(|p| p.exists())
            .or_else(|| {
                let mut candidates = vec![
                    PathBuf::from("/usr/libexec/simplehf-engine"),
                    PathBuf::from("target/release/simplehf-engine"),
                ];
                if let Some(home) = std::env::var_os("HOME") {
                    candidates.insert(
                        1,
                        PathBuf::from(home).join(".local/libexec/simplehf-engine"),
                    );
                }
                candidates.into_iter().find(|p| p.exists())
            });
        let Some(engine) = engine else {
            let _ = tx.send(EngineEvent::Job {
                status: "failed".into(),
                error: Some("simplehf-engine not installed".into()),
            });
            return;
        };
        let mut command = Command::new(engine);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if !token.is_empty() {
            command.env("HF_TOKEN", token);
        }
        let Ok(mut child) = command.spawn() else {
            let _ = tx.send(EngineEvent::Job {
                status: "failed".into(),
                error: Some("could not start engine".into()),
            });
            return;
        };
        let manifest = serde_json::json!({"repo_id": repo.id, "destination": destination, "connections": 8, "files": files});
        if let Some(mut stdin) = child.stdin.take() {
            let _ = serde_json::to_writer(&mut stdin, &manifest);
            let _ = stdin.write_all(b"\n");
            let _ = stdin.flush();
            if let Ok(mut input) = input_for_worker.lock() {
                *input = Some(stdin);
            }
        }
        let stdout = child.stdout.take();
        if let Ok(mut slot) = process_for_worker.lock() {
            *slot = Some(child);
        }
        if let Some(stdout) = stdout {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if let Ok(event) = serde_json::from_str(&line) {
                    let _ = tx.send(event);
                }
            }
        }
        let mut cancelled = false;
        if let Ok(mut slot) = process_for_worker.lock() {
            if let Some(child) = slot.as_mut() {
                cancelled = child.wait().map(|status| !status.success()).unwrap_or(true);
            }
            *slot = None;
        }
        if let Ok(mut input) = input_for_worker.lock() {
            *input = None;
        }
        if cancelled {
            let _ = tx.send(EngineEvent::Job {
                status: "cancelled".into(),
                error: None,
            });
        }
    });
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let mut changed = false;
        while let Ok(event) = rx.try_recv() {
            changed = true;
            let mut job = job.borrow_mut();
            match event {
                EngineEvent::Job { status, error } => {
                    job.status = status.clone();
                    for file in &mut job.files {
                        if status == "paused" && file.status == "downloading" {
                            file.status = "paused".into();
                            file.speed = 0.0;
                        } else if status == "downloading" && file.status == "paused" {
                            file.status = "downloading".into();
                            file.last_bytes = file.downloaded;
                            file.last_update = Instant::now();
                        }
                    }
                    job.row_status.set_text(&format!(
                        "{}{}",
                        status,
                        error.map(|e| format!(" • {e}")).unwrap_or_default()
                    ));
                }
                EngineEvent::File {
                    index,
                    status,
                    downloaded,
                    total,
                    error,
                } => {
                    if let Some(file) = job.files.get_mut(index) {
                        let now = Instant::now();
                        let elapsed = now.duration_since(file.last_update).as_secs_f64();
                        file.speed = if status == "downloading" && elapsed > 0.0 {
                            downloaded.saturating_sub(file.last_bytes) as f64 / elapsed
                        } else {
                            0.0
                        };
                        file.status = status;
                        file.downloaded = downloaded;
                        file.total = total;
                        file.error = error;
                        file.last_bytes = downloaded;
                        file.last_update = now;
                    }
                }
            }
            let downloaded: u64 = job.files.iter().map(|f| f.downloaded).sum();
            let total: u64 = job.files.iter().map(|f| f.total).sum();
            let speed: f64 = job.files.iter().map(|f| f.speed).sum();
            job.row_progress.set_fraction(if total > 0 {
                downloaded as f64 / total as f64
            } else {
                0.0
            });
            if matches!(job.status.as_str(), "Queued" | "downloading" | "paused") {
                let label = match job.status.as_str() {
                    "downloading" => "Downloading",
                    "paused" => "Paused",
                    _ => "Queued",
                };
                let throughput = if speed > 0.0 {
                    format!(" • {}/s", format_bytes(speed))
                } else {
                    String::new()
                };
                let eta = if speed > 0.0 && downloaded < total {
                    format!(
                        " • {} remaining",
                        format_duration((total - downloaded) as f64 / speed)
                    )
                } else {
                    String::new()
                };
                job.row_status.set_text(&format!(
                    "{label} • {} / {}{throughput}{eta}",
                    format_bytes(downloaded as f64),
                    format_bytes(total as f64),
                ));
            }
        }
        if changed {
            render_details(&details, &job.borrow());
        }
        if matches!(
            job.borrow().status.as_str(),
            "complete" | "failed" | "cancelled"
        ) {
            let job = job.borrow();
            job.row_pause.set_sensitive(false);
            job.row_cancel.set_sensitive(false);
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

fn build_window(app: &adw::Application) -> adw::ApplicationWindow {
    let state = Rc::new(RefCell::new(AppState::default()));
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("SimpleHF")
        .default_width(1180)
        .default_height(780)
        .build();
    let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shell.set_size_request(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(
        "SimpleHF",
        "Native Rust Hugging Face downloader",
    )));
    let menu = gio::Menu::new();
    menu.append(Some("About SimpleHF"), Some("app.about"));
    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.set_tooltip_text(Some("Main menu"));
    menu_button.set_menu_model(Some(&menu));
    header.pack_end(&menu_button);
    shell.append(&header);
    let vertical = gtk::Paned::new(gtk::Orientation::Vertical);
    vertical.set_vexpand(true);
    vertical.set_wide_handle(true);
    vertical.set_resize_start_child(true);
    vertical.set_resize_end_child(true);
    vertical.set_shrink_start_child(false);
    vertical.set_shrink_end_child(false);
    shell.append(&vertical);
    let upper = gtk::Paned::new(gtk::Orientation::Horizontal);
    upper.set_wide_handle(true);
    upper.set_resize_start_child(true);
    upper.set_resize_end_child(true);
    upper.set_shrink_start_child(false);
    upper.set_shrink_end_child(false);
    vertical.set_start_child(Some(&upper));

    let discovery = gtk::Box::new(gtk::Orientation::Vertical, 8);
    discovery.set_margin_start(12);
    discovery.set_margin_end(12);
    discovery.set_margin_top(12);
    discovery.set_margin_bottom(12);
    upper.set_start_child(Some(&discovery));
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search Hugging Face"));
    let search_button = gtk::Button::with_label("Search");
    let search_line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    search.set_hexpand(true);
    search_line.append(&search);
    search_line.append(&search_button);
    discovery.append(&search_line);
    let direct = gtk::Entry::new();
    direct.set_placeholder_text(Some("organization/model"));
    let open = gtk::Button::with_label("Open");
    open.add_css_class("suggested-action");
    let direct_line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    direct.set_hexpand(true);
    direct_line.append(&direct);
    direct_line.append(&open);
    discovery.append(&direct_line);
    let token = gtk::PasswordEntry::new();
    token.set_placeholder_text(Some("Hugging Face token (memory only)"));
    token.set_show_peek_icon(true);
    discovery.append(&token);
    let results = gtk::ListBox::new();
    results.add_css_class("boxed-list");
    let results_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .vexpand(true)
        .child(&results)
        .build();
    discovery.append(&results_scroll);
    let repo_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    repo_box.set_margin_start(12);
    repo_box.set_margin_end(12);
    repo_box.set_margin_top(12);
    repo_box.set_margin_bottom(12);
    upper.set_end_child(Some(&repo_box));
    let repo_title = gtk::Label::new(Some("Open a repository"));
    repo_title.set_xalign(0.0);
    repo_title.set_hexpand(true);
    repo_title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    repo_title.add_css_class("title-3");
    let all = gtk::Button::with_label("All");
    let none = gtk::Button::with_label("None");
    let download = gtk::Button::with_label("Add to Downloads");
    download.add_css_class("suggested-action");
    download.set_sensitive(false);
    repo_box.append(&repo_title);
    let selection_label = gtk::Label::new(None);
    selection_label.set_xalign(0.0);
    selection_label.set_hexpand(true);
    selection_label.add_css_class("dim-label");
    repo_box.append(&selection_label);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.append(&all);
    actions.append(&none);
    actions.append(&download);
    repo_box.append(&actions);
    let tree = gtk::ListBox::new();
    tree.set_selection_mode(gtk::SelectionMode::None);
    let tree_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .vexpand(true)
        .child(&tree)
        .build();
    repo_box.append(&tree_scroll);
    let lower = gtk::Paned::new(gtk::Orientation::Horizontal);
    lower.set_wide_handle(true);
    lower.set_resize_start_child(true);
    lower.set_resize_end_child(true);
    lower.set_shrink_start_child(false);
    lower.set_shrink_end_child(false);
    vertical.set_end_child(Some(&lower));
    let jobs = gtk::ListBox::new();
    jobs.add_css_class("boxed-list");
    let jobs_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .vexpand(true)
        .child(&jobs)
        .build();
    lower.set_start_child(Some(&jobs_scroll));
    let details = gtk::ListBox::new();
    details.add_css_class("boxed-list");
    let details_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .vexpand(true)
        .child(&details)
        .build();
    lower.set_end_child(Some(&details_scroll));

    keep_paned_proportion(&vertical, 0.64);
    keep_paned_proportion(&upper, 0.3);
    keep_paned_proportion(&lower, 0.4);

    window.set_content(Some(&shell));

    let load = Rc::new({
        let window = window.clone();
        let state = state.clone();
        let tree = tree.clone();
        let title = repo_title.clone();
        let label = selection_label.clone();
        let download = download.clone();
        let token = token.clone();
        move |id: String| {
            title.set_text("Loading…");
            let rx = spawn_request(load_repository(id, token.text().to_string()));
            let window = window.clone();
            let state = state.clone();
            let tree = tree.clone();
            let title = title.clone();
            let label = label.clone();
            let download = download.clone();
            poll_result(rx, move |result| match result {
                Ok(repo) => {
                    title.set_text(&format!(
                        "{}{}",
                        repo.id,
                        if repo.gated { " • gated" } else { "" }
                    ));
                    state.borrow_mut().repository = Some(repo);
                    render_tree(&tree, state, &label, &download);
                }
                Err(error) => show_error(&window, &error),
            });
        }
    });
    {
        let load = load.clone();
        let direct = direct.clone();
        open.connect_clicked(move |_| load(direct.text().to_string()));
    }
    {
        let load = load.clone();
        direct.connect_activate(move |entry| load(entry.text().to_string()));
    }
    {
        let results = results.clone();
        let search = search.clone();
        let token = token.clone();
        let window = window.clone();
        let load = load.clone();
        search_button.connect_clicked(move |_| {
            let rx = spawn_request(search_hub(
                search.text().to_string(),
                token.text().to_string(),
            ));
            let results = results.clone();
            let window = window.clone();
            let load = load.clone();
            poll_result(rx, move |result| match result {
                Ok(models) => {
                    clear_list(&results);
                    for model in models {
                        let row = adw::ActionRow::builder()
                            .title(&model.id)
                            .subtitle(format!(
                                "{} downloads • {} likes{}",
                                model.downloads,
                                model.likes,
                                if model.gated != Value::Bool(false) && !model.gated.is_null() {
                                    " • gated"
                                } else {
                                    ""
                                }
                            ))
                            .activatable(true)
                            .build();
                        let id = model.id;
                        let load = load.clone();
                        row.connect_activated(move |_| load(id.clone()));
                        results.append(&row);
                    }
                }
                Err(error) => show_error(&window, &error),
            });
        });
    }
    for (button, selected) in [(all.clone(), true), (none.clone(), false)] {
        let state = state.clone();
        let tree = tree.clone();
        let label = selection_label.clone();
        let download = download.clone();
        button.connect_clicked(move |_| {
            if let Some(repo) = &mut state.borrow_mut().repository {
                repo.root.set_selected(selected);
            }
            render_tree(&tree, state.clone(), &label, &download);
        });
    }
    {
        let state = state.clone();
        let window = window.clone();
        let token = token.clone();
        let jobs = jobs.clone();
        let details = details.clone();
        download.connect_clicked(move |_| {
            let Some(repo) = state.borrow().repository.clone() else {
                return;
            };
            let mut files = Vec::new();
            repo.root.collect(&mut files);
            let chooser = gtk::FileChooserNative::new(
                Some("Choose download folder"),
                Some(&window),
                gtk::FileChooserAction::SelectFolder,
                Some("Download Here"),
                Some("Cancel"),
            );
            let state = state.clone();
            let token = token.clone();
            let jobs = jobs.clone();
            let details = details.clone();
            chooser.connect_response(move |chooser, response| {
                if response == gtk::ResponseType::Accept {
                    if let Some(path) = chooser.file().and_then(|f| f.path()) {
                        start_download(
                            repo.clone(),
                            files.clone(),
                            path,
                            token.text().to_string(),
                            state.clone(),
                            jobs.clone(),
                            details.clone(),
                        );
                    }
                }
            });
            chooser.show();
        });
    }
    {
        let state = state.clone();
        let details = details.clone();
        jobs.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                let index = row.index() as usize;
                state.borrow_mut().current_job = Some(index);
                if let Some(job) = state.borrow().jobs.get(index) {
                    render_details(&details, &job.borrow());
                }
            }
        });
    }
    window
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    let about = gio::SimpleAction::new("about", None);
    let app_weak = app.downgrade();
    about.connect_activate(move |_, _| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(window) = app
            .active_window()
            .and_then(|window| window.downcast::<adw::ApplicationWindow>().ok())
        else {
            return;
        };
        show_about(&window);
    });
    app.add_action(&about);
    app.connect_activate(|app| build_window(app).present());
    app.run()
}
