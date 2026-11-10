//! Fixed-capacity time series used by the real-time charts.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::model::{GpuState, ProcessId, Snapshot};

/// A bounded series of samples; the oldest sample is dropped when full.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    capacity: usize,
    data: VecDeque<f32>,
}

impl Series {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2);
        Self { capacity, data: VecDeque::with_capacity(capacity) }
    }

    pub fn push(&mut self, value: f32) {
        let value = if value.is_finite() { value } else { 0.0 };
        if self.data.len() == self.capacity {
            self.data.pop_front();
        }
        self.data.push_back(value);
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn last(&self) -> Option<f32> {
        self.data.back().copied()
    }

    pub fn max(&self) -> f32 {
        self.data.iter().copied().fold(0.0, f32::max)
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.data.iter().copied()
    }

    /// Change capacity, keeping the most recent samples.
    pub fn resize(&mut self, capacity: usize) {
        let capacity = capacity.max(2);
        while self.data.len() > capacity {
            self.data.pop_front();
        }
        self.capacity = capacity;
    }
}

/// Machine-wide histories.
#[derive(Debug, Clone)]
pub struct SystemHistory {
    pub cpu: Series,
    pub memory: Series,
    pub gpu: Series,
    pub net_rx: Series,
    pub net_tx: Series,
    pub disk_read: Series,
    pub disk_write: Series,
}

impl SystemHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            cpu: Series::new(capacity),
            memory: Series::new(capacity),
            gpu: Series::new(capacity),
            net_rx: Series::new(capacity),
            net_tx: Series::new(capacity),
            disk_read: Series::new(capacity),
            disk_write: Series::new(capacity),
        }
    }

    pub fn record(&mut self, snap: &Snapshot) {
        self.cpu.push(snap.cpu.usage);
        self.memory.push(snap.memory.used_percent());
        // Only real measurements: a powered-down GPU is genuinely at 0 %, but
        // a resting (unqueried) or unsupported one adds no point at all.
        if let Some(g) = snap.gpus.first() {
            match (g.state, g.usage) {
                (GpuState::Active, Some(u)) => self.gpu.push(u),
                (GpuState::Asleep, _) => self.gpu.push(0.0),
                _ => {}
            }
        }
        self.net_rx.push(snap.net_rx_bps() as f32);
        self.net_tx.push(snap.net_tx_bps() as f32);
        self.disk_read.push(snap.disk_read_bps() as f32);
        self.disk_write.push(snap.disk_write_bps() as f32);
    }

    pub fn resize(&mut self, capacity: usize) {
        for s in [
            &mut self.cpu,
            &mut self.memory,
            &mut self.gpu,
            &mut self.net_rx,
            &mut self.net_tx,
            &mut self.disk_read,
            &mut self.disk_write,
        ] {
            s.resize(capacity);
        }
    }
}

/// CPU and memory history for one process.
#[derive(Debug, Clone)]
pub struct ProcessSeries {
    pub cpu: Series,
    /// Resident memory, bytes (stored as f32 — precision is plenty for charts).
    pub memory: Series,
}

/// Per-process histories, pruned automatically when processes disappear.
#[derive(Debug, Clone)]
pub struct ProcessHistory {
    capacity: usize,
    series: HashMap<ProcessId, ProcessSeries>,
}

impl ProcessHistory {
    pub fn new(capacity: usize) -> Self {
        Self { capacity, series: HashMap::new() }
    }

    pub fn record(&mut self, snap: &Snapshot) {
        let mut alive = HashSet::with_capacity(snap.processes.len());
        for p in &snap.processes {
            let id = p.id();
            alive.insert(id);
            let cap = self.capacity;
            let entry = self
                .series
                .entry(id)
                .or_insert_with(|| ProcessSeries { cpu: Series::new(cap), memory: Series::new(cap) });
            // No measurement yet (new process): leave a gap, not a fake 0.
            if let Some(cpu) = p.cpu {
                entry.cpu.push(cpu);
            }
            entry.memory.push(p.memory as f32);
        }
        self.series.retain(|id, _| alive.contains(id));
    }

    pub fn get(&self, id: ProcessId) -> Option<&ProcessSeries> {
        self.series.get(&id)
    }

    pub fn len(&self) -> usize {
        self.series.len()
    }

    pub fn is_empty(&self) -> bool {
        self.series.is_empty()
    }

    pub fn resize(&mut self, capacity: usize) {
        self.capacity = capacity;
        for s in self.series.values_mut() {
            s.cpu.resize(capacity);
            s.memory.resize(capacity);
        }
    }

