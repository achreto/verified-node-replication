// Copyright © 2019-2022 VMware, Inc. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Allows to query information about the machine's CPU topology.

use std::fmt;

use hwloc2::*;
use lazy_static::lazy_static;
use serde::Serialize;

pub type Node = u64;
pub type Socket = u64;
pub type Core = u64;
pub type Cpu = u64;
pub type L1 = u64;
pub type L2 = u64;
pub type L3 = u64;

lazy_static! {
    pub static ref MACHINE_TOPOLOGY: MachineTopology = MachineTopology::new();
}
/// The strategy how threads are allocated in the system.
#[derive(Serialize, Copy, Clone, Eq, PartialEq)]
pub enum ThreadMapping {
    /// Don't do any pinning.
    #[allow(unused)]
    None,
    /// Allocate threads on the same socket (as much as possible).
    Sequential,
    /// fills up a numa node (cores first, then hyperthreads once all NUMA nodes are full)
    NUMAFill,
    /// Spread thread allocation out across sockets (as much as possible).
    #[allow(unused)]
    Interleave,
}

impl fmt::Display for ThreadMapping {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            ThreadMapping::None => write!(f, "None"),
            ThreadMapping::Sequential => write!(f, "Sequential"),
            ThreadMapping::Interleave => write!(f, "Interleave"),
            ThreadMapping::NUMAFill => write!(f, "NUMAFill"),
        }
    }
}

impl fmt::Debug for ThreadMapping {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            ThreadMapping::None => write!(f, "TM=None"),
            ThreadMapping::Sequential => write!(f, "TM=Sequential"),
            ThreadMapping::Interleave => write!(f, "TM=Interleave"),
            ThreadMapping::NUMAFill => write!(f, "TM=NUMAFill"),
        }
    }
}

/// NUMA Node information.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Copy, Clone)]
pub struct NodeInfo {
    /// Node index
    pub node: Node,
    /// Memory in bytes
    pub memory: u64,
}

/// Information about a CPU in the system.
#[derive(Eq, PartialEq, Clone, Copy)]
pub struct CpuInfo {
    pub node: Option<NodeInfo>,
    pub socket: Socket,
    pub core: Core,
    pub cpu: Cpu,
    pub l1: L1,
    pub l2: L2,
    pub l3: L3,
}

impl std::fmt::Debug for CpuInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "CpuInfo {{ core/l1/l2: {}/{}/{}, cpu: {}, socket/l3/node: {}/{}/{:?} }}",
            self.core, self.l1, self.l2, self.cpu, self.socket, self.l3, self.node
        )
    }
}

#[derive(Debug)]
pub struct MachineTopology {
    data: Vec<CpuInfo>,
}

impl MachineTopology {
    pub fn new() -> MachineTopology {
        let mut data: Vec<CpuInfo> = Default::default();

        let topo = Topology::new().expect("Can't retrieve Topology");
        let cpus = topo
            .objects_with_type(&ObjectType::PU)
            .expect("Can't find CPUs");

        for cpu in cpus {
            let mut parent = cpu.parent();

            // Find the parent core of the CPU
            while parent.is_some() && parent.unwrap().object_type() != ObjectType::Core {
                parent = parent.unwrap().parent();
            }
            let core = parent.expect("PU has no Core?");

            // Find the parent L1 cache of the CPU
            while parent.is_some()
                && (parent.unwrap().object_type() != ObjectType::L1Cache
                    || parent.unwrap().cache_attributes().unwrap().depth() < 1)
            {
                parent = parent.unwrap().parent();
            }
            let l1 = parent.expect("Core doesn't have a L1 cache?");

            // Find the parent L2 cache of the CPU
            while parent.is_some()
                && (parent.unwrap().object_type() != ObjectType::L2Cache
                    || parent.unwrap().cache_attributes().unwrap().depth() < 2)
            {
                parent = parent.unwrap().parent();
            }
            let l2 = parent.expect("Core doesn't have a L2 cache?");

            // Find the parent socket/L3 cache of the CPU
            while parent.is_some()
                && (parent.unwrap().object_type() != ObjectType::L3Cache
                    || parent.unwrap().cache_attributes().unwrap().depth() < 3)
            {
                parent = parent.unwrap().parent();
            }
            let socket = parent.expect("Core doesn't have a L3 cache (socket)?");

            // Find the parent NUMA node of the CPU
            while parent.is_some() && parent.unwrap().object_type() != ObjectType::NUMANode {
                parent = parent.unwrap().parent();
            }
            let numa_node = parent.map(|n| NodeInfo {
                node: n.os_index() as Node,
                memory: n.total_memory(),
            });

            let cpu_info = CpuInfo {
                node: numa_node,
                socket: socket.logical_index() as Socket,
                core: core.logical_index() as Core,
                cpu: cpu.os_index() as Cpu,
                l1: l1.logical_index() as L1,
                l2: l2.logical_index() as L2,
                l3: socket.logical_index() as L3,
            };

            data.push(cpu_info);
        }

        MachineTopology { data }
    }

