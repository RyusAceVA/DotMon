//! Search, filter and sort for the process / application list.

use crate::apps::{AppGroup, ProcessKind, classify};
use crate::model::{ProcessId, Snapshot};
use crate::protection::{Protection, ProtectionPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowKey {
    Process(ProcessId),
    /// Application group, identified by its root process.
    App(ProcessId),
}

/// One line of the list, either a process or an application group.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub key: RowKey,
    pub name: String,
    pub pid: u32,
    pub user: String,
    pub kind: ProcessKind,
    /// `None`: no measurement yet (shown as "—", sorted below any value).
    pub cpu: Option<f32>,
    pub memory: u64,
    pub threads: u32,
    pub io_bps: Option<u64>,
    /// Number of processes (1 for a process row).
    pub count: usize,
    pub protection: Protection,
    /// Lower-cased text searched by the query.
    haystack: String,
}

impl Row {
    pub fn is_protected(&self) -> bool {
        !self.protection.is_allowed()
    }
}

pub fn process_rows(snap: &Snapshot, policy: &ProtectionPolicy) -> Vec<Row> {
    snap.processes
        .iter()
        .map(|p| Row {
            key: RowKey::Process(p.id()),
            name: p.display_name().to_string(),
            pid: p.pid,
            user: p.user.clone(),
            kind: classify(p, policy),
            cpu: p.cpu,
            memory: p.memory,
            threads: p.threads,
            io_bps: p.io_bps(),
            count: 1,
            protection: policy.assess(p),
            haystack: format!("{} {} {}", p.display_name(), p.cmdline, p.user).to_lowercase(),
        })
        .collect()
}

