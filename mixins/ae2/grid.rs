// Applied Energistics 2's ME network, ported by hand from appliedenergistics2-fabric-15.4.12.jar: which
// devices form one grid (appeng.me.Grid), channels (appeng.me.pathfinding: PathingCalculation with
// a controller, AdHocChannelUpdater without one, ControllerValidator), energy (EnergyService) and
// the network's storage across its cells by priority (appeng.me.storage.NetworkStorage,
// MEInventoryHandler.isPreferredStorageFor). Devices are numbered nodes and their connections are
// pairs: the game side says which blocks touch. No `//!` comments: module payloads can't hold
// inner attributes.

use super::port_ae2_core::{Actionable, AeKey, BasicCellInventory, IncludeExclude, KeyCounter};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

/// AE2's channel mode (ChannelMode, AEConfig `channels`): DEFAULT 1, X2..X4, or 0 for INFINITE. It scales what a
/// cable carries (`cableCapacityFactor`) and how many devices an ad hoc network runs (`adHocNetworkChannels`).
pub static CHANNEL_MODE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

pub fn channel_factor() -> u32 {
    CHANNEL_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// ChannelMode.getAdHocNetworkChannels.
pub fn ad_hoc_channels() -> usize {
    match channel_factor() {
        0 => usize::MAX,
        f => 8 * f as usize,
    }
}

/// A node's channel capacity in the current mode (GridNode.getMaxChannels).
fn capacity(base: u32) -> u32 {
    match channel_factor() {
        0 => u32::MAX,
        f => base * f,
    }
}

/// A device on the network, as the game side numbers it (a block position packed, say).
pub type NodeId = u64;

/// What a device is to the network (`GridFlags`, the node's channel capacity, idle power).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeKind {
    pub controller: bool,
    /// Works only with a channel (terminals, buses, drives); cables don't.
    pub requires_channel: bool,
    /// Channels that may pass through it: 8 for a cable or a device, 32 for dense cable.
    pub max_channels: u32,
    /// Dense cable: paths through it are tried first (`DENSE_CAPACITY`).
    pub dense: bool,
    /// AE per tick the device draws while on.
    pub idle_power: f64,
}

impl NodeKind {
    pub const CABLE: Self = Self { controller: false, requires_channel: false, max_channels: 8, dense: false, idle_power: 0.0 };
    pub const DENSE_CABLE: Self = Self { controller: false, requires_channel: false, max_channels: 32, dense: true, idle_power: 0.0 };
    pub const CONTROLLER: Self = Self { controller: true, requires_channel: false, max_channels: 32, dense: true, idle_power: 3.0 };
    /// A terminal, bus, drive, interface: one channel.
    pub fn device(idle_power: f64) -> Self {
        Self { controller: false, requires_channel: true, max_channels: 8, dense: false, idle_power }
    }
    /// An energy cell, acceptor or other block that needs no channel.
    pub fn passive(idle_power: f64) -> Self {
        Self { controller: false, requires_channel: false, max_channels: 8, dense: false, idle_power }
    }
}

/// Why a grid's controllers don't run it (`ControllerState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerState {
    NoController,
    ControllerOnline,
    /// Controllers that don't form one block of at most 7x7x7 (`ControllerValidator`).
    ControllerConflict,
}

/// One grid: the devices it joins, and what pathing gave them.
#[derive(Clone, Debug)]
pub struct Grid {
    pub nodes: BTreeSet<NodeId>,
    pub controller_state: ControllerState,
    /// The devices that have a channel and so are online when powered.
    pub with_channel: HashSet<NodeId>,
    /// Channels carried through each node (`getUsedChannels`), for cable rendering.
    pub used_channels: HashMap<NodeId, u32>,
    pub channels_in_use: u32,
    /// AdHocNetworkError.NESTED_P2P_TUNNEL: no controller, and an ME tunnel's channel would pass through another.
    pub nested_p2p: bool,
}

impl Grid {
    /// Whether a device works: it has a channel or needs none, and the grid isn't in conflict.
    pub fn is_active(&self, node: NodeId, kinds: &HashMap<NodeId, NodeKind>) -> bool {
        if self.controller_state == ControllerState::ControllerConflict {
            return false;
        }
        match kinds.get(&node) {
            Some(kind) if kind.requires_channel => self.with_channel.contains(&node),
            Some(_) => true,
            None => false,
        }
    }
}

