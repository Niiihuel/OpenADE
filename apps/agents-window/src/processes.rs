//! The processes openADE started, directly or through them (agents' CLIs,
//! terminal shells, MCP servers, language servers, the browser): their CPU,
//! memory and disk traffic, read from `/proc` on Linux, and a way to end one.

use std::collections::HashMap;
use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ProcessRow {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
    pub command: String,
    /// Of one core, since the previous sample.
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub read_bytes: u64,
    pub written_bytes: u64,
}

/// CPU time is a difference: the sampler keeps the previous one.
#[derive(Default)]
pub(crate) struct ProcessSampler {
    previous: HashMap<u32, u64>,
    at: Option<Instant>,
}

/// `/proc/<pid>/stat`: the name and the fields after it, which a name with
/// spaces or parentheses cannot shift.
fn parse_stat(stat: &str) -> Option<(String, u32, u64, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_owned();
    let fields: Vec<&str> = stat.get(close + 2..)?.split(' ').collect();
    // After the name: state, ppid, ... utime (11), stime (12), ... rss (21).
    let parent = fields.get(1)?.parse().ok()?;
    let ticks = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
    let rss_pages = fields.get(21)?.parse().ok()?;
    Some((name, parent, ticks, rss_pages))
}

/// `/proc/<pid>/io`'s bytes read from and written to storage.
fn parse_io(io: &str) -> (u64, u64) {
    let field = |name: &str| {
        io.lines()
            .find_map(|line| line.strip_prefix(name)?.trim().parse().ok())
            .unwrap_or(0)
    };
    (field("read_bytes:"), field("write_bytes:"))
}

/// The pids under `root`, given each process's parent.
fn descendants(root: u32, parents: &HashMap<u32, u32>) -> Vec<u32> {
    let mut found = vec![root];
    let mut index = 0;
    while index < found.len() {
        let parent = found[index];
        let mut children: Vec<u32> = parents.iter().filter(|(_, p)| **p == parent).map(|(pid, _)| *pid).collect();
        children.sort_unstable();
        found.extend(children);
        index += 1;
    }
    found.remove(0);
    found
}

impl ProcessSampler {
    /// openADE's descendants now. Empty where `/proc` does not exist.
    pub(crate) fn sample(&mut self) -> Vec<ProcessRow> {
        #[cfg(target_os = "linux")]
        {
            let Ok(entries) = std::fs::read_dir("/proc") else {
                return Vec::new();
            };
            let mut stats = HashMap::new();
            for entry in entries.flatten() {
                let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
                    continue;
                };
                if let Some(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok().as_deref().and_then(parse_stat) {
                    stats.insert(pid, stat);
                }
            }
            let parents = stats.iter().map(|(pid, (_, parent, _, _))| (*pid, *parent)).collect();
            // SAFETY: sysconf only reads system constants.
            let (page, hertz) = unsafe { (libc::sysconf(libc::_SC_PAGESIZE).max(1) as u64, libc::sysconf(libc::_SC_CLK_TCK).max(1) as f32) };
            let elapsed = self.at.map(|at| at.elapsed().as_secs_f32()).filter(|seconds| *seconds > 0.0);
            let mut rows = Vec::new();
            let mut previous = HashMap::new();
            for pid in descendants(std::process::id(), &parents) {
                let Some((name, parent, ticks, rss)) = stats.get(&pid).cloned() else {
                    continue;
                };
                previous.insert(pid, ticks);
                let cpu_percent = match (elapsed, self.previous.get(&pid)) {
                    (Some(seconds), Some(before)) => ticks.saturating_sub(*before) as f32 / hertz / seconds * 100.0,
                    _ => 0.0,
                };
                let (read_bytes, written_bytes) = std::fs::read_to_string(format!("/proc/{pid}/io")).map(|io| parse_io(&io)).unwrap_or_default();
                let command = std::fs::read(format!("/proc/{pid}/cmdline"))
                    .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " ").trim().chars().take(300).collect())
                    .unwrap_or_default();
                rows.push(ProcessRow { pid, parent, name, command, cpu_percent, memory_bytes: rss * page, read_bytes, written_bytes });
            }
            self.previous = previous;
            self.at = Some(Instant::now());
            rows
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (&self.previous, &self.at);
            Vec::new()
        }
    }
}

/// Ask `pid` to end (SIGTERM), when it is still one of openADE's.
pub(crate) fn terminate(pid: u32) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let ours = ProcessSampler::default().sample().iter().any(|row| row.pid == pid);
        if !ours {
            return Err("Ese proceso ya no es de openADE".into());
        }
        // SAFETY: a signal to a pid just checked to descend from this process.
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Err("Disponible en Linux".into())
    }
}

/// `1.5 MB` and the like.
pub(crate) fn bytes(value: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut amount = value as f64;
    let mut unit = 0;
    while amount >= 1024.0 && unit < UNITS.len() - 1 {
        amount /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{value} B") } else { format!("{amount:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_and_io_lines_are_read_whatever_the_name() {
        let stat = "4242 (my (odd) name) S 100 4242 4242 0 -1 4194560 500 0 0 0 30 12 0 0 20 0 1 0 123 1000000 250 18446744073709551615";
        assert_eq!(parse_stat(stat), Some(("my (odd) name".into(), 100, 42, 250)));
        assert_eq!(parse_io("rchar: 9\nread_bytes: 4096\nwrite_bytes: 8192\n"), (4096, 8192));
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536 * 1024), "1.5 MB");
    }

    #[test]
    fn descendants_are_found_through_every_generation_and_nothing_else() {
        let parents: HashMap<u32, u32> = [(10, 1), (11, 10), (12, 11), (13, 10), (20, 1)].into();
        assert_eq!(descendants(10, &parents), [11, 13, 12]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_of_this_process_is_listed_and_can_be_ended() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let mut sampler = ProcessSampler::default();
        let rows = sampler.sample();
        let row = rows.iter().find(|row| row.pid == child.id()).expect("the child is listed");
        assert_eq!(row.name, "sleep");
        assert!(row.command.starts_with("sleep 30"), "{}", row.command);
        assert!(terminate(1).is_err(), "init is not ours");
        terminate(child.id()).unwrap();
        assert!(!child.wait().unwrap().success());
    }
}
