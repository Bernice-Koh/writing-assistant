//! Whether a focused element belongs to this application rather than to the user's writing
//! target.
//!
//! The assistant has windows of its own, and they take focus like anything else: without this
//! check the capture thread adopts the app's own UI as the tracked element and the overlay
//! follows itself. Comparing process ids alone is not enough, because WebView2 renders the
//! frontend in a separate `msedgewebview2.exe` process whose id differs from this one; that
//! process is a *descendant* of this one, so the test walks the parent chain instead.

use std::collections::HashMap;

use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// Caps the parent walk. The chain from a WebView2 host to this process is a couple of links
/// long, so this only ever matters if a snapshot comes back malformed enough to contain a
/// cycle, which would otherwise hang the capture thread.
const MAX_DEPTH: usize = 32;

/// Whether `pid` is this process or one of its descendants.
///
/// A failure to read the process table answers `false`: the cost of wrongly deciding a foreign
/// window is ours is that the user's own writing stops being tracked, which is worse than the
/// cost of briefly tracking our own window.
pub fn belongs_to_this_app(pid: u32) -> bool {
    let own = std::process::id();
    if pid == own {
        return true;
    }
    match parent_ids() {
        Ok(parents) => is_descendant(&parents, pid, own),
        Err(error) => {
            log::debug!("could not read the process table, treating pid {pid} as foreign: {error}");
            false
        }
    }
}

/// Every running process id mapped to its parent's, from one snapshot of the process table.
///
/// Rebuilt per call rather than cached: a process id is only unique while its process lives, so
/// a cached answer can outlive the process it described and misclassify whatever reuses the id.
/// Only focus changes reach this, and those arrive at human speed.
fn parent_ids() -> Result<HashMap<u32, u32>, windows::core::Error> {
    // SAFETY: the snapshot handle is owned here and closed by its `Drop`; `Process32FirstW` and
    // `Process32NextW` are handed an entry whose `dwSize` is set as they require, and are only
    // read while they report success.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?;
        let mut entry = PROCESSENTRY32W {
            dwSize: u32::try_from(size_of::<PROCESSENTRY32W>())
                .expect("PROCESSENTRY32W is far smaller than u32::MAX"),
            ..Default::default()
        };
        let mut parents = HashMap::new();
        if Process32FirstW(snapshot, &raw mut entry).is_ok() {
            loop {
                parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                if Process32NextW(snapshot, &raw mut entry).is_err() {
                    break;
                }
            }
        }
        Ok(parents)
    }
}

/// Walks `pid`'s parent chain looking for `ancestor`. Split from the snapshot call so the walk
/// itself, which holds the loop-termination reasoning, is testable without a live process table.
fn is_descendant(parents: &HashMap<u32, u32>, pid: u32, ancestor: u32) -> bool {
    let mut current = pid;
    for _ in 0..MAX_DEPTH {
        let Some(&parent) = parents.get(&current) else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        // A process that parents itself, or the idle process at id 0, terminates the chain;
        // without this the walk would spin until MAX_DEPTH for every foreign window.
        if parent == current || parent == 0 {
            return false;
        }
        current = parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(pairs: &[(u32, u32)]) -> HashMap<u32, u32> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn direct_child_is_a_descendant() {
        let parents = table(&[(100, 1), (200, 100)]);
        assert!(is_descendant(&parents, 200, 100));
    }

    #[test]
    fn grandchild_is_a_descendant() {
        // The shape this exists for: WebView2's host under the Tauri process under the shell.
        let parents = table(&[(100, 1), (200, 100), (300, 200)]);
        assert!(is_descendant(&parents, 300, 100));
    }

    #[test]
    fn unrelated_process_is_not_a_descendant() {
        let parents = table(&[(100, 1), (200, 100), (900, 1)]);
        assert!(!is_descendant(&parents, 900, 100));
    }

    #[test]
    fn a_parent_is_not_a_descendant_of_its_own_child() {
        let parents = table(&[(100, 1), (200, 100)]);
        assert!(!is_descendant(&parents, 100, 200));
    }

    #[test]
    fn unknown_pid_is_not_a_descendant() {
        assert!(!is_descendant(&table(&[(100, 1)]), 555, 100));
    }

    #[test]
    fn self_parenting_entry_terminates_instead_of_looping() {
        assert!(!is_descendant(&table(&[(200, 200)]), 200, 100));
    }

    #[test]
    fn cycle_terminates_instead_of_looping() {
        let parents = table(&[(200, 300), (300, 200)]);
        assert!(!is_descendant(&parents, 200, 100));
    }

    #[test]
    fn chain_to_the_idle_process_terminates() {
        let parents = table(&[(200, 4), (4, 0)]);
        assert!(!is_descendant(&parents, 200, 100));
    }

    #[test]
    fn chain_longer_than_the_depth_cap_gives_up() {
        // Each process parented by the previous, with the sought ancestor past the cap.
        let mut pairs: Vec<(u32, u32)> = (1..=MAX_DEPTH as u32 + 5).map(|n| (n + 1, n)).collect();
        pairs.push((1, 9999));
        assert!(!is_descendant(&table(&pairs), MAX_DEPTH as u32 + 6, 1));
    }
}
