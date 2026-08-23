use std::{
    env,
    fs::{self, File},
    io,
    os::unix::{fs::PermissionsExt, net::UnixDatagram},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use memsol_core::{
    AttentionContext, AttentionGraph, AttentionLearner, AttentionObservation, ContextEvent,
    ContextLearner, HyprlandEvent, HyprlandEventStream, apply_event, classify_pressure,
    snapshot_attention,
    telemetry::{read_meminfo, read_memory_psi},
};

const SAVE_INTERVAL: Duration = Duration::from_secs(30);

fn main() -> io::Result<()> {
    println!("memsold observer starting; no reclaim or freeze actions are enabled");

    let learner_path = state_path("learning.json");
    let context_path = state_path("context-learning.json");
    let learner = Arc::new(Mutex::new(load_attention_learner(learner_path.as_deref())));
    let contextual = Arc::new(Mutex::new(load_context_learner(context_path.as_deref())));
    let attention = start_attention_observer(Arc::clone(&learner), Arc::clone(&contextual));
    start_context_event_listener(Arc::clone(&attention), Arc::clone(&contextual));
    let mut last_save = Instant::now();

    loop {
        let memory = read_meminfo()?;
        let psi = read_memory_psi()?;
        let level = classify_pressure(memory, psi);
        let now = wall_clock_duration();

        let attention_summary = attention.lock().ok().map(|graph| {
            format!(
                "hypr_windows={} focused={} visible={} hidden={}",
                graph.windows().len(),
                graph.focused_count(),
                graph.visible_count(),
                graph.hidden_count()
            )
        });

        let learning_summary = attention
            .lock()
            .ok()
            .and_then(|graph| {
                learner.lock().ok().map(|model| {
                    contextual.lock().map_or_else(
                        |_| learning_summary(&graph, &model, now),
                        |contextual| combined_learning_summary(&graph, &model, &contextual, now),
                    )
                })
            })
            .unwrap_or_else(|| "learning=unavailable".to_owned());

        println!(
            "pressure={level:?} available={:.1}% psi.some.avg10={:.2} psi.full.avg10={:.2} swap_used_mib={} {} {}",
            memory.available_ratio() * 100.0,
            psi.some.avg10,
            psi.full.avg10,
            memory.swap_used_kib() / 1024,
            attention_summary
                .as_deref()
                .unwrap_or("hyprland=unavailable"),
            learning_summary,
        );

        if last_save.elapsed() >= SAVE_INTERVAL {
            if let (Some(path), Ok(model)) = (learner_path.as_deref(), learner.lock()) {
                if let Err(error) = save_attention_learner(path, &model) {
                    eprintln!("failed to persist attention learning state: {error}");
                }
            }
            if let (Some(path), Ok(model)) = (context_path.as_deref(), contextual.lock()) {
                if let Err(error) = save_context_learner(path, &model) {
                    eprintln!("failed to persist contextual learning state: {error}");
                }
            }
            last_save = Instant::now();
        }

        thread::sleep(Duration::from_secs(5));
    }
}

fn start_attention_observer(
    learner: Arc<Mutex<AttentionLearner>>,
    contextual: Arc<Mutex<ContextLearner>>,
) -> Arc<Mutex<AttentionGraph>> {
    let mut stream = match HyprlandEventStream::connect() {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("Hyprland attention observer unavailable: {error}");
            return Arc::new(Mutex::new(AttentionGraph::default()));
        }
    };

    let graph = match snapshot_attention() {
        Ok(graph) => {
            println!(
                "Hyprland attention observer attached: {} windows across {} workspaces",
                graph.windows().len(),
                graph.workspaces().len()
            );
            graph
        }
        Err(error) => {
            eprintln!("failed to snapshot Hyprland state: {error}");
            return Arc::new(Mutex::new(AttentionGraph::default()));
        }
    };

    let now = wall_clock_duration();
    if let Ok(mut model) = learner.lock() {
        model.observe(observation_from_graph(&graph, now, None));
    }
    if let Ok(mut model) = contextual.lock() {
        model.observe_attention(&attention_context(&graph, None), now);
    }

    let graph = Arc::new(Mutex::new(graph));
    let event_graph = Arc::clone(&graph);

    thread::spawn(move || {
        loop {
            match stream.next_event() {
                Ok(Some(event)) => {
                    let now = wall_clock_duration();
                    if let Ok(mut graph) = event_graph.lock() {
                        apply_event(&mut graph, event.clone());
                        if should_learn_from(&event) {
                            let workspace_hint = workspace_hint(&event);
                            if let Ok(mut model) = learner.lock() {
                                model.observe(observation_from_graph(
                                    &graph,
                                    now,
                                    workspace_hint.clone(),
                                ));
                            }
                            if let Ok(mut model) = contextual.lock() {
                                model.observe_attention(
                                    &attention_context(&graph, workspace_hint),
                                    now,
                                );
                            }
                        }
                    }
                }
                Ok(None) => {
                    eprintln!("Hyprland event socket closed");
                    return;
                }
                Err(error) => {
                    eprintln!("Hyprland event observer failed: {error}");
                    return;
                }
            }
        }
    });

    graph
}

fn start_context_event_listener(
    attention: Arc<Mutex<AttentionGraph>>,
    learner: Arc<Mutex<ContextLearner>>,
) {
    let Some(path) = runtime_event_socket_path() else {
        eprintln!("context event socket disabled: XDG_RUNTIME_DIR unavailable");
        return;
    };

    thread::spawn(move || {
        if let Err(error) = run_context_event_listener(&path, &attention, &learner) {
            eprintln!("context event listener stopped: {error}");
        }
    });
}