    /// Sum of several processes' series, aligned on the most recent sample.
    /// Used for app groups.
    pub fn sum(&self, ids: &[ProcessId]) -> (Vec<f32>, Vec<f32>) {
        let mut cpu: Vec<f32> = Vec::new();
        let mut mem: Vec<f32> = Vec::new();
        for id in ids {
            if let Some(s) = self.series.get(id) {
                add_aligned(&mut cpu, s.cpu.values());
                add_aligned(&mut mem, s.memory.values());
            }
        }
        (cpu, mem)
    }
}

/// Adds `src` into `acc`, aligning both on their last element.
fn add_aligned(acc: &mut Vec<f32>, src: impl ExactSizeIterator<Item = f32>) {
    let n = src.len();
    if n > acc.len() {
        let pad = n - acc.len();
        acc.splice(0..0, std::iter::repeat_n(0.0, pad));
    }
    let offset = acc.len() - n;
    for (i, v) in src.enumerate() {
        acc[offset + i] += v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::proc;

    #[test]
    fn series_drops_oldest_when_full() {
        let mut s = Series::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            s.push(v);
        }
        assert_eq!(s.values().collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
        assert_eq!(s.last(), Some(4.0));
        assert_eq!(s.max(), 4.0);
    }

    #[test]
    fn series_sanitises_non_finite() {
        let mut s = Series::new(4);
        s.push(f32::NAN);
        s.push(f32::INFINITY);
        assert_eq!(s.values().collect::<Vec<_>>(), vec![0.0, 0.0]);
    }

    #[test]
    fn series_resize_keeps_recent() {
        let mut s = Series::new(5);
        for v in 0..5 {
            s.push(v as f32);
        }
        s.resize(2);
        assert_eq!(s.values().collect::<Vec<_>>(), vec![3.0, 4.0]);
        assert_eq!(s.capacity(), 2);
    }

    #[test]
    fn gpu_history_records_only_measurements() {
        use crate::model::GpuStats;
        let mut h = SystemHistory::new(10);
        let snap = |state, usage| Snapshot {
            gpus: vec![GpuStats { state, usage, ..Default::default() }],
            ..Default::default()
        };
        h.record(&snap(GpuState::Active, Some(42.0)));
        h.record(&snap(GpuState::Resting, None));
        h.record(&snap(GpuState::Active, None));
        h.record(&snap(GpuState::Asleep, None));
        assert_eq!(h.gpu.values().collect::<Vec<_>>(), vec![42.0, 0.0]);
    }

    #[test]
    fn process_history_prunes_dead_processes() {
        let mut h = ProcessHistory::new(10);
        let a = proc(1, None, "a", 0);
        let b = proc(2, None, "b", 0);
        let mut snap = Snapshot { processes: vec![a.clone(), b.clone()], ..Default::default() };
        h.record(&snap);
        assert_eq!(h.len(), 2);
        snap.processes = vec![a.clone()];
        h.record(&snap);
        assert_eq!(h.len(), 1);
        assert!(h.get(b.id()).is_none());
        assert_eq!(h.get(a.id()).unwrap().cpu.len(), 2);
    }

    #[test]
    fn sum_aligns_on_latest_sample() {
        let mut h = ProcessHistory::new(10);
        let mut a = proc(1, None, "a", 0);
        a.cpu = Some(1.0);
        let mut b = proc(2, None, "b", 0);
        b.cpu = Some(10.0);
        h.record(&Snapshot { processes: vec![a.clone()], ..Default::default() });
        h.record(&Snapshot { processes: vec![a.clone(), b.clone()], ..Default::default() });
        let (cpu, _) = h.sum(&[a.id(), b.id()]);
        assert_eq!(cpu, vec![1.0, 11.0]);
    }

    #[test]
    fn unmeasured_cpu_leaves_a_gap_not_a_zero() {
        let mut h = ProcessHistory::new(10);
        let mut a = proc(1, None, "a", 0);
        a.cpu = None;
        h.record(&Snapshot { processes: vec![a.clone()], ..Default::default() });
        let s = h.get(a.id()).unwrap();
        assert_eq!(s.cpu.len(), 0, "a missing measurement is not stored");
        assert_eq!(s.memory.len(), 1, "other series are unaffected");
        a.cpu = Some(4.0);
        h.record(&Snapshot { processes: vec![a.clone()], ..Default::default() });
        assert_eq!(h.get(a.id()).unwrap().cpu.values().collect::<Vec<_>>(), vec![4.0]);
    }
}