/// All devices and their connections; `grids` recomputes who is on which grid with which channels.
#[derive(Clone, Debug, Default)]
pub struct Network {
    kinds: HashMap<NodeId, NodeKind>,
    /// Block positions, for the controller size rule.
    positions: HashMap<NodeId, [i32; 3]>,
    links: HashMap<NodeId, BTreeSet<NodeId>>,
    /// ME P2P tunnels on one frequency, by their main nodes (an input with its outputs). MEP2PTunnelPart has two nodes:
    /// the main one (REQUIRE_CHANNEL, COMPRESSED_CHANNEL) on its host, and the outer one (DENSE_CAPACITY,
    /// CANNOT_CARRY_COMPRESSED) on what's in front of it. The outer nodes of a pair join (`updateConnections`) once both
    /// main nodes are online on one grid.
    tunnels: BTreeMap<NodeId, BTreeSet<NodeId>>,
    /// A tunnel's main node to its outer one, which only this file knows of: no `Grid` lists it.
    outers: HashMap<NodeId, NodeId>,
    outer_nodes: HashSet<NodeId>,
    /// Nodes of one multiblock (a CPU, a spatial structure), by a number for it.
    multiblocks: HashMap<NodeId, u64>,
}

impl Network {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, id: NodeId, kind: NodeKind, pos: [i32; 3]) {
        self.kinds.insert(id, kind);
        self.positions.insert(id, pos);
        self.links.entry(id).or_default();
    }

    pub fn remove_node(&mut self, id: NodeId) {
        self.kinds.remove(&id);
        self.positions.remove(&id);
        if let Some(others) = self.links.remove(&id) {
            for o in others {
                if let Some(l) = self.links.get_mut(&o) {
                    l.remove(&id);
                }
            }
        }
        self.tunnels.remove(&id);
        for ends in self.tunnels.values_mut() {
            ends.remove(&id);
        }
    }

    /// The outer node of the ME tunnel with this main node, made the first time.
    pub fn tunnel_outer(&mut self, main: NodeId) -> NodeId {
        if let Some(outer) = self.outers.get(&main) {
            return *outer;
        }
        // From the top of the range: `pack` only comes near it for a block at x and z -1, y 4095 and down.
        let outer = u64::MAX - self.outers.len() as u64;
        let pos = self.positions.get(&main).copied().unwrap_or([0; 3]);
        self.add_node(outer, NodeKind { controller: false, requires_channel: false, max_channels: 32, dense: true, idle_power: 0.0 }, pos);
        self.outers.insert(main, outer);
        self.outer_nodes.insert(outer);
        outer
    }

    /// An input and an output on one frequency, by their main nodes.
    pub fn connect_tunnel(&mut self, a: NodeId, b: NodeId) {
        if a != b && self.kinds.contains_key(&a) && self.kinds.contains_key(&b) {
            self.tunnel_outer(a);
            self.tunnel_outer(b);
            self.tunnels.entry(a).or_default().insert(b);
        }
    }

    pub fn connect(&mut self, a: NodeId, b: NodeId) {
        if a != b && self.kinds.contains_key(&a) && self.kinds.contains_key(&b) {
            self.links.entry(a).or_default().insert(b);
            self.links.entry(b).or_default().insert(a);
        }
    }

    pub fn disconnect(&mut self, a: NodeId, b: NodeId) {
        self.links.entry(a).or_default().remove(&b);
        self.links.entry(b).or_default().remove(&a);
    }

    pub fn kinds(&self) -> &HashMap<NodeId, NodeKind> {
        &self.kinds
    }

    pub fn kind(&self, id: NodeId) -> Option<NodeKind> {
        self.kinds.get(&id).copied()
    }

    /// The grids: connected devices, each with its controller state and channels.
    pub fn grids(&self) -> Vec<Grid> {
        // MEP2PTunnelPart.updateConnections happens as grids settle: a pair joined can change who has a channel. Stop at a
        // fixed point; a rig that never has one (joining takes the channels that let it join) ends on its last round.
        let mut joined: BTreeSet<(NodeId, NodeId)> = BTreeSet::new();
        let mut grids = self.grids_once();
        for _ in 0..8 {
            let mut next = BTreeSet::new();
            let on = |node: &NodeId| grids.iter().position(|g| g.with_channel.contains(node));
            for (input, outputs) in &self.tunnels {
                for output in outputs {
                    if let (Some(a), Some(b)) = (on(input), on(output))
                        && a == b
                    {
                        next.insert((*input, *output));
                    }
                }
            }
            if next == joined {
                break;
            }
            joined = next;
            let mut net = self.clone();
            for (input, output) in &joined {
                net.connect(self.outers[input], self.outers[output]);
            }
            grids = net.grids_once();
        }
        grids
    }

    fn grids_once(&self) -> Vec<Grid> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut ids: Vec<_> = self.kinds.keys().copied().collect();
        ids.sort();
        for start in ids {
            if !seen.insert(start) {
                continue;
            }
            let mut members = BTreeSet::from([start]);
            let mut queue = VecDeque::from([start]);
            while let Some(n) = queue.pop_front() {
                for &o in self.links.get(&n).into_iter().flatten() {
                    if seen.insert(o) {
                        members.insert(o);
                        queue.push_back(o);
                    }
                }
            }
            let mut grid = self.path(members);
            grid.nodes.retain(|n| !self.outer_nodes.contains(n));
            grid.used_channels.retain(|n, _| !self.outer_nodes.contains(n));
            if !grid.nodes.is_empty() {
                out.push(grid);
            }
        }
        out
    }

    /// The grid a device is on.
    pub fn grid_of(&self, id: NodeId) -> Option<Grid> {
        self.grids().into_iter().find(|g| g.nodes.contains(&id))
    }

    fn controller_state(&self, members: &BTreeSet<NodeId>) -> ControllerState {
        let controllers: Vec<NodeId> = members.iter().copied().filter(|n| self.kinds[n].controller).collect();
        if controllers.is_empty() {
            return ControllerState::NoController;
        }
        // ControllerValidator: one connected group of controllers, at most 7 blocks on each axis.
        let set: HashSet<NodeId> = controllers.iter().copied().collect();
        let mut reached = HashSet::from([controllers[0]]);
        let mut queue = VecDeque::from([controllers[0]]);
        while let Some(n) = queue.pop_front() {
            for o in self.links[&n].iter().filter(|o| set.contains(o)) {
                if reached.insert(*o) {
                    queue.push_back(*o);
                }
            }
        }
        if reached.len() != set.len() {
            return ControllerState::ControllerConflict;
        }
        // ControllerValidator.hasControllerCross: a controller with neighbours both ways on more than one axis.
        let at: HashSet<[i32; 3]> = controllers.iter().map(|c| self.positions[c]).collect();
        for c in &controllers {
            let p = self.positions[c];
            let full = (0..3)
                .filter(|&axis| {
                    let (mut lo, mut hi) = (p, p);
                    lo[axis] -= 1;
                    hi[axis] += 1;
                    at.contains(&lo) && at.contains(&hi)
                })
                .count();
            if full > 1 {
                return ControllerState::ControllerConflict;
            }
        }
        for axis in 0..3 {
            let values = controllers.iter().map(|c| self.positions[c][axis]);
            let (min, max) = values.clone().fold((i32::MAX, i32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
            if max - min + 1 > 7 {
                return ControllerState::ControllerConflict;
            }
        }
        ControllerState::ControllerOnline
    }

    /// Channels for one grid's devices.
    fn path(&self, members: BTreeSet<NodeId>) -> Grid {
        let state = self.controller_state(&members);
        let mut grid = Grid {
            nodes: members,
            controller_state: state,
            with_channel: HashSet::new(),
            used_channels: HashMap::new(),
            channels_in_use: 0,
            nested_p2p: false,
        };
        match state {
            ControllerState::ControllerConflict => {}
            ControllerState::NoController => {
                // PathingService.calculateAdHocChannels: a compressed channel where one can't be carried is 0 channels.
                grid.nested_p2p = grid.nodes.iter().any(|n| self.outers.contains_key(n)) && grid.nodes.iter().any(|n| self.outer_nodes.contains(n));
                // AdHocChannelUpdater: up to 8 devices work without a controller, more and none do.
                let wanting: Vec<NodeId> = grid.nodes.iter().copied().filter(|n| self.kinds[n].requires_channel).collect();
                if !grid.nested_p2p && self.channels_wanted(&grid.nodes) <= ad_hoc_channels() {
                    grid.channels_in_use = self.channels_wanted(&grid.nodes) as u32;
                    for n in &grid.nodes {
                        grid.used_channels.insert(*n, grid.channels_in_use);
                    }
                    grid.with_channel.extend(wanting);
                }
            }
            ControllerState::ControllerOnline => self.path_from_controllers(&mut grid),
        }
        grid
    }

    /// PathingCalculation: a breadth-first tree from the controllers, dense cable first, each
    /// channel-hungry device taking one channel on every node up its route while none is full.
    fn path_from_controllers(&self, grid: &mut Grid) {
        let mut route: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        let mut queues: [VecDeque<NodeId>; 2] = [VecDeque::new(), VecDeque::new()];
        let controllers: Vec<NodeId> = grid.nodes.iter().copied().filter(|n| self.kinds[n].controller).collect();
        for &c in &controllers {
            route.insert(c, None);
        }
        for &c in &controllers {
            for &o in &self.links[&c] {
                if !route.contains_key(&o) {
                    route.insert(o, Some(c));
                    queues[if self.kinds[&o].dense { 0 } else { 1 }].push_back(o);
                    self.try_use_channel(o, &route, grid);
                }
            }
        }
        for q in 0..2 {
            while let Some(n) = queues[q].pop_front() {
                for &o in &self.links[&n] {
                    if route.contains_key(&o) {
                        continue;
                    }
                    route.insert(o, Some(n));
                    self.try_use_channel(o, &route, grid);
                    let index = if self.kinds[&o].dense { 0 } else { 1 }.max(q);
                    queues[index].push_back(o);
                }
            }
        }
    }

    /// PathingCalculation.multiblocksWithChannel: a multiblock's nodes share one channel.
    pub fn set_multiblock(&mut self, node: NodeId, group: u64) {
        self.multiblocks.insert(node, group);
    }

    /// The channels these nodes need: a multiblock counts once.
    pub fn channels_wanted<'a>(&self, nodes: impl IntoIterator<Item = &'a NodeId>) -> usize {
        let mut groups = HashSet::new();
        nodes
            .into_iter()
            .filter(|n| self.kinds.get(*n).is_some_and(|k| k.requires_channel))
            .filter(|n| self.multiblocks.get(*n).is_none_or(|g| groups.insert(*g)))
            .count()
    }

    fn try_use_channel(&self, start: NodeId, route: &HashMap<NodeId, Option<NodeId>>, grid: &mut Grid) {
        if !self.kinds[&start].requires_channel {
            return;
        }
        if let Some(group) = self.multiblocks.get(&start)
            && grid.with_channel.iter().any(|n| self.multiblocks.get(n) == Some(group))
        {
            grid.with_channel.insert(start);
            return;
        }
        let path: Vec<NodeId> = std::iter::successors(Some(start), |n| route.get(n).copied().flatten())
            .filter(|n| !self.kinds[n].controller)
            .collect();
        // PathingCalculation.tryUseChannel: a COMPRESSED_CHANNEL through a CANNOT_CARRY_COMPRESSED node gets none.
        if self.outers.contains_key(&start) && path.iter().any(|n| self.outer_nodes.contains(n)) {
            return;
        }
        if path.iter().any(|n| grid.used_channels.get(n).copied().unwrap_or(0) >= capacity(self.kinds[n].max_channels)) {
            return;
        }
        for n in path {
            *grid.used_channels.entry(n).or_insert(0) += 1;
        }
        grid.with_channel.insert(start);
        grid.channels_in_use += 1;
    }
}

