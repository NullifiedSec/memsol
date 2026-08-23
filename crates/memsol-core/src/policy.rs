use crate::model::{MemorySnapshot, PressureLevel, PsiMemory};

/// Conservative observer-only classification.
///
/// These thresholds are intentionally provisional. They exist so early telemetry
/// can be categorized and inspected; controller code must not treat them as a
/// license to reclaim or freeze workloads.
#[must_use]
pub fn classify_pressure(memory: MemorySnapshot, psi: PsiMemory) -> PressureLevel {
    let available = memory.available_ratio();

    if psi.full.avg10 >= 5.0 || (psi.some.avg10 >= 20.0 && available < 0.05) {
        PressureLevel::Critical
    } else if psi.full.avg10 >= 1.0 || psi.some.avg10 >= 10.0 || available < 0.08 {
        PressureLevel::Pressured
    } else if psi.some.avg10 >= 2.0 || available < 0.15 {
        PressureLevel::Elevated
    } else {
        PressureLevel::Normal
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{MemorySnapshot, PressureLevel, PsiLine, PsiMemory};

    use super::classify_pressure;

    fn psi(some: f64, full: f64) -> PsiMemory {
        PsiMemory {
            some: PsiLine { avg10: some, avg60: some, avg300: some, total_us: 0 },
            full: PsiLine { avg10: full, avg60: full, avg300: full, total_us: 0 },
        }
    }

    fn memory(available_ratio: f64) -> MemorySnapshot {
        let total_kib = 1_000_000;
        MemorySnapshot {
            total_kib,
            available_kib: (total_kib as f64 * available_ratio) as u64,
            swap_total_kib: 0,
            swap_free_kib: 0,
        }
    }

    #[test]
    fn normal_when_capacity_and_psi_are_healthy() {
        assert_eq!(classify_pressure(memory(0.5), psi(0.0, 0.0)), PressureLevel::Normal);
    }

    #[test]
    fn critical_when_full_stalls_are_sustained() {
        assert_eq!(classify_pressure(memory(0.2), psi(8.0, 5.0)), PressureLevel::Critical);
    }
}
