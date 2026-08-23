use std::{thread, time::Duration};

use memsol_core::{classify_pressure, telemetry::{read_meminfo, read_memory_psi}};

fn main() -> std::io::Result<()> {
    println!("memsold observer starting; no reclaim or freeze actions are enabled");

    loop {
        let memory = read_meminfo()?;
        let psi = read_memory_psi()?;
        let level = classify_pressure(memory, psi);

        println!(
            "pressure={level:?} available={:.1}% psi.some.avg10={:.2} psi.full.avg10={:.2} swap_used_mib={:.1}",
            memory.available_ratio() * 100.0,
            psi.some.avg10,
            psi.full.avg10,
            memory.swap_used_kib() as f64 / 1024.0,
        );

        thread::sleep(Duration::from_secs(5));
    }
}