pub fn app_rows(snap: &Snapshot, groups: &[AppGroup], policy: &ProtectionPolicy) -> Vec<Row> {
    groups
        .iter()
        .map(|g| {
            let mut haystack = format!("{} {}", g.name, g.user);
            for m in &g.members {
                if let Some(p) = snap.process(*m) {
                    haystack.push(' ');
                    haystack.push_str(p.display_name());
                }
            }
            let protection = snap.process(g.root).map(|p| policy.assess(p)).unwrap_or(Protection::Allowed);
            let protection = if g.kind == ProcessKind::Kernel {
                Protection::Protected(crate::protection::ProtectionReason::KernelThread)
            } else {
                protection
            };
            Row {
                key: RowKey::App(g.root),
                name: g.name.clone(),
                pid: g.root.pid,
                user: g.user.clone(),
                kind: g.kind,
                cpu: g.cpu,
                memory: g.memory,
                threads: g.threads,
                io_bps: g.io_bps,
                count: g.count(),
                protection,
                haystack: haystack.to_lowercase(),
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    Name,
    Pid,
    #[default]
    Cpu,
    Memory,
    Threads,
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortDir {
    Asc,
    #[default]
    Desc,
}

impl SortDir {
    pub fn flip(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Apps,
    Services,
    /// Session components, other users' processes and kernel threads.
    System,
}

impl Filter {
    fn accepts(self, kind: ProcessKind) -> bool {
        match self {
            Filter::All => true,
            Filter::Apps => kind == ProcessKind::App,
            Filter::Services => kind == ProcessKind::Service,
            Filter::System => {
                matches!(kind, ProcessKind::Session | ProcessKind::System | ProcessKind::Kernel)
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessQuery {
    pub search: String,
    pub filter: Filter,
    pub sort: SortKey,
    pub dir: SortDir,
    pub show_kernel: bool,
}

impl ProcessQuery {
    /// Clicking a column header: same key flips the direction, a new key
    /// starts with the most useful direction (A→Z for text, high→low for numbers).
    pub fn toggle_sort(&mut self, key: SortKey) {
        if self.sort == key {
            self.dir = self.dir.flip();
        } else {
            self.sort = key;
            self.dir = match key {
                SortKey::Name | SortKey::Pid => SortDir::Asc,
                _ => SortDir::Desc,
            };
        }
    }

    fn matches(&self, row: &Row, tokens: &[String]) -> bool {
        if !self.show_kernel && row.kind == ProcessKind::Kernel {
            return false;
        }
        if !self.filter.accepts(row.kind) {
            return false;
        }
        tokens.iter().all(|t| {
            if let Some(pid) = t.strip_prefix("pid:") {
                return row.pid.to_string() == pid;
            }
            row.haystack.contains(t.as_str()) || row.pid.to_string().starts_with(t.as_str())
        })
    }

    /// Filters and sorts `rows` in place. Sorting is stable and uses the PID
    /// as a tie-breaker so rows do not jitter between refreshes.
    pub fn apply(&self, rows: &mut Vec<Row>) {
        let tokens: Vec<String> = self.search.split_whitespace().map(str::to_lowercase).collect();
        rows.retain(|r| self.matches(r, &tokens));

        rows.sort_by(|a, b| {
            use std::cmp::Ordering;
            let primary = match self.sort {
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Pid => a.pid.cmp(&b.pid),
                SortKey::Cpu => a.cpu.partial_cmp(&b.cpu).unwrap_or(Ordering::Equal),
                SortKey::Memory => a.memory.cmp(&b.memory),
                SortKey::Threads => a.threads.cmp(&b.threads),
                SortKey::Io => a.io_bps.cmp(&b.io_bps),
            };
            let primary = if self.dir == SortDir::Desc { primary.reverse() } else { primary };
            primary.then(a.pid.cmp(&b.pid))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::group_apps;
    use crate::model::fixtures::proc;

    const ME: u32 = 1000;

    fn snap() -> Snapshot {
        let mut a = proc(10, Some(1), "firefox", ME);
        a.cpu = Some(12.0);
        a.memory = 900;
        a.cmdline = "/usr/lib/firefox/firefox --new-window".into();
        let mut b = proc(11, Some(10), "Web Content", ME);
        b.cpu = Some(3.0);
        b.memory = 400;
        let mut c = proc(20, Some(1), "kitty", ME);
        c.cpu = Some(0.5);
        c.memory = 100;
        let mut d = proc(30, Some(2), "kworker/0:1", 0);
        d.kernel_thread = true;
        let mut e = proc(40, Some(1), "syncthing", ME);
        e.cgroup = Some("/user.slice/x/app.slice/syncthing.service".into());
        e.memory = 2000;
        let f = proc(50, Some(1), "cupsd", 0);
        Snapshot { processes: vec![proc(1, None, "systemd", 0), a, b, c, d, e, f], ..Default::default() }
    }

    fn names(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn default_sort_is_cpu_desc_without_kernel_threads() {
        let pol = ProtectionPolicy::new(1, ME);
        let mut rows = process_rows(&snap(), &pol);
        ProcessQuery::default().apply(&mut rows);
        assert_eq!(names(&rows)[..3], ["firefox", "Web Content", "kitty"]);
        assert!(!names(&rows).contains(&"kworker/0:1"));
    }

    #[test]
    fn search_matches_name_cmdline_and_pid() {
        let pol = ProtectionPolicy::new(1, ME);
        let q = |s: &str| {
            let mut rows = process_rows(&snap(), &pol);
            ProcessQuery { search: s.into(), ..Default::default() }.apply(&mut rows);
            rows
        };
        assert_eq!(names(&q("FIRE")), ["firefox"]);
        assert_eq!(names(&q("new-window")), ["firefox"]);
        assert_eq!(names(&q("pid:20")), ["kitty"]);
        assert_eq!(names(&q("web content")), ["Web Content"]);
        assert!(q("zzz-nothing").is_empty());
    }

    #[test]
    fn filters_by_kind() {
        let pol = ProtectionPolicy::new(1, ME);
        let run = |f: Filter| {
            let mut rows = process_rows(&snap(), &pol);
            ProcessQuery { filter: f, sort: SortKey::Name, dir: SortDir::Asc, ..Default::default() }
                .apply(&mut rows);
            rows
        };
        assert_eq!(names(&run(Filter::Services)), ["syncthing"]);
        assert_eq!(names(&run(Filter::Apps)), ["firefox", "kitty", "Web Content"]);
        assert_eq!(names(&run(Filter::System)), ["cupsd", "systemd"]);
    }

    #[test]
    fn toggle_sort_flips_and_resets() {
        let mut q = ProcessQuery::default();
        q.toggle_sort(SortKey::Cpu);
        assert_eq!(q.dir, SortDir::Asc);
        q.toggle_sort(SortKey::Name);
        assert_eq!((q.sort, q.dir), (SortKey::Name, SortDir::Asc));
        q.toggle_sort(SortKey::Memory);
        assert_eq!((q.sort, q.dir), (SortKey::Memory, SortDir::Desc));
    }

    #[test]
    fn sort_by_memory_and_tie_break_by_pid() {
        let pol = ProtectionPolicy::new(1, ME);
        let mut rows = process_rows(&snap(), &pol);
        ProcessQuery { sort: SortKey::Memory, ..Default::default() }.apply(&mut rows);
        assert_eq!(names(&rows)[0], "syncthing");
        // systemd and cupsd both have 0 bytes: lower PID first.
        let tail: Vec<u32> = rows.iter().rev().take(2).map(|r| r.pid).collect();
        assert_eq!(tail, vec![50, 1]);
    }

    #[test]
    fn app_rows_aggregate_and_search_members() {
        let s = snap();
        let pol = ProtectionPolicy::new(1, ME);
        let groups = group_apps(&s, &pol);
        let mut rows = app_rows(&s, &groups, &pol);
        ProcessQuery { search: "web content".into(), ..Default::default() }.apply(&mut rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "firefox");
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[0].cpu, Some(15.0));
        assert!(matches!(rows[0].key, RowKey::App(_)));
    }

    #[test]
    fn protection_is_attached_to_rows() {
        let pol = ProtectionPolicy::new(1, ME);
        let rows = process_rows(&snap(), &pol);
        let get = |n: &str| rows.iter().find(|r| r.name == n).unwrap();
        assert!(get("systemd").is_protected());
        assert!(get("cupsd").is_protected());
        assert!(!get("firefox").is_protected());
    }

    #[test]
    fn unmeasured_cpu_sorts_below_every_measured_value() {
        let pol = ProtectionPolicy::new(1, ME);
        let mut s = snap();
        // "kitty" (0.5 %) is the lowest measured value; "cupsd" has none yet.
        s.processes.iter_mut().find(|p| p.name == "cupsd").unwrap().cpu = None;
        let mut rows = process_rows(&s, &pol);
        ProcessQuery { sort: SortKey::Cpu, dir: SortDir::Desc, ..Default::default() }.apply(&mut rows);
        assert_eq!(rows.last().unwrap().name, "cupsd");
        assert_eq!(rows.last().unwrap().cpu, None);
        assert_eq!(rows[0].name, "firefox");
        ProcessQuery { sort: SortKey::Cpu, dir: SortDir::Asc, ..Default::default() }.apply(&mut rows);
        assert_eq!(rows[0].name, "cupsd", "ascending: unknown first, consistently with the I/O column");
    }
}
