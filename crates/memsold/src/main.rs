use std::{
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use memsol_core::{
    AttentionGraph, HyprlandEventStream, apply_event, classify_pressure, snapshot_attention,
    telemetry::{read_meminfo, read_memory_psi},
};

fn main() -> std::io::Result<()> {
    println!("memsold observer starting; no reclaim or freeze actions are enabled");
    let attention = start_attention_observer();

    loop {
        let memory = read_meminfo()?;
        let psi = read_memory_psi()?;
        let level = classify_pressure(memory, psi);
        let attention_summary = attention.lock().ok().map(|graph| {
            format!(
                "hypr_windows={} focused={} visible={} hidden={}",
                graph.windows().len(),
                graph.focused_count(),
                graph.visible_count(),
                graph.hidden_count()
            )
        });

        println!(
            "pressure={level:?} available={:.1}% psi.some.avg10={:.2} psi.full.avg10={:.2} swap_used_mib={} {}",
            memory.available_ratio() * 100.0,
            psi.some.avg10,
            psi.full.avg10,
            memory.swap_used_kib() / 1024,
            attention_summary
                .as_deref()
                .unwrap_or("hyprland=unavailable"),
        );

        thread::sleep(Duration::from_secs(5));
    }
}

fn start_attention_observer() -> Arc<Mutex<AttentionGraph>> {
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

    let graph = Arc::new(Mutex::new(graph));
    let event_graph = Arc::clone(&graph);

    thread::spawn(move || {
        loop {
            match stream.next_event() {
                Ok(Some(event)) => {
                    if let Ok(mut graph) = event_graph.lock() {
                        apply_event(&mut graph, event);
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