/// `EnergyService`: the AE stored in a grid's controllers and energy cells.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EnergyStore {
    pub stored: f64,
    pub capacity: f64,
}

impl EnergyStore {
    pub fn new(capacity: f64) -> Self {
        Self { stored: 0.0, capacity }
    }

    /// `injectPower`: answers what didn't fit.
    pub fn inject(&mut self, amount: f64, mode: Actionable) -> f64 {
        let fits = amount.min(self.capacity - self.stored).max(0.0);
        if mode == Actionable::Modulate {
            self.stored += fits;
        }
        amount - fits
    }

    /// `extractAEPower`: answers what there was.
    pub fn extract(&mut self, amount: f64, mode: Actionable) -> f64 {
        let got = amount.min(self.stored).max(0.0);
        if mode == Actionable::Modulate {
            self.stored -= got;
        }
        got
    }

    pub fn is_powered(&self) -> bool {
        self.stored > 0.0001
    }

    /// One tick of the devices' idle draw; false when it couldn't be paid.
    pub fn tick_idle(&mut self, idle: f64) -> bool {
        self.extract(idle, Actionable::Modulate) + 0.0001 >= idle
    }
}

/// Something the network stores in (`MEStorage`): a cell in a drive, a storage bus on a chest.
pub trait MeStorage {
    fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64;
    fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64;
    fn available_stacks(&self, out: &mut KeyCounter);
    /// Tried before the others of its priority (`isPreferredStorageFor`).
    fn is_preferred_for(&self, what: &AeKey) -> bool {
        let _ = what;
        false
    }
    /// The storage itself, to reach its own type (a cell to save) through a `Box<dyn MeStorage>`.
    fn as_any(&mut self) -> &mut dyn std::any::Any;
}

