use std::{
    env,
    fs::{self, File},
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use memsol_core::{
    AttentionGraph, AttentionLearner, AttentionObservation, HyprlandEvent, HyprlandEventStream,
    apply_event, classify_pressure, snapshot_attention,
    telemetry::{read_meminfo, read_memory_psi},
};

const SAVE_INTERVAL: Duration = Duration::from_secs(30);

fn main() -> io::Result<()> {
    println!("memsold observer starting; no reclaim or freeze actions are enabled");

    let learner_path = learning_state_path();
    let learner = Arc::new(Mutex::new(load_learner(learner_path.as_ref())));
    let attention = start_attention_observer(Arc::clone(&learner));
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
            .and_then(|graph| learner.lock().ok().map(|model| learning_summary(&graph, &model, now)))
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
            if let (Some(path), Ok(model)) = (learner_path.as_ref(), learner.lock()) {
                if let Err(error) = save_learner(path, &model) {
                    eprintln!("failed to persist learning state: {error}");
                }
            }
            last_save = Instant::now();
        }

        thread::sleep(Duration::from_secs(5));
    }
}

fn start_attention_observer(
    learner: Arc<Mutex<AttentionLearner>>,
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

    if let Ok(mut model) = learner.lock() {
        model.observe(observation_from_graph(&graph, wall_clock_duration(), None));
    }

    let graph = Arc::new(Mutex::new(graph));
    let event_graph = Arc::clone(&graph);

    thread::spawn(move || loop {
        match stream.next_event() {
            Ok(Some(event)) => {
                let now = wall_clock_duration();
                if let Ok(mut graph) = event_graph.lock() {
                    apply_event(&mut graph, event.clone());
                    if should_learn_from(&event) {
                        let workspace_hint = workspace_hint(&event);
                        let observation = observation_from_graph(&graph, now, workspace_hint);
                        if let Ok(mut model) = learner.lock() {
                            model.observe(observation);
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
    });

    graph
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

fn observation_from_graph(
    graph: &AttentionGraph,
    at: Duration,
    workspace_hint: Option<String>,
) -> AttentionObservation {
    let focused = graph
        .focused_window()
        .and_then(|address| graph.windows().get(address));

    AttentionObservation {
        at,
        workspace: workspace_hint.or_else(|| focused.map(|window| window.workspace.clone())),
        app_class: focused.map(|window| window.class.clone()),
    }
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
            |prediction| format!("{}:{:.0}%", prediction.target, prediction.probability * 100.0),
        );

    let next_app = learner
        .predict_next_apps(&focused.class, now, 1)
        .into_iter()
        .next()
        .map_or_else(
            || "none".to_owned(),
            |prediction| format!("{}:{:.0}%", prediction.target, prediction.probability * 100.0),
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

fn learning_state_path() -> Option<PathBuf> {
    if let Some(state_home) = env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(state_home).join("memsol/learning.json"));
    }

    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/memsol/learning.json"))
}

fn load_learner(path: Option<&PathBuf>) -> AttentionLearner {
    let Some(path) = path else {
        eprintln!("learning state persistence disabled: HOME/XDG_STATE_HOME unavailable");
        return AttentionLearner::default();
    };

    match File::open(path) {
        Ok(file) => match AttentionLearner::load_json(file) {
            Ok(model) => {
                println!("loaded learning state from {}", path.display());
                model
            }
            Err(error) => {
                eprintln!("ignoring invalid learning state {}: {error}", path.display());
                AttentionLearner::default()
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => AttentionLearner::default(),
        Err(error) => {
            eprintln!("failed to read learning state {}: {error}", path.display());
            AttentionLearner::default()
        }
    }
}

fn save_learner(path: &PathBuf, learner: &AttentionLearner) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "learning state path has no parent",
        ));
    };
    fs::create_dir_all(parent)?;

    let temporary = path.with_extension("json.tmp");
    {
        let file = File::create(&temporary)?;
        learner.save_json(file)?;
    }
    fs::rename(temporary, path)
}
