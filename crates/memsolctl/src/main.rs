use memsol_core::{
    AttentionState, classify_pressure, snapshot_attention,
    telemetry::{read_meminfo, read_memory_psi},
};

fn main() -> std::io::Result<()> {
    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "status".to_owned());

    match command.as_str() {
        "status" => status(),
        "attention" | "hypr" => attention(),
        other => {
            eprintln!("unknown command: {other}\nusage: memsolctl [status|attention]");
            std::process::exit(2);
        }
    }
}

fn status() -> std::io::Result<()> {
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

fn attention() -> std::io::Result<()> {
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