impl MeStorage for Box<dyn MeStorage> {
    fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        (**self).insert(what, amount, mode)
    }
    fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        (**self).extract(what, amount, mode)
    }
    fn available_stacks(&self, out: &mut KeyCounter) {
        (**self).available_stacks(out)
    }
    fn is_preferred_for(&self, what: &AeKey) -> bool {
        (**self).is_preferred_for(what)
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        (**self).as_any()
    }
}

impl MeStorage for BasicCellInventory {
    fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        BasicCellInventory::insert(self, what, amount, mode)
    }
    fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        BasicCellInventory::extract(self, what, amount, mode)
    }
    fn available_stacks(&self, out: &mut KeyCounter) {
        BasicCellInventory::available_stacks(self, out)
    }
    /// A cell partitioned to the key, or one that already holds it.
    fn is_preferred_for(&self, what: &AeKey) -> bool {
        (self.partition_mode() == IncludeExclude::Whitelist && self.is_listed(what))
            || self.amount_of(what) > 0
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// CraftingServiceStorage: what enters a network that a crafting CPU waits for goes to the CPU. Entering storage is
/// noted here, and the CPUs take it from there on their next tick.
#[derive(Default)]
pub struct CpuWatch {
    pub keys: Vec<AeKey>,
    pub log: Vec<(usize, AeKey, i64)>,
    pub grid: usize,
    /// The CPUs' own storing, which isn't for them to take back.
    pub quiet: bool,
}

impl CpuWatch {
    fn note(&mut self, what: &AeKey, amount: i64) {
        if !self.quiet && self.keys.contains(what) {
            self.log.push((self.grid, what.clone(), amount));
        }
    }
}

thread_local! {
    pub static CPU_WATCH: std::cell::RefCell<CpuWatch> = std::cell::RefCell::new(CpuWatch::default());
}

/// `NetworkStorage`: the network's storages by priority, highest first for inserts and lowest
/// first for extracts. AE2 walks every storage for each operation; with thousands of cells that walk is the
/// whole cost, so this one keeps an index of which storages hold each key (built on first use, kept up as items
/// go in; an entry for a storage that has since run out is harmless). Volatile storages (a storage bus's
/// container, which changes outside the network) stay out of the index and are always asked.
pub struct NetworkStorage<S> {
    mounts: BTreeMap<std::cmp::Reverse<i32>, Vec<(u64, S)>>,
    volatile: std::collections::HashSet<u64>,
    /// Each storage's place: its priority and its index in that priority's list.
    places: HashMap<u64, (std::cmp::Reverse<i32>, usize)>,
    index: Option<HashMap<AeKey, Vec<u64>>>,
    /// What the indexed storages hold in all, kept up as items move (a terminal's list reads it, not every cell).
    totals: KeyCounter,
}

impl<S> Default for NetworkStorage<S> {
    fn default() -> Self {
        Self { mounts: BTreeMap::new(), volatile: Default::default(), places: HashMap::new(), index: None, totals: KeyCounter::new() }
    }
}

impl<S: MeStorage> NetworkStorage<S> {
    pub fn new() -> Self {
        Self::default()
    }

    fn replace_places(&mut self) {
        self.places = self
            .mounts
            .iter()
            .flat_map(|(p, list)| list.iter().enumerate().map(move |(i, (id, _))| (*id, (*p, i))))
            .collect();
    }

    /// Adds a storage under the caller's id.
    pub fn mount(&mut self, id: u64, priority: i32, storage: S) {
        self.mounts.entry(std::cmp::Reverse(priority)).or_default().push((id, storage));
        self.replace_places();
        self.index = None;
    }

    /// Adds a storage whose contents change outside the network: always asked, never indexed.
    pub fn mount_volatile(&mut self, id: u64, priority: i32, storage: S) {
        self.volatile.insert(id);
        self.mounts.entry(std::cmp::Reverse(priority)).or_default().push((id, storage));
        self.replace_places();
    }

    pub fn unmount(&mut self, id: u64) -> Option<S> {
        let mut taken = None;
        self.mounts.retain(|_, list| {
            if let Some(i) = list.iter().position(|(sid, _)| *sid == id) {
                taken = Some(list.remove(i).1);
            }
            !list.is_empty()
        });
        if taken.is_some() {
            self.replace_places();
            if !self.volatile.remove(&id) {
                self.index = None;
            }
        }
        taken
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut S> {
        let (p, i) = *self.places.get(&id)?;
        self.mounts.get_mut(&p)?.get_mut(i).map(|(_, s)| s)
    }

    pub fn storages(&self) -> impl Iterator<Item = (u64, &S)> {
        self.mounts.values().flatten().map(|(id, s)| (*id, s))
    }

    fn ensure_index(&mut self) {
        if self.index.is_some() {
            return;
        }
        let mut index: HashMap<AeKey, Vec<u64>> = HashMap::new();
        let mut totals = KeyCounter::new();
        for (id, s) in self.mounts.values().flatten() {
            if self.volatile.contains(id) {
                continue;
            }
            let mut held = KeyCounter::new();
            s.available_stacks(&mut held);
            for (key, n) in held.iter() {
                if n > 0 {
                    index.entry(key.clone()).or_default().push(*id);
                    totals.add(key, n);
                }
            }
        }
        self.index = Some(index);
        self.totals = totals;
    }

    /// The storages that may hold `what`: the indexed ones and the volatile ones, highest priority first.
    fn holders(&self, what: &AeKey) -> Vec<u64> {
        let mut ids: Vec<u64> = self.index.as_ref().and_then(|i| i.get(what)).cloned().unwrap_or_default();
        ids.extend(self.volatile.iter().copied());
        ids.sort_by_key(|id| self.places.get(id).copied());
        ids.dedup();
        ids
    }

    /// Counts `n` (negative: taken) of `what` in or out of storage `id`.
    fn count(&mut self, id: u64, what: &AeKey, n: i64) {
        if n == 0 || self.volatile.contains(&id) || self.index.is_none() {
            return;
        }
        if n > 0 {
            self.totals.add(what, n);
            self.note(id, what);
        } else {
            self.totals.remove(what, -n);
        }
    }

    fn note(&mut self, id: u64, what: &AeKey) {
        if self.volatile.contains(&id) {
            return;
        }
        if let Some(index) = self.index.as_mut() {
            let list = index.entry(what.clone()).or_default();
            if !list.contains(&id) {
                list.push(id);
            }
        }
    }

    /// `insert`: the storages already holding it (or partitioned to it) first, highest priority first, then
    /// the rest in priority order.
    pub fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        let done = self.insert_stored(what, amount, mode);
        if done > 0 && mode == Actionable::Modulate {
            CPU_WATCH.with_borrow_mut(|w| w.note(what, done));
        }
        done
    }

    /// NetworkStorage.insert: priorities highest first; inside one, the storages that prefer it (holding it, or
    /// partitioned to it), then the rest.
    fn insert_stored(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        self.ensure_index();
        let mut remaining = amount;
        let groups: Vec<Vec<u64>> = self.mounts.values().map(|list| list.iter().map(|(id, _)| *id).collect()).collect();
        for group in groups {
            for preferred in [true, false] {
                for &id in &group {
                    if remaining <= 0 {
                        return amount;
                    }
                    let Some(s) = self.get_mut(id) else { continue };
                    if s.is_preferred_for(what) != preferred {
                        continue;
                    }
                    let n = s.insert(what, remaining, mode);
                    remaining -= n;
                    if mode == Actionable::Modulate {
                        self.count(id, what, n);
                    }
                }
            }
        }
        amount - remaining
    }

    /// NetworkStorage.extract: priorities lowest first; inside one, in the order they were mounted.
    pub fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        self.ensure_index();
        let mut extracted = 0;
        let holders = self.holders(what);
        let mut order: Vec<u64> = Vec::with_capacity(holders.len());
        let mut at = holders.len();
        // `holders` is highest priority first: its runs of one priority, last to first, each kept in order.
        while at > 0 {
            let place = self.places.get(&holders[at - 1]).map(|p| p.0);
            let mut start = at - 1;
            while start > 0 && self.places.get(&holders[start - 1]).map(|p| p.0) == place {
                start -= 1;
            }
            order.extend_from_slice(&holders[start..at]);
            at = start;
        }
        for id in order {
            if extracted >= amount {
                break;
            }
            if let Some(s) = self.get_mut(id) {
                let n = s.extract(what, amount - extracted, mode);
                extracted += n;
                if mode == Actionable::Modulate {
                    self.count(id, what, -n);
                }
            }
        }
        extracted
    }

    pub fn available_stacks(&self, out: &mut KeyCounter) {
        for (_, s) in self.storages() {
            s.available_stacks(out);
        }
    }

    /// What the terminal shows: the kept totals and what the volatile storages hold now.
    pub fn contents(&mut self) -> KeyCounter {
        self.ensure_index();
        let mut out = self.totals.clone();
        for (id, s) in self.mounts.values().flatten() {
            if self.volatile.contains(id) {
                s.available_stacks(&mut out);
            }
        }
        out.remove_zeros();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::port_ae2_core::{CellContents, CellType, CellUpgrades, FuzzyMode, StorageTier};
    use super::*;

    /// A line of nodes along x, each connected to the next.
    fn line(net: &mut Network, kinds: &[NodeKind]) {
        for (i, kind) in kinds.iter().enumerate() {
            net.add_node(i as u64, *kind, [i as i32, 0, 0]);
            if i > 0 {
                net.connect(i as u64 - 1, i as u64);
            }
        }
    }

    #[test]
    fn storage_index_finds_items_across_many_cells() {
        let cell = CellType::item_cell(StorageTier::ALL[0]);
        let mut st: NetworkStorage<BasicCellInventory> = NetworkStorage::new();
        for c in 0..200 {
            st.mount(c, 0, BasicCellInventory::new(cell, &[], FuzzyMode::IgnoreAll, CellUpgrades::default(), &CellContents::default()));
        }
        let keys: Vec<AeKey> = (0..2000).map(|k| AeKey::item("minecraft:stone", &format!("n{k}"), 0, 0)).collect();
        let mut stored = 0;
        for k in &keys {
            stored += st.insert(k, 10, Actionable::Modulate);
        }
        assert_eq!(stored, 20_000);
        assert_eq!(st.contents().size(), 2000);
        // Every key comes back out, wherever it went.
        for k in &keys {
            assert_eq!(st.extract(k, 10, Actionable::Modulate), 10);
        }
        assert!(st.contents().is_empty());
        // More of a key goes where it already is first, and a remount reindexes.
        st.insert(&keys[0], 5, Actionable::Modulate);
        let first = st.unmount(0).unwrap();
        st.mount(0, 0, first);
        assert_eq!(st.extract(&keys[0], 5, Actionable::Simulate), 5);
    }

    #[test]
    fn eight_devices_work_without_a_controller() {
        let mut net = Network::new();
        let mut kinds = vec![NodeKind::CABLE];
        kinds.extend(std::iter::repeat(NodeKind::device(1.0)).take(8));
        line(&mut net, &kinds);
        let grid = net.grid_of(0).unwrap();
        assert_eq!(grid.controller_state, ControllerState::NoController);
        assert_eq!(grid.with_channel.len(), 8);
        net.add_node(100, NodeKind::device(1.0), [9, 0, 0]);
        net.connect(8, 100);
        assert!(net.grid_of(0).unwrap().with_channel.is_empty());
    }

    #[test]
    fn a_cable_carries_eight_channels_from_the_controller() {
        let mut net = Network::new();
        line(&mut net, &[NodeKind::CONTROLLER, NodeKind::CABLE]);
        for i in 0..10 {
            net.add_node(10 + i, NodeKind::device(1.0), [1, 1 + i as i32, 0]);
            net.connect(1, 10 + i);
        }
        let grid = net.grid_of(0).unwrap();
        assert_eq!(grid.controller_state, ControllerState::ControllerOnline);
        assert_eq!(grid.channels_in_use, 8);
        assert_eq!(grid.used_channels[&1], 8);
        // Dense cable carries 32.
        let mut dense = Network::new();
        line(&mut dense, &[NodeKind::CONTROLLER, NodeKind::DENSE_CABLE]);
        for i in 0..10 {
            dense.add_node(10 + i, NodeKind::device(1.0), [1, 1 + i as i32, 0]);
            dense.connect(1, 10 + i);
        }
        assert_eq!(dense.grid_of(0).unwrap().channels_in_use, 10);
    }

    #[test]
    fn controllers_must_touch_and_fit_in_seven() {
        let mut net = Network::new();
        line(&mut net, &[NodeKind::CONTROLLER, NodeKind::CABLE, NodeKind::CONTROLLER]);
        assert_eq!(net.grid_of(0).unwrap().controller_state, ControllerState::ControllerConflict);
        let mut long = Network::new();
        line(&mut long, &[NodeKind::CONTROLLER; 8]);
        assert_eq!(long.grid_of(0).unwrap().controller_state, ControllerState::ControllerConflict);
        let mut ok = Network::new();
        line(&mut ok, &[NodeKind::CONTROLLER; 7]);
        assert_eq!(ok.grid_of(0).unwrap().controller_state, ControllerState::ControllerOnline);
    }

    #[test]
    fn separate_pieces_are_separate_grids() {
        let mut net = Network::new();
        line(&mut net, &[NodeKind::CONTROLLER, NodeKind::CABLE, NodeKind::device(1.0)]);
        net.disconnect(1, 2);
        assert_eq!(net.grids().len(), 2);
        let alone = net.grid_of(2).unwrap();
        assert_eq!(alone.controller_state, ControllerState::NoController);
        // On its own, the device still gets an ad-hoc channel.
        assert!(alone.is_active(2, net.kinds()));
    }

    fn cell() -> BasicCellInventory {
        BasicCellInventory::new(CellType::item_cell(StorageTier::SIZE_1K), &[], FuzzyMode::IgnoreAll, CellUpgrades::default(), &CellContents::default())
    }

    #[test]
    fn storage_fills_by_priority_and_prefers_cells_that_hold_the_key() {
        let dirt = AeKey::item("minecraft:dirt", "", 0, 0);
        let sand = AeKey::item("minecraft:sand", "", 0, 0);
        let mut storage = NetworkStorage::new();
        storage.mount(1, 0, cell());
        storage.mount(2, 0, cell());
        storage.mount(3, 5, cell());
        // The highest priority first.
        assert_eq!(storage.insert(&dirt, 10, Actionable::Modulate), 10);
        assert_eq!(storage.get_mut(3).unwrap().amount_of(&dirt), 10);
        // Sand goes in at priority 5 too; once 3 is full, the cells holding nothing come next.
        assert_eq!(storage.insert(&sand, 10_000, Actionable::Modulate), 10_000);
        assert_eq!(storage.contents().get(&sand), 10_000);
        // Extracts take from the lowest priority first.
        assert_eq!(storage.extract(&sand, 100, Actionable::Modulate), 100);
        assert_eq!(storage.get_mut(3).unwrap().amount_of(&dirt), 10);
        assert_eq!(storage.contents().get(&sand), 9_900);
        assert!(storage.unmount(3).is_some());
        assert_eq!(storage.contents().get(&dirt), 0);
    }

    #[test]
    fn energy_runs_out() {
        let mut e = EnergyStore::new(100.0);
        assert_eq!(e.inject(150.0, Actionable::Modulate), 50.0);
        assert!(e.tick_idle(60.0));
        assert!(!e.tick_idle(60.0));
        assert!(!e.is_powered());
    }
}