fn run_context_event_listener(
    path: &Path,
    attention: &Arc<Mutex<AttentionGraph>>,
    learner: &Arc<Mutex<ContextLearner>>,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        fs::remove_file(path)?;
    }

    let socket = UnixDatagram::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    println!("context event socket listening at {}", path.display());

    let mut buffer = [0_u8; 4096];
    loop {
        let size = socket.recv(&mut buffer)?;
        let event: ContextEvent = match serde_json::from_slice(&buffer[..size]) {
            Ok(event) => event,
            Err(error) => {
                eprintln!("ignoring invalid context event: {error}");
                continue;
            }
        };

        let context = attention
            .lock()
            .map(|graph| attention_context(&graph, None))
            .unwrap_or_default();
        if let Ok(mut model) = learner.lock() {
            model.observe_event(&event, wall_clock_duration(), context);
        }
    }
}

fn should_learn_from(event: &HyprlandEvent) -> bool {
    matches!(
        event,
        HyprlandEvent::WorkspaceChanged { .. }
            | HyprlandEvent::FocusedMonitor { .. }
            | HyprlandEvent::ActiveWindow { .. }
    )
}

fn workspace_hint(event: &HyprlandEvent) -> Option<String> {
    match event {
        HyprlandEvent::WorkspaceChanged { name, .. } => Some(name.clone()),
        HyprlandEvent::FocusedMonitor { workspace, .. } => Some(workspace.clone()),
        _ => None,
    }
}

fn attention_context(graph: &AttentionGraph, workspace_hint: Option<String>) -> AttentionContext {
    let focused = graph
        .focused_window()
        .and_then(|address| graph.windows().get(address));

    AttentionContext {
        workspace: workspace_hint.or_else(|| focused.map(|window| window.workspace.clone())),
        app_class: focused.map(|window| window.class.clone()),
    }
}

fn observation_from_graph(
    graph: &AttentionGraph,
    at: Duration,
    workspace_hint: Option<String>,
) -> AttentionObservation {
    let context = attention_context(graph, workspace_hint);
    AttentionObservation {
        at,
        workspace: context.workspace,
        app_class: context.app_class,
    }
}

fn combined_learning_summary(
    graph: &AttentionGraph,
    learner: &AttentionLearner,
    contextual: &ContextLearner,
    now: Duration,
) -> String {
    let basic = learning_summary(graph, learner, now);
    let context = attention_context(graph, None);
    let contextual_workspace = contextual
        .predict_workspaces(&context, now, 1)
        .into_iter()
        .next()
        .map_or_else(
            || "none".to_owned(),
            |prediction| {
                format!(
                    "{}:{:.0}%@{}",
                    prediction.target,
                    prediction.probability * 100.0,
                    prediction.event
                )
            },
        );
    format!("{basic} contextual_next={contextual_workspace}")
}

fn learning_summary(graph: &AttentionGraph, learner: &AttentionLearner, now: Duration) -> String {
    let Some(focused) = graph
        .focused_window()
        .and_then(|address| graph.windows().get(address))
    else {
        return "learning=no-focused-window".to_owned();
    };

    let next_workspace = learner
        .predict_next_workspaces(&focused.workspace, now, 1)
        .into_iter()
        .next()
        .map_or_else(
            || "none".to_owned(),
            |prediction| {
                format!(
                    "{}:{:.0}%",
                    prediction.target,
                    prediction.probability * 100.0
                )
            },
        );

    let next_app = learner
        .predict_next_apps(&focused.class, now, 1)
        .into_iter()
        .next()
        .map_or_else(
            || "none".to_owned(),
            |prediction| {
                format!(
                    "{}:{:.0}%",
                    prediction.target,
                    prediction.probability * 100.0
                )
            },
        );

    format!(
        "learning=workspace:{} app:{} next_workspace={} next_app={}",
        focused.workspace, focused.class, next_workspace, next_app
    )
}

fn wall_clock_duration() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}

fn state_path(filename: &str) -> Option<PathBuf> {
    if let Some(state_home) = env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(state_home).join("memsol").join(filename));
    }

    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/memsol").join(filename))
}

fn runtime_event_socket_path() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map(|runtime| runtime.join("memsol/events.sock"))
}

fn load_attention_learner(path: Option<&Path>) -> AttentionLearner {
    load_state(path, AttentionLearner::load_json).unwrap_or_else(|error| {
        eprintln!("ignoring attention learning state: {error}");
        AttentionLearner::default()
    })
}

fn load_context_learner(path: Option<&Path>) -> ContextLearner {
    load_state(path, ContextLearner::load_json).unwrap_or_else(|error| {
        eprintln!("ignoring contextual learning state: {error}");
        ContextLearner::default()
    })
}

fn load_state<T>(path: Option<&Path>, decode: impl FnOnce(File) -> io::Result<T>) -> io::Result<T>
where
    T: Default,
{
    let Some(path) = path else {
        return Ok(T::default());
    };
    match File::open(path) {
        Ok(file) => decode(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error),
    }
}

fn save_attention_learner(path: &Path, learner: &AttentionLearner) -> io::Result<()> {
    save_state(path, |file| learner.save_json(file))
}

fn save_context_learner(path: &Path, learner: &ContextLearner) -> io::Result<()> {
    save_state(path, |file| learner.save_json(file))
}

fn save_state(path: &Path, encode: impl FnOnce(File) -> io::Result<()>) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "state path has no parent",
        ));
    };
    fs::create_dir_all(parent)?;

    let temporary = path.with_extension("tmp");
    encode(File::create(&temporary)?)?;
    fs::rename(temporary, path)
}
