use std::{
    env,
    fs::File,
    io,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use memsol_core::{
    AttentionLearner, AttentionState, classify_pressure, snapshot_attention,
    telemetry::{read_meminfo, read_memory_psi},
};

fn main() -> io::Result<()> {
    let command = env::args().nth(1).unwrap_or_else(|| "status".to_owned());

    match command.as_str() {
        "status" => status(),
        "attention" | "hypr" => attention(),
        "learn" | "learning" => learning(),
        other => {
            eprintln!("unknown command: {other}\nusage: memsolctl [status|attention|learn]");
            std::process::exit(2);
        }
    }
}

fn status() -> io::Result<()> {
    let memory = read_meminfo()?;
    let psi = read_memory_psi()?;
    let level = classify_pressure(memory, psi);

    println!("memsol observer status");
    println!("  pressure:        {level:?}");
    println!(
        "  available:       {:.1}%",
        memory.available_ratio() * 100.0
    );
    println!("  psi some avg10:  {:.2}%", psi.some.avg10);
    println!("  psi full avg10:  {:.2}%", psi.full.avg10);
    println!("  swap used:       {} MiB", memory.swap_used_kib() / 1024);

    Ok(())
}

fn attention() -> io::Result<()> {
    let graph = snapshot_attention()?;
    let mut workspaces: Vec<_> = graph.workspaces().values().collect();
    workspaces.sort_by_key(|workspace| workspace.id);

    println!("memsol Hyprland attention snapshot");
    println!(
        "  windows: {} focused / {} visible / {} hidden",
        graph.focused_count(),
        graph.visible_count(),
        graph.hidden_count()
    );

    for workspace in workspaces {
        let state = if workspace.active { "ACTIVE" } else { "hidden" };
        println!(
            "  workspace {:>4} {:<24} {:<6} windows={} monitor={}",
            workspace.id,
            workspace.name,
            state,
            workspace.window_count,
            workspace.monitor.as_deref().unwrap_or("-")
        );

        let mut windows: Vec<_> = graph
            .windows()
            .values()
            .filter(|window| window.workspace == workspace.name)
            .collect();
        windows.sort_by(|left, right| left.address.cmp(&right.address));

        for window in windows {
            let attention = match window.attention {
                AttentionState::Focused => "FOCUSED",
                AttentionState::Visible => "VISIBLE",
                AttentionState::Hidden => "hidden",
            };
            println!(
                "    {:<8} {:<9} {:<20} {}",
                window.address, attention, window.class, window.title
            );
        }
    }

    Ok(())
}

fn learning() -> io::Result<()> {
    let path = learning_state_path().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "HOME and XDG_STATE_HOME are unavailable",
        )
    })?;
    let learner = AttentionLearner::load_json(File::open(&path)?)?;
    let graph = snapshot_attention()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);

    println!("memsol learned attention model");
    println!("  state: {}", path.display());

    let Some(window) = graph
        .focused_window()
        .and_then(|address| graph.windows().get(address))
    else {
        println!("  no focused Hyprland window");
        return Ok(());
    };

    println!("  current workspace: {}", window.workspace);
    println!("  current app:       {}", window.class);
    println!(
        "  workspace relevance: {:.2}",
        learner.workspace_relevance(&window.workspace, now)
    );
    println!(
        "  app relevance:       {:.2}",
        learner.app_relevance(&window.class, now)
    );

    println!("  likely next workspaces:");
    let workspace_predictions = learner.predict_next_workspaces(&window.workspace, now, 5);
    if workspace_predictions.is_empty() {
        println!("    insufficient observations");
    } else {
        for prediction in workspace_predictions {
            println!(
                "    {:<24} {:>5.1}%  decayed_samples={:.2}",
                prediction.target,
                prediction.probability * 100.0,
                prediction.decayed_samples
            );
        }
    }

    println!("  likely next apps:");
    let app_predictions = learner.predict_next_apps(&window.class, now, 5);
    if app_predictions.is_empty() {
        println!("    insufficient observations");
    } else {
        for prediction in app_predictions {
            println!(
                "    {:<24} {:>5.1}%  decayed_samples={:.2}",
                prediction.target,
                prediction.probability * 100.0,
                prediction.decayed_samples
            );
        }
    }

    Ok(())
}

fn learning_state_path() -> Option<PathBuf> {
    if let Some(state_home) = env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(state_home).join("memsol/learning.json"));
    }

    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/memsol/learning.json"))
}
