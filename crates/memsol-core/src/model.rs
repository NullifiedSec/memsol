#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PsiLine {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
    pub total_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PsiMemory {
    pub some: PsiLine,
    pub full: PsiLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureLevel {
    Normal,
    Elevated,
    Pressured,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub total_kib: u64,
    pub available_kib: u64,
    pub swap_total_kib: u64,
    pub swap_free_kib: u64,
}

impl MemorySnapshot {
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn available_ratio(self) -> f64 {
        if self.total_kib == 0 {
            return 0.0;
        }
        self.available_kib as f64 / self.total_kib as f64
    }

    #[must_use]
    pub const fn swap_used_kib(self) -> u64 {
        self.swap_total_kib.saturating_sub(self.swap_free_kib)
    }
}