    /// Return how many processing units that the system has
    pub fn cores(&self) -> usize {
        self.data.len()
    }

    pub fn sockets(&self) -> Vec<Socket> {
        let mut sockets: Vec<Cpu> = self.data.iter().map(|t| t.socket).collect();
        sockets.sort();
        sockets.dedup();
        sockets
    }

    pub fn nodes(&self) -> Vec<Node> {
        let mut nodes: Vec<Cpu> = self
            .data
            .iter()
            .map(|t| t.node.map_or_else(|| 0, |n| n.node))
            .collect();
        nodes.sort();
        nodes.dedup();
        nodes
    }

    pub fn cpus_on_node(&self, node: Node) -> Vec<&CpuInfo> {
        self.data.iter().filter(|t| t.socket == node).collect()
    }

    pub fn cpus_on_socket(&self, socket: Socket) -> Vec<&CpuInfo> {
        self.data.iter().filter(|t| t.socket == socket).collect()
    }

    pub fn allocate(&self, strategy: ThreadMapping, how_many: usize, use_ht: bool) -> Vec<CpuInfo> {
        let v = Vec::with_capacity(how_many);
        let mut cpus = self.data.clone();

        if !use_ht {
            cpus.sort_by_key(|c| c.core);
            cpus.dedup_by(|a, b| a.core == b.core);
        }

        match strategy {
            ThreadMapping::None => v,
            ThreadMapping::Interleave => {
                let mut ht1 = cpus.clone();

                // Get cores first, remove HT
                ht1.sort_by_key(|c| c.core);
                ht1.dedup_by(|a, b| a.core == b.core);

                // Add the HTs removed by dedup at the end
                let mut ht2 = vec![];
                for cpu in cpus {
                    if !ht1.contains(&cpu) {
                        ht2.push(cpu);
                    }
                }
                ht2.sort_by_key(|c| c.core);
                ht1.extend(ht2);

                // now get the sockets
                let sockets = self.sockets();
                let num_sockets = sockets.len();

                // calculate how many CPUs we need per socket, rounded up to the next core
                let cpus_per_socket = (how_many + num_sockets - 1) / num_sockets;

                let mut allocated : Vec<Vec<CpuInfo>> = sockets.iter().map(|_| Vec::new()).collect();
                let mut num_alloc_cpus = 0;
                for cpu in ht1.into_iter() {
                    if num_alloc_cpus == how_many {
                        break;
                    }
                    // if we already reached the target on that node, skip that core
                    // XXX: assumes all node have the same number of cores
                    if allocated[cpu.socket as usize].len() == cpus_per_socket {
                        continue;
                    }

                    allocated.get_mut(cpu.socket as usize).unwrap().push(cpu);
                    num_alloc_cpus += 1;
                }

                let c : Vec<CpuInfo> = allocated.into_iter().flatten().collect();
                assert!(c.len() == how_many);
                c
            }
            ThreadMapping::Sequential => {
                cpus.sort_by(|a, b| {
                    if a.socket != b.socket {
                        // Allocate from the same socket first
                        a.socket.partial_cmp(&b.socket).unwrap()
                    } else {
                        // But avoid placing on hyper-threads core until all cores are used
                        a.cpu.partial_cmp(&b.cpu).unwrap()
                    }
                });
                let c = cpus.iter().take(how_many).map(|c| *c).collect();
                c
            }
            ThreadMapping::NUMAFill => {
                let mut ht1 = cpus.clone();

                // Get cores first, remove HT
                ht1.sort_by_key(|c| c.core);
                ht1.dedup_by(|a, b| a.core == b.core);

                // Add the HTs removed by dedup at the end
                let mut ht2 = vec![];
                for cpu in cpus {
                    if !ht1.contains(&cpu) {
                        ht2.push(cpu);
                    }
                }
                // sort the core list by socket, and combine them
                ht2.sort_by_key(|c| c.socket);
                ht1.sort_by_key(|c| c.socket);
                ht1.extend(ht2);

                // ht1 should now have all cores sorted by socket, then all hyperthreads sorted by socket

                ht1.into_iter().take(how_many).collect()
            }
        }
    }
}
