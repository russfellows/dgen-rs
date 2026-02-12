// src/numa.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! NUMA topology detection and CPU pinning
//!
//! Uses hwlocality for cross-platform NUMA topology detection

use anyhow::Result;
use hwlocality::{object::types::ObjectType, Topology};
use std::collections::HashSet;

/// NUMA node information
#[derive(Debug, Clone)]
pub struct NumaNode {
    /// Node ID
    pub node_id: usize,
    /// CPU IDs in this NUMA node
    pub cpus: Vec<usize>,
    /// Memory in GB local to this node
    pub memory_gb: f64,
}

/// System NUMA topology
#[derive(Debug, Clone)]
pub struct NumaTopology {
    /// Number of NUMA nodes
    pub num_nodes: usize,
    /// Total physical cores
    pub physical_cores: usize,
    /// Total logical CPUs
    pub logical_cpus: usize,
    /// Per-NUMA node details
    pub nodes: Vec<NumaNode>,
    /// Is this a UMA system (single NUMA node)
    pub is_uma: bool,
}

impl NumaTopology {
    /// Detect NUMA topology from system using hwlocality
    pub fn detect() -> Result<Self> {
        tracing::debug!("Detecting NUMA topology via hwlocality...");

        let topology = Topology::new()?;

        // Get all NUMA nodes
        let numa_nodes: Vec<_> = topology.objects_with_type(ObjectType::NUMANode).collect();

        let num_nodes = numa_nodes.len().max(1); // At least 1 node
        let is_uma = num_nodes == 1;

        tracing::info!("Detected {} NUMA node(s)", num_nodes);

        // Build node details
        let nodes: Vec<NumaNode> = if numa_nodes.is_empty() {
            // No NUMA nodes detected - treat as single UMA node
            vec![NumaNode {
                node_id: 0,
                cpus: (0..num_cpus::get()).collect(),
                memory_gb: 0.0,
            }]
        } else {
            numa_nodes
                .iter()
                .filter_map(|node| {
                    let node_id = node.os_index()?;

                    // Get CPUs in this NUMA node's cpuset
                    let cpuset = node.cpuset()?;
                    let cpus: Vec<usize> = (0..topology.objects_with_type(ObjectType::PU).count())
                        .filter(|&cpu_id| cpuset.is_set(cpu_id))
                        .collect();

                    Some(NumaNode {
                        node_id,
                        cpus,
                        memory_gb: 0.0, // hwlocality can provide this if needed
                    })
                })
                .collect()
        };

        let physical_cores = num_cpus::get_physical();
        let logical_cpus = num_cpus::get();

        Ok(Self {
            num_nodes,
            physical_cores,
            logical_cpus,
            nodes,
            is_uma,
        })
    }

    /// Check if NUMA-aware optimizations should be enabled
    pub fn should_enable_numa_pinning(&self) -> bool {
        self.num_nodes > 1
    }

    /// Get deployment type description
    pub fn deployment_type(&self) -> &str {
        if self.is_uma {
            "UMA (single NUMA node - cloud VM or workstation)"
        } else {
            "NUMA (multi-socket system or large cloud VM)"
        }
    }

    /// Get CPUs for a specific NUMA node
    pub fn cpus_for_node(&self, node_id: usize) -> Option<&[usize]> {
        self.nodes
            .iter()
            .find(|n| n.node_id == node_id)
            .map(|n| n.cpus.as_slice())
    }
}

