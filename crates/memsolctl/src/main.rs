use memsol_core::{
    classify_pressure,
    telemetry::{read_meminfo, read_memory_psi},
};

fn main() -> std::io::Result<()> {
    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "status".to_owned());

    match command.as_str() {
        "status" => status(),
        other => {
            eprintln!("unknown command: {other}\nusage: memsolctl [status]");
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
    println!(
        "  swap used:       {:.1} MiB",
        memory.swap_used_kib() as f64 / 1024.0
    );

    Ok(())
}
