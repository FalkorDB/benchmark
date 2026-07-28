//! Best-effort collection of the **client** host metadata (via `sysinfo`) recorded in a report's
//! [`Meta::host`](crate::synthetic::report::Meta::host) — the machine driving the benchmark, as
//! opposed to the FalkorDB server described by
//! [`ServerInfo`](crate::synthetic::report::ServerInfo).

use crate::synthetic::report::HostInfo;
use sysinfo::System;

/// Collect the client host metadata. Best-effort: any field `sysinfo` can't determine on this
/// platform is left `None` (or `0` for the always-available counts), never an error.
pub fn collect() -> HostInfo {
    let sys = System::new_all();
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|s| !s.is_empty());
    HostInfo {
        hostname: non_empty(System::host_name()),
        os: non_empty(System::long_os_version()),
        kernel: non_empty(System::kernel_version()),
        arch: {
            let a = System::cpu_arch();
            if a.trim().is_empty() {
                None
            } else {
                Some(a)
            }
        },
        cpu,
        physical_cores: System::physical_core_count(),
        logical_cores: sys.cpus().len(),
        total_memory_bytes: sys.total_memory(),
    }
}

/// Map an `Option<String>` to `None` when the string is missing or blank.
fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_populates_always_available_fields() {
        // Best-effort fields (hostname/os/cpu) can be absent on some platforms/CI, so only assert
        // the ones sysinfo reliably provides everywhere.
        let h = collect();
        assert!(h.logical_cores >= 1, "at least one logical CPU");
        assert!(h.total_memory_bytes > 0, "some physical memory");
        if let Some(pc) = h.physical_cores {
            assert!(pc >= 1);
            assert!(
                pc <= h.logical_cores,
                "physical cores ({pc}) can't exceed logical ({})",
                h.logical_cores
            );
        }
    }
}