/// Detect number of NUMA nodes
///
/// Cloud VMs typically present as single NUMA node.
/// Bare metal multi-socket shows 2+ nodes.
#[allow(dead_code)] // May be used in future for additional validation
fn detect_numa_nodes() -> Result<usize> {
    tracing::trace!("detect_numa_nodes called");

    #[cfg(target_os = "linux")]
    {
        // Method 1: Check /sys/devices/system/node/
        let node_path = std::path::Path::new("/sys/devices/system/node");
        if node_path.exists() {
            let mut numa_nodes = Vec::new();

            for entry in std::fs::read_dir(node_path)? {
                let entry = entry?;
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                if name_str.starts_with("node") && name_str[4..].chars().all(|c| c.is_ascii_digit())
                {
                    if let Ok(node_id) = name_str[4..].parse::<usize>() {
                        numa_nodes.push(node_id);
                    }
                }
            }

            if !numa_nodes.is_empty() {
                return Ok(numa_nodes.len());
            }
        }

        // Method 2: Check /proc/cpuinfo for physical id
        if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
            let mut physical_ids = HashSet::new();

            for line in cpuinfo.lines() {
                if line.starts_with("physical id") {
                    if let Some(id_str) = line.split(':').nth(1) {
                        if let Ok(id) = id_str.trim().parse::<usize>() {
                            physical_ids.insert(id);
                        }
                    }
                }
            }

            if !physical_ids.is_empty() {
                return Ok(physical_ids.len());
            }
        }
    }

    // Fallback: Assume UMA
    tracing::debug!("Could not detect NUMA topology, assuming UMA");
    Ok(1)
}

/// Detect detailed NUMA topology using /sys interface
#[allow(dead_code)] // May be used in future for detailed topology analysis
fn detect_numa_topology_details() -> Result<Vec<NumaNode>> {
    #[cfg(target_os = "linux")]
    {
        let node_path = std::path::Path::new("/sys/devices/system/node");
        if !node_path.exists() {
            anyhow::bail!("NUMA topology not available");
        }

        let mut numa_nodes = Vec::new();

        for entry in std::fs::read_dir(node_path)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if name_str.starts_with("node") && name_str[4..].chars().all(|c| c.is_ascii_digit()) {
                if let Ok(node_id) = name_str[4..].parse::<usize>() {
                    let node_dir = entry.path();

                    // Read CPUs from cpulist
                    let mut cpus = Vec::new();
                    let cpulist_path = node_dir.join("cpulist");
                    if let Ok(cpulist) = std::fs::read_to_string(&cpulist_path) {
                        for range in cpulist.trim().split(',') {
                            if range.contains('-') {
                                let parts: Vec<&str> = range.split('-').collect();
                                if parts.len() == 2 {
                                    if let (Ok(start), Ok(end)) =
                                        (parts[0].parse::<usize>(), parts[1].parse::<usize>())
                                    {
                                        for cpu in start..=end {
                                            cpus.push(cpu);
                                        }
                                    }
                                }
                            } else if let Ok(cpu) = range.parse::<usize>() {
                                cpus.push(cpu);
                            }
                        }
                    }

                    // Read memory from meminfo
                    let mut memory_gb = 0.0;
                    let meminfo_path = node_dir.join("meminfo");
                    if let Ok(meminfo) = std::fs::read_to_string(&meminfo_path) {
                        for line in meminfo.lines() {
                            if line.contains("MemTotal:") {
                                let parts: Vec<&str> = line.split_whitespace().collect();
                                if parts.len() >= 4 {
                                    if let Ok(kb) = parts[3].parse::<f64>() {
                                        memory_gb = kb / 1024.0 / 1024.0;
                                    }
                                }
                                break;
                            }
                        }
                    }

                    numa_nodes.push(NumaNode {
                        node_id,
                        cpus,
                        memory_gb,
                    });
                }
            }
        }

        if numa_nodes.is_empty() {
            anyhow::bail!("No NUMA nodes detected");
        }

        numa_nodes.sort_by_key(|n| n.node_id);
        Ok(numa_nodes)
    }

    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("NUMA detection only supported on Linux");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_tracing() {
        use tracing_subscriber::{fmt, EnvFilter};
        let _ = fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .try_init();
    }

    #[test]
    fn test_detect_topology() {
        init_tracing();
        if let Ok(topology) = NumaTopology::detect() {
            println!("NUMA topology: {:?}", topology);
            assert!(topology.num_nodes >= 1);
            assert!(topology.physical_cores >= 1);
            assert!(topology.logical_cpus >= topology.physical_cores);
        }
    }
}
