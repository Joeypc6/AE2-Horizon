// AE2's ME network in the game: the glue between Horizon's native hooks and the ported logic (port_ae2_core,
// port_ae2_grid). The server half gives AE2's devices their block entities, walks cables to form grids, keeps
// power, stores items in the cells of drives and ME chests (on the cell items themselves), answers terminal
// clicks and streams each open terminal's items to its player. The client half lays out and draws AE2's
// screens. Server and client run on different threads, each with its own thread-local state. No `//!`
// comments: module payloads can't hold inner attributes.

use super::port_ae2_core::{
    Actionable, AeKey, BasicCellInventory, CellContents, CellType, CellUpgrades, FuzzyMode, StorageTier, readable_number,
};
use super::port_ae2_crafting::{CalculationStrategy, Pattern, PatternBook, PatternInput, calculate};
use super::port_ae2_execution::{CpuEvent, CpuNetwork, CraftingCpu};
use super::port_ae2_grid::{ControllerState, Network, NetworkStorage, NodeKind};
use super::Game;
use horizon_client::containers::{ModBlockEntity, ModContainer};
use horizon_client::hud::HudBatch;
use horizon_client::items::ItemStack;
use horizon_world::nbt::{Compound, JavaString, Tag};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

pub const ID: &str = "ae2";
const NET_ITEMS: &str = "ae2:items";
/// The pattern access terminal's providers and their patterns.
const NET_PROVIDERS: &str = "ae2:providers";
/// The open screen's small state (crafting status, the encoder's ghosts and result, the assembler's grid), whole.
const NET_EXTRAS: &str = "ae2:extras";
/// Most messages one viewer is sent per stream (the rest wait for the next): a first list of a huge network goes
/// over a few seconds instead of in one burst.
const MESSAGES_PER_STREAM: usize = 8;
/// JEI's recipe transfer, client to server (`jei.rs`).
pub const NET_TRANSFER: &str = "ae2:transfer";

type Pos = [i32; 3];

/// Devices that get a block entity: everything that joins a network or holds items.
const DEVICES: &[&str] = &[
    "controller", "drive", "chest", "interface", "pattern_provider", "io_port", "molecular_assembler",
    "energy_acceptor", "energy_cell", "dense_energy_cell", "creative_energy_cell", "vibration_chamber",
    "crafting_unit", "crafting_accelerator", "crafting_monitor", "1k_crafting_storage", "4k_crafting_storage",
    "16k_crafting_storage", "64k_crafting_storage", "256k_crafting_storage", "wireless_access_point",
    "quantum_ring", "quantum_link", "spatial_io_port", "spatial_pylon", "spatial_anchor", "cell_workbench",
    "inscriber", "charger", "condenser", "crystal_resonance_generator", "terminal", "crafting_terminal",
    "pattern_encoding_terminal", "pattern_access_terminal", "level_emitter", "import_bus", "export_bus", "storage_bus", "growth_accelerator",
    "flawless_budding_quartz", "flawed_budding_quartz", "chipped_budding_quartz", "damaged_budding_quartz",
    "me_p2p_tunnel", "item_p2p_tunnel", "fluid_p2p_tunnel", "redstone_p2p_tunnel", "light_p2p_tunnel", "fe_p2p_tunnel",
    "annihilation_plane", "formation_plane", "energy_level_emitter", "storage_monitor", "conversion_monitor",
    "monitor", "semi_dark_monitor", "dark_monitor", "quartz_fiber", "toggle_bus", "inverted_toggle_bus", "light_detector",
];

/// `mymod:name` → `name`, for AE2's ids only.
fn ae2_name(id: &str) -> Option<&str> {
    // AE2's flat interface, pattern provider and energy acceptor (parts on a cable) work as the blocks do.
    id.strip_prefix("ae2:").map(|n| match n {
        "cable_interface" => "interface",
        "cable_pattern_provider" => "pattern_provider",
        "cable_energy_acceptor" => "energy_acceptor",
        // A powered illuminated panel is its lit twin (light 15).
        "hz_monitor_lit" => "monitor",
        "hz_semi_dark_monitor_lit" => "semi_dark_monitor",
        "hz_dark_monitor_lit" => "dark_monitor",
        // A cable with facades on it is that cable (its twin on a block-entity template).
        n => n.strip_prefix("hz_fcable_").unwrap_or(n),
    })
}

/// The block's id with the flat parts named as their blocks (see `ae2_name`).
fn canon(id: &str) -> String {
    ae2_name(id).map_or_else(|| id.to_owned(), |n| format!("ae2:{n}"))
}

fn is_device(block: &str) -> bool {
    ae2_name(block).is_some_and(|n| DEVICES.contains(&n))
}

/// Budding quartz ticks (it grows buds) but is no network device in AE2: no node, no channel, no Jade state.
fn is_budding(block: &str) -> bool {
    ae2_name(block).is_some_and(|n| n.ends_with("_budding_quartz"))
}

/// A cable's colour (`fluix` for AE2's transparent one) and whether it is dense.
fn cable(block: &str) -> Option<(&str, bool)> {
    let name = ae2_name(block)?;
    for suffix in ["_glass_cable", "_covered_cable", "_smart_cable", "_covered_dense_cable", "_smart_dense_cable"] {
        if let Some(color) = name.strip_suffix(suffix) {
            return Some((color, suffix.contains("dense")));
        }
    }
    None
}

/// Whether two neighbouring network blocks join (`AEColor.matches`: same colour, or either fluix).
fn joins(a: &str, b: &str) -> bool {
    match (cable(a), cable(b)) {
        (Some((ca, _)), Some((cb, _))) => ca == cb || ca == "fluix" || cb == "fluix",
        _ => true,
    }
}

fn node_kind(block: &str) -> NodeKind {
    if let Some((_, dense)) = cable(block) {
        return if dense { NodeKind::DENSE_CABLE } else { NodeKind::CABLE };
    }
    match ae2_name(block).unwrap_or("") {
        "controller" => NodeKind::CONTROLLER,
        "drive" => NodeKind::device(2.0),
        "chest" => NodeKind::device(1.0),
        "terminal" | "crafting_terminal" | "pattern_encoding_terminal" | "pattern_access_terminal" | "level_emitter" | "energy_level_emitter" => NodeKind::device(0.5),
        "storage_monitor" | "conversion_monitor" => NodeKind::device(0.5),
        "monitor" | "semi_dark_monitor" | "dark_monitor" => NodeKind::passive(1.0 / 16.0),
        "quartz_fiber" | "toggle_bus" | "inverted_toggle_bus" | "light_detector" => NodeKind::passive(0.0),
        "import_bus" | "export_bus" | "storage_bus" | "interface" | "pattern_provider" | "io_port" => NodeKind::device(1.0),
        "annihilation_plane" | "formation_plane" | "spatial_io_port" => NodeKind::device(1.0),
        n if n.ends_with("_p2p_tunnel") => NodeKind::device(1.0),
        "spatial_pylon" => NodeKind::device(0.5),
        "inscriber" => NodeKind::passive(1.0),
        "growth_accelerator" => NodeKind::passive(2.0),
        _ => NodeKind::passive(0.0),
    }
}

fn pack(p: Pos) -> u64 {
    ((p[0] as i64 as u64 & 0x3FF_FFFF) << 38) | ((p[2] as i64 as u64 & 0x3FF_FFFF) << 12) | (p[1] as i64 as u64 & 0xFFF)
}

const SIDES: [Pos; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];

fn add(p: Pos, d: Pos) -> Pos {
    [p[0] + d[0], p[1] + d[1], p[2] + d[2]]
}

// ---------------------------------------------------------------------------------------------------------
// Cells: what a storage cell holds rides on the cell item, in its custom data.

const CELL_DATA: &str = "minecraft:custom_data";
const CELL_KEY: &str = "ae2:cell";

fn cell_type(item: &str) -> Option<CellType> {
    let name = ae2_name(item)?;
    if name == "color_applicator" {
        return Some(CellType {
            key_type: super::port_ae2_core::KeyType::Items,
            total_bytes: 2048,
            bytes_per_type: 32,
            total_types: 27,
            idle_drain: 0.5,
            storable_in_storage_cell: true,
        });
    }
    if name == "matter_cannon" {
        return Some(CellType {
            key_type: super::port_ae2_core::KeyType::Items,
            total_bytes: 512,
            bytes_per_type: 8,
            total_types: 1,
            idle_drain: 0.5,
            storable_in_storage_cell: true,
        });
    }
    let (size, fluid) = match name.strip_prefix("item_storage_cell_") {
        Some(s) => (s, false),
        None => (name.strip_prefix("fluid_storage_cell_")?, true),
    };
    let tier = StorageTier::ALL.into_iter().find(|t| t.name_prefix == size)?;
    Some(if fluid { CellType::fluid_cell(tier) } else { CellType::item_cell(tier) })
}

/// An AE key as one string, saved on cells: id, damage, maximum damage and components.
fn key_to_string(key: &AeKey) -> String {
    // Fluids marked with `~` (a cell's keys are all of its kind, but a pattern's may be either).
    let mark = if key.key_type() == super::port_ae2_core::KeyType::Fluids { "~" } else { "" };
    format!("{mark}{}|{}|{}|{}", key.id(), key.fuzzy_search_value(), key.fuzzy_search_max_value(), key.components())
}

fn key_from_string(s: &str) -> Option<AeKey> {
    if let Some(fluid) = s.strip_prefix('~') {
        let mut parts = fluid.splitn(4, '|');
        let id = parts.next()?;
        return Some(AeKey::fluid(id, parts.nth(2).unwrap_or("")));
    }
    let mut parts = s.splitn(4, '|');
    let id = parts.next()?;
    let damage = parts.next()?.parse().ok()?;
    let max = parts.next()?.parse().ok()?;
    Some(AeKey::item(id, parts.next().unwrap_or(""), damage, max))
}

fn java(s: &str) -> JavaString {
    JavaString::from(s)
}

/// A cell's id and version as written on it (none on a cell the network hasn't stored in yet).
fn cell_id(stack: &ItemStack) -> Option<(i64, i64)> {
    let Some(Tag::Compound(data)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        return None;
    };
    match (data.get(&java("ae2:cell_id")), data.get(&java("ae2:cell_ver"))) {
        (Some(Tag::Long(id)), Some(Tag::Long(ver))) => Some((*id, *ver)),
        _ => None,
    }
}

/// Writes a cell's id, version and status (what a drive's light shows: 0 absent .. 4 full) on it.
fn set_cell_id(stack: &mut ItemStack, id: i64, ver: i64, status: i64) {
    let extra = stack.extra.get_or_insert_with(Default::default);
    let mut data = match extra.other.iter().position(|(k, _)| k == CELL_DATA) {
        Some(i) => match extra.other.remove(i).1 {
            Tag::Compound(c) => c,
            _ => Compound::new(),
        },
        None => Compound::new(),
    };
    data.insert(java("ae2:cell_id"), Tag::Long(id));
    data.insert(java("ae2:cell_ver"), Tag::Long(ver));
    data.insert(java("ae2:cell_status"), Tag::Long(status));
    extra.other.push((CELL_DATA.to_owned(), Tag::Compound(data)));
    extra.other.sort_by(|a, b| a.0.cmp(&b.0));
}

fn cell_status_number(inv: &BasicCellInventory) -> i64 {
    match inv.status() {
        super::port_ae2_core::CellState::Absent => 0,
        super::port_ae2_core::CellState::Empty => 1,
        super::port_ae2_core::CellState::NotEmpty => 2,
        super::port_ae2_core::CellState::TypesFull => 3,
        super::port_ae2_core::CellState::Full => 4,
    }
}

/// The status written on a cell when it was last saved, if any.
fn saved_cell_status(stack: &ItemStack) -> Option<i64> {
    let Some(Tag::Compound(data)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        return None;
    };
    match data.get(&java("ae2:cell_status")) {
        Some(Tag::Long(n)) => Some(*n),
        _ => None,
    }
}

fn read_cell(stack: &ItemStack) -> CellContents {
    let mut out = CellContents::default();
    let Some(Tag::Compound(data)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        return out;
    };
    let Some(Tag::Compound(cell)) = data.get(&java(CELL_KEY)) else { return out };
    if let (Some(Tag::List(keys)), Some(Tag::LongArray(amts))) = (cell.get(&java("k")), cell.get(&java("a"))) {
        for (k, a) in keys.iter().zip(amts) {
            if let Tag::String(js) = k
                && let Some(key) = js.to_string().ok().and_then(|s| key_from_string(&s))
            {
                out.keys.push(key);
                out.amounts.push(*a);
                out.item_count += *a;
            }
        }
    }
    out
}

fn write_cell(stack: &mut ItemStack, contents: &CellContents) {
    let extra = stack.extra.get_or_insert_with(Default::default);
    let mut data = match extra.other.iter().position(|(k, _)| k == CELL_DATA) {
        Some(i) => match extra.other.remove(i).1 {
            Tag::Compound(c) => c,
            _ => Compound::new(),
        },
        None => Compound::new(),
    };
    if contents.keys.is_empty() {
        data.remove(&java(CELL_KEY));
    } else {
        let mut cell = Compound::new();
        cell.insert(java("k"), Tag::List(contents.keys.iter().map(|k| Tag::String(java(&key_to_string(k)))).collect()));
        cell.insert(java("a"), Tag::LongArray(contents.amounts.clone()));
        data.insert(java(CELL_KEY), Tag::Compound(cell));
    }
    if !data.is_empty() {
        extra.other.push((CELL_DATA.to_owned(), Tag::Compound(data)));
        extra.other.sort_by(|a, b| a.0.cmp(&b.0));
    }
    if *extra.as_ref() == Default::default() {
        stack.extra = None;
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// The AE key of a stack: its id and damage, and its other components as their NBT when it has any.
fn key_of(game: &Game<'_>, stack: &ItemStack) -> AeKey {
    let max = game.res.items().get(&stack.item).map_or(0, |i| i.max_damage) as i32;
    let components = if stack.extra.is_some() {
        let mut one = stack.clone();
        one.count = 1;
        one.damage = 0;
        horizon_world::nbt::write_network(&horizon_client::save::item_to_nbt(&one), Default::default())
            .map(|b| hex(&b))
            .unwrap_or_default()
    } else {
        String::new()
    };
    AeKey::item(&stack.item, &components, stack.damage as i32, max)
}

fn stack_of(key: &AeKey, count: u32) -> ItemStack {
    let mut stack = if key.components().is_empty() {
        ItemStack::new(key.id(), count)
    } else {
        unhex(key.components())
            .and_then(|b| horizon_world::nbt::read_network(&b, Default::default()).ok())
            .and_then(|(tag, _)| horizon_client::save::item_from_nbt(&tag))
            .unwrap_or_else(|| ItemStack::new(key.id(), count))
    };
    stack.count = count;
    stack.damage = key.fuzzy_search_value().max(0) as u32;
    stack
}

fn max_stack(game: &Game<'_>, id: &str) -> u32 {
    game.res.items().get(id).map_or(64, |i| i.max_stack.max(1))
}

// ---------------------------------------------------------------------------------------------------------
// Server state.

/// One grid as the server last worked it out.
#[derive(Default)]
struct GridInfo {
    nodes: HashSet<Pos>,
    online: bool,
    /// The controllers break AE2's rules (more than one structure, or past 7 on an axis).
    conflict: bool,
    /// No controller and more than 8 devices want a channel (AdHocNetworkError.TOO_MANY_CHANNELS).
    too_many: bool,
    /// The channels each block carries (AE2's pathing: what a smart cable lights up).
    cable_channels: HashMap<Pos, u32>,
    /// Devices that work: they have a channel, or need none.
    active: HashSet<Pos>,
    /// Drives and ME chests with their cell slots, in that order.
    storage: Vec<Pos>,
    idle: f64,
    channels: u32,
    /// Pattern providers' patterns: the provider and the pattern.
    patterns: Vec<(Pos, Arc<Pattern>)>,
    /// The crafting CPUs its formed clusters make: each one's key (lowest storage block), bytes, co-processors.
    cpus: Vec<(Pos, i64, i32)>,
    /// Each formed crafting block's CPU key.
    cpu_of: HashMap<Pos, Pos>,
    /// Storage buses and the container each faces.
    storage_buses: Vec<(Pos, Pos)>,
    /// Import and export buses and the container each faces.
    io_buses: Vec<(Pos, Pos, bool)>,
    /// Formation planes and the block each places into.
    formation_planes: Vec<(Pos, Pos)>,
}

/// The pattern access terminal's list: each provider, as its viewer was sent them (for clicks).
#[derive(Default)]
struct ProviderList {
    hash: u64,
    providers: Vec<Pos>,
}

/// What a terminal's viewer has been sent (AE2's `IncrementalUpdateHelper`): each item it knows by a stable
/// index (a removed one is -1 until the index is reused), the storage generation it saw, the hash of the small
/// extras (crafting status, the encoder's grid), and messages not sent yet.
#[derive(Default)]
struct Sent {
    keys: Vec<(AeKey, i64)>,
    index: HashMap<AeKey, usize>,
    free: Vec<usize>,
    generation: u64,
    extras: u64,
    pending: VecDeque<Vec<u8>>,
    full_after: u64,
}

#[derive(Default)]
struct Server {
    tick: u64,
    /// Stacks a terminal took out for a player's inventory (a menu button has no player to give to): given in
    /// that player's inventory tick, what doesn't fit going back to the network.
    pending_gives: Vec<(u128, usize, ItemStack)>,
    /// Cables with facades saved by an older build (`ae2:hz_fcable_<cable>`), to become the shaped twins.
    legacy_twins: Vec<(Pos, Compound)>,
    /// Toggle buses that join their front now (redstone on, or off for the inverted one).
    toggles_on: HashSet<Pos>,
    /// A wireless crafting terminal's open screen: the inventory slot the terminal (and its grid) is in, the cursor
    /// stack, and the clicks waiting for the player's inventory tick.
    wcraft_slot: HashMap<u128, i32>,
    wcursor: HashMap<u128, ItemStack>,
    wcraft_ops: Vec<(u128, String)>,
    /// Tiny TNT to light this tick, and what is lit (TinyTNTPrimedEntity).
    tnt_primes: Vec<Pos>,
    tiny_tnts: Vec<super::tnt::PrimedTnt>,
    /// Each grid's current passive generator (EnergyService's overlay grid `currentPassiveGenerator`).
    crg_current: HashMap<usize, Pos>,
    /// Light detectors' signals (LightDetectorBlock.getSignal: the light less 6), by position.
    light_signals: HashMap<Pos, i32>,
    /// Block states a block entity's tick wants (it is out of the world then): set at the end of the tick.
    prop_sets: Vec<(Pos, &'static str, &'static str)>,
    /// Each player's last right-clicked block and face (`native_right_click_block`), for what they place next.
    last_click: HashMap<u128, (Pos, Pos)>,
    /// The enchantments on what they held then (an annihilation plane keeps them).
    last_enchants: HashMap<u128, Vec<(Arc<str>, u32)>>,
    /// Planes just placed and the enchantments they get on their first tick.
    plane_enchants: HashMap<Pos, Vec<(Arc<str>, u32)>>,
    /// Device block entities seen ticking: position and block, and when.
    devices: HashMap<Pos, (String, u64)>,
    grids: Vec<GridInfo>,
    grid_of: HashMap<Pos, usize>,
    /// Players with a terminal open, by the terminal they used.
    viewers: HashMap<u128, Pos>,
    /// What each viewer was last sent, in order, with amounts (0: craftable only): their clicks index into it.
    sent: HashMap<u128, Sent>,
    /// What each pattern access terminal's viewer was sent.
    provider_lists: HashMap<u128, ProviderList>,
    /// Each grid's storage generation: bumped whenever a cell or a storage bus's container changes, so a terminal
    /// with nothing new to show sends nothing.
    storage_gen: HashMap<usize, u64>,
    /// AE each grid had at the last power pass, by its lowest position.
    energy: HashMap<Pos, f64>,
    /// Drives whose cells changed look: their block state is flipped so clients get the update tag.
    flips: Vec<Pos>,
    /// The `powered` state last set on blocks that show it.
    powered: HashMap<Pos, bool>,
    /// Crafting CPUs, by their key position.
    cpus: HashMap<Pos, CraftingCpu>,
    /// Crafts a molecular assembler is making: when, the CPU, what comes out.
    assembling: Vec<(u64, Pos, Vec<(AeKey, i64)>)>,
    /// Storage cells as the network uses them (AE2 keeps a mounted cell's inventory live): by the id written on the
    /// cell, its version (bumped each save, so an edited or copied cell is read again), the inventory, and the tick
    /// it was last used (cells out of every drive for a while are let go).
    cells: HashMap<i64, (i64, Box<dyn super::port_ae2_grid::MeStorage>, u64)>,
    /// Level emitters giving a signal now, and the changes still to show and pass on.
    lit_emitters: HashSet<Pos>,
    /// P2P tunnels as they last ticked: frequency, whether an output, kind, tick.
    p2p: HashMap<Pos, (i64, bool, String, u64)>,
    /// Settings a tunnel gets once its new block entity ticks (after it was turned into another kind).
    p2p_pending: HashMap<Pos, (i64, bool)>,
    /// Redstone P2P outputs' signals, and blocks whose neighbours must read signals again.
    p2p_signal: HashMap<Pos, i32>,
    signal_updates: Vec<Pos>,
    /// Players using a wireless terminal, and the inventory slots they asked to put in.
    wireless: HashSet<u128>,
    /// Players on an ME chest's own terminal (its cell, not the network): the wireless screen over the chest.
    chest_terms: HashSet<u128>,
    wireless_inserts: Vec<(u128, i32)>,
    emitter_changes: Vec<(Pos, bool)>,
    next_cell_id: i64,
    /// Each grid's storage, mounted (by its first drive's position).
    mounted: HashMap<Pos, MountedStorage>,
    /// Each machine a processing pattern uses: when something last came out, and its ticks an output as measured.
    machine_times: HashMap<Pos, (u64, f64)>,
    /// What each CPU's requested job makes, for the toast when it's done.
    request_items: HashMap<Pos, (String, i64)>,
    /// Who turned off Notify about finished crafting jobs.
    no_notify: HashSet<u128>,
    /// The toolbelt each viewer's client has, what its clicks have yet to write on the tool, and the tick each was
    /// last looked at.
    toolbelt_sent: HashMap<u128, String>,
    toolbelt_writes: HashMap<u128, String>,
    toolbelt_flush: HashSet<u128>,
    toolbelt_seen: HashMap<u128, u64>,
    /// The "+" on a crafting terminal: a player, the terminal, the grid cell and what may fill it from their inventory.
    terminal_fills: Vec<(u128, Pos, usize, Vec<String>)>,
    /// Each molecular assembler's cycle: its cards, its last push and its ticks a craft as measured.
    assembler_cycles: HashMap<Pos, (usize, u64, f64)>,
    /// Each busy CPU's last forecast: at which tick, the ticks left then, the ticks a second.
    forecasts: HashMap<Pos, (u64, Option<f64>, f64)>,
    /// The tick rate's last measure: tick, time, ticks a second.
    tick_rate: Option<(u64, std::time::Instant, f64)>,
    /// What each molecular assembler makes now: from and until which tick, the item, and the ingredients in its
    /// grid (id and amount, in the pattern's order).
    assembler_jobs: HashMap<Pos, (u64, u64, String, Vec<(String, i64)>)>,
    /// Machines a pattern provider fed: the machine, the CPU, what it still owes.
    awaiting: Vec<(Pos, Pos, Vec<(AeKey, i64)>)>,
    /// Who asked each CPU's job, to tell them when it ends.
    requesters: HashMap<Pos, u128>,
    /// Item screens (portable cells, the network tool, the knife): which each player has open, their clicks (done
    /// in the player's inventory tick, where the held item can change), stacks on their way into the held item,
    /// and what the screen was last sent (keys by index, and a hash).
    item_screens: HashMap<u128, String>,
    item_ops: Vec<(u128, String)>,
    item_carry: HashMap<u128, Vec<ItemStack>>,
    portable_sent: HashMap<u128, (Vec<AeKey>, u64)>,
}

/// Blocks with a `powered` state (controller, terminals): lit on a working network.
fn shows_power(block: &str) -> bool {
    // (The level emitters' `powered` is their redstone signal.)
    (is_part(block) && !block.ends_with("level_emitter"))
        || matches!(
            ae2_name(block),
            Some("controller" | "chest" | "growth_accelerator" | "molecular_assembler" | "crafting_monitor" | "crafting_accelerator" | "1k_crafting_storage" | "4k_crafting_storage"
                | "16k_crafting_storage" | "64k_crafting_storage" | "256k_crafting_storage")
        )
}

thread_local! {
    static SERVER: RefCell<Server> = RefCell::new(Server::default());
    static CLIENT: RefCell<Client> = RefCell::new(Client::default());
}

/// `native_block_entity`: AE2's devices get a block entity of their own id.
/// AE2's sky stone chests: an inventory each, outside any network.
fn is_sky_chest(block: &str) -> bool {
    matches!(ae2_name(block), Some("sky_stone_chest" | "smooth_sky_stone_chest" | "crank"))
}

pub fn block_entity(block: &str, entity: &mut Option<(String, Compound)>) {
    if entity.is_none() && (is_device(block) || is_sky_chest(block) || block == "ae2:sky_stone_tank" || block == "ae2:tiny_tnt" || is_twin(block) || is_legacy_twin(block) || is_paint(block)) {
        *entity = Some((block.to_owned(), Compound::new()));
    }
}

pub fn block_entity_ticks(block: &str, ticks: &mut bool) {
    // (The tank ticks for nothing: Horizon keeps a mod block entity for a block in its ticking table.)
    if is_device(block) || block == "ae2:sky_stone_tank" || block == "ae2:tiny_tnt" || is_twin(block) || is_legacy_twin(block) || is_paint(block) {
        *ticks = true;
    }
}

/// AE2's upgrade slots by machine (`getUpgradeSlots`): after the machine's own slots.
fn upgrade_slots(block: &str) -> usize {
    match ae2_name(block) {
        Some("io_port") => 3,
        Some("inscriber") => 4,
        Some("molecular_assembler") => 5,
        Some("import_bus" | "export_bus") => 5,
        Some("vibration_chamber") => 3,
        Some("storage_bus" | "formation_plane") => 5,
        Some("interface" | "level_emitter") => 1,
        _ => 0,
    }
}

/// The machine's own slots before its upgrade slots.
fn own_slots(block: &str) -> usize {
    match ae2_name(block) {
        Some("inscriber") => 4,
        Some("molecular_assembler") => 1,
        Some("import_bus" | "export_bus" | "storage_bus") => 0,
        Some("io_port") => 12,
        Some("vibration_chamber") => 1,
        Some("interface") => 9,
        Some("level_emitter") => 1,
        _ => 0,
    }
}

/// AE2's upgrade inventory: one card a slot, cards only. Extra cards go to empty upgrade slots; what's left
/// over, or isn't a card, goes back to the nearest player.
fn tidy_upgrades(game: &mut Game<'_>, pos: Pos, entity: &mut ModBlockEntity) {
    let n = upgrade_slots(&entity.id);
    if n == 0 {
        return;
    }
    let own = own_slots(&entity.id);
    if entity.items.len() < own + n {
        entity.items.resize(own + n, None);
    }
    let mut spare: Vec<ItemStack> = Vec::new();
    for i in own..own + n {
        let Some(st) = entity.items[i].as_mut() else { continue };
        let is_card = ae2_name(&st.item).is_some_and(|c| c.ends_with("_card") && c != "memory_card");
        if !is_card {
            spare.push(entity.items[i].take().unwrap());
        } else if st.count > 1 {
            let mut rest = st.clone();
            rest.count -= 1;
            st.count = 1;
            spare.push(rest);
        }
    }
    if spare.is_empty() {
        return;
    }
    let mut back = Vec::new();
    for mut st in spare {
        let is_card = ae2_name(&st.item).is_some_and(|c| c.ends_with("_card") && c != "memory_card");
        while is_card && st.count > 0 {
            let Some(free) = (own..own + n).find(|&i| entity.items[i].is_none()) else { break };
            let mut one = st.clone();
            one.count = 1;
            entity.items[free] = Some(one);
            st.count -= 1;
        }
        if st.count > 0 {
            back.push(st);
        }
    }
    let at = [pos[0] as f64 + 0.5, pos[1] as f64 + 0.5, pos[2] as f64 + 0.5];
    let nearest = game
        .native_server_players()
        .into_iter()
        .min_by(|a, b| {
            let d = |p: [f64; 3]| (p[0] - at[0]).powi(2) + (p[1] - at[1]).powi(2) + (p[2] - at[2]).powi(2);
            d(a.position).total_cmp(&d(b.position))
        })
        .map(|p| p.uuid);
    for st in back {
        match nearest {
            Some(uuid) => {
                let _ = game.native_give_stack(uuid, st);
            }
            None => {
                if let Some(free) = (own..own + n).find(|&i| entity.items[i].is_none()) {
                    entity.items[free] = Some(st);
                }
            }
        }
    }
}

/// How many of `card` sit in a machine's upgrade slots (those after its first `own` slots).
fn cards(entity: &ModBlockEntity, own: usize, card: &str) -> usize {
    entity.items.iter().skip(own).flatten().filter(|s| &*s.item == card).map(|s| s.count as usize).sum()
}

fn container(slots: usize, menu: Option<&str>, hoppers: bool) -> ModContainer {
    let all: Vec<usize> = if hoppers { (0..slots).collect() } else { Vec::new() };
    ModContainer {
        slots,
        faces: std::array::from_fn(|_| all.clone()),
        inputs: all.clone(),
        outputs: all,
        menu: menu.map(str::to_owned),
    }
}

/// `native_block_entity_container`: the drive's ten cells, the ME chest's one, a terminal's insert slot and the
/// vibration chamber's fuel.
pub fn block_entity_container(block: &str, out: &mut Option<ModContainer>) {
    *out = match ae2_name(block) {
        Some("drive") => Some(container(10, Some("ae2:drive"), false)),
        Some("controller") => Some(container(1, Some("ae2:controller"), false)),
        // AE2's interface: its storage slots, open to hoppers and pipes (what it stocks, others can take).
        // (Hoppers reach the nine storage slots, not the card.)
        Some("interface") => {
            let mut c = container(9, Some("ae2:interface"), true);
            c.slots = 10;
            Some(c)
        }
        Some("level_emitter") => Some(container(2, Some("ae2:level_emitter"), false)),
        Some("energy_level_emitter") => Some(container(1, Some("ae2:energy_level_emitter"), false)),
        Some("cell_workbench") => Some(container(1 + WB_UPGRADES, Some("ae2:cell_workbench"), false)),
        Some("quantum_link") => Some(container(1, Some("ae2:quantum_link"), true)),
        // AE2's condenser: anything goes in (and is gone), the output comes out; the storage component stays.
        Some("condenser") => Some(ModContainer {
            slots: 3,
            faces: std::array::from_fn(|_| vec![0, 1]),
            inputs: vec![0],
            outputs: vec![1],
            menu: Some("ae2:condenser".to_owned()),
        }),
        // The item P2P tunnel's input: what hoppers and pipes put in goes out of its outputs.
        Some(n) if is_info_block(n) => Some(container(1, Some("ae2:info"), n == "item_p2p_tunnel")),
        Some("spatial_io_port") => Some(ModContainer {
            slots: 2,
            faces: std::array::from_fn(|_| vec![0, 1]),
            inputs: vec![0],
            outputs: vec![1],
            menu: Some("ae2:spatial_io_port".to_owned()),
        }),
        Some("wireless_access_point") => Some(container(3, Some("ae2:wireless_access_point"), false)),
        Some("io_port") => Some(ModContainer {
            slots: 12 + upgrade_slots(block),
            faces: std::array::from_fn(|_| (0..12).collect()),
            inputs: (0..6).collect(),
            outputs: (6..12).collect(),
            menu: Some("ae2:io_port".to_owned()),
        }),
        Some("pattern_access_terminal") => Some(container(1, Some("ae2:pattern_access_terminal"), false)),
        Some("sky_stone_chest" | "smooth_sky_stone_chest") => Some(container(36, Some("ae2:sky_chest"), true)),
        Some("molecular_assembler") => Some(container(1 + upgrade_slots(block), Some("ae2:molecular_assembler"), false)),
        Some(_) if is_crafting(block) => Some(container(1, Some("ae2:crafting_cpu"), false)),
        Some("chest") => Some(container(1, Some("ae2:chest"), false)),
        Some("terminal") => Some(container(1 + VIEW_CELLS, Some("ae2:terminal"), false)),
        // Slot 0 takes shift-clicks into the network (as the terminal's one slot), then the grid, then the result.
        Some("crafting_terminal") => Some(container(11 + VIEW_CELLS, Some("ae2:crafting_terminal"), false)),
        Some("pattern_encoding_terminal") => Some(container(2 + VIEW_CELLS, Some("ae2:pattern_encoding_terminal"), false)),
        // AE2's pattern provider: nine patterns, then the return inventory, which anything beside it may fill.
        Some("pattern_provider") => Some(ModContainer {
            slots: 18,
            faces: std::array::from_fn(|_| (9..18).collect()),
            inputs: (9..18).collect(),
            outputs: Vec::new(),
            menu: Some("ae2:pattern_provider".to_owned()),
        }),
        Some("import_bus" | "export_bus") => Some(container(upgrade_slots(block), Some("ae2:io_bus"), false)),
        Some("storage_bus" | "formation_plane") => Some(container(upgrade_slots(block), Some("ae2:storage_bus"), false)),
        // (Hoppers feed the fuel slot, not the cards.)
        Some("vibration_chamber") => {
            let mut c = container(1, Some("ae2:vibration_chamber"), true);
            c.slots = 4;
            Some(c)
        }
        // AE2's charger has no screen: items go in by hand or by hopper.
        Some("charger") => Some(container(1, None, true)),
        // The inscriber: top press, middle, bottom press, output; hoppers fill the three and take the output.
        Some("inscriber") => Some(ModContainer {
            slots: 4 + upgrade_slots(block),
            faces: std::array::from_fn(|_| vec![0, 1, 2, 3]),
            inputs: vec![0, 1, 2],
            outputs: vec![3],
            menu: Some("ae2:inscriber".to_owned()),
        }),
        _ => return,
    };
}

/// `native_block_entity_tick`: devices report in; terminals show whether they're online, take what's put in
/// their slot and hand it to the network; vibration chambers burn fuel.
pub fn block_entity_tick(game: &mut Game<'_>, pos: Pos, entity: &mut ModBlockEntity) {
    if is_legacy_twin(&entity.id) {
        let nbt = entity.nbt.clone();
        SERVER.with_borrow_mut(|s| {
            if !s.legacy_twins.iter().any(|(p, _)| *p == pos) {
                s.legacy_twins.push((pos, nbt));
            }
        });
        return;
    }
    if entity.id == "ae2:tiny_tnt" {
        let dim = game.dim.id().to_owned();
        if game.native_neighbor_signal(&dim, pos) > 0 {
            SERVER.with_borrow_mut(|s| s.tnt_primes.push(pos));
        }
        return;
    }
    if !is_device(&entity.id) {
        return;
    }
    once(&format!("block entity ticks: {}", entity.id));
    let (tick, grid) = SERVER.with_borrow_mut(|s| {
        if !is_budding(&entity.id) {
            s.devices.insert(pos, (entity.id.clone(), s.tick));
        }
        (s.tick, s.grid_of.get(&pos).copied())
    });
    let active = grid.is_some_and(|g| SERVER.with_borrow(|s| s.grids.get(g).is_some_and(|g| g.online && g.active.contains(&pos))));
    tidy_upgrades(game, pos, entity);
    match ae2_name(&entity.id).unwrap_or("") {
        "terminal" => {
            entity.data.resize(4, 0);
            entity.data[0] = i32::from(active);
            if active
                && let Some(g) = grid
                && let Some(stack) = entity.items.first_mut().and_then(|s| s.take())
            {
                let key = key_of(game, &stack);
                let stored = with_storage(game, g, None, |st| st.insert(&key, i64::from(stack.count), Actionable::Modulate));
                let left = stack.count.saturating_sub(stored as u32);
                if left > 0 {
                    let mut back = stack;
                    back.count = left;
                    entity.items[0] = Some(back);
                }
            }
        }
        "crafting_terminal" => {
            entity.data.resize(4, 0);
            entity.data[0] = i32::from(active);
            if active
                && let Some(g) = grid
                && let Some(stack) = entity.items.first_mut().and_then(|s| s.take())
            {
                let key = key_of(game, &stack);
                let stored = with_storage(game, g, None, |st| st.insert(&key, i64::from(stack.count), Actionable::Modulate));
                let left = stack.count.saturating_sub(stored as u32);
                if left > 0 {
                    let mut back = stack;
                    back.count = left;
                    entity.items[0] = Some(back);
                }
            }
            crafting_terminal_tick(game, grid.filter(|_| active), entity);
        }
        "pattern_access_terminal" => {
            entity.data.resize(4, 0);
            entity.data[0] = i32::from(active);
            entity.data[1] = nbt_i64(&entity.nbt, "show") as i32;
        }
        n if n.ends_with("_p2p_tunnel") => {
            let kind = entity.id.clone();
            entity.data.resize(1, 0);
            entity.data[0] = i32::from(active);
            if let Some((freq, output)) = SERVER.with_borrow_mut(|s| s.p2p_pending.remove(&pos)) {
                set_nbt_i64(&mut entity.nbt, "freq", freq);
                set_nbt_i64(&mut entity.nbt, "output", i64::from(output));
            }
            let freq = nbt_i64(&entity.nbt, "freq");
            let output = nbt_i64(&entity.nbt, "output") != 0;
            SERVER.with_borrow_mut(|s| {
                s.p2p.insert(pos, (freq, output, kind.clone(), tick));
            });
            if freq != 0 && !output {
                p2p_input_tick(game, pos, &kind, freq, tick, entity);
            }
        }
        "annihilation_plane" => {
            if let Some(levels) = SERVER.with_borrow_mut(|s| s.plane_enchants.remove(&pos)) {
                let mut c = Compound::new();
                for (id, level) in levels {
                    c.insert(java(&id), Tag::Int(level as i32));
                }
                entity.nbt.insert(java("enchantments"), Tag::Compound(c));
            }
            entity.data.resize(1, 0);
            entity.data[0] = i32::from(active);
            if active && tick % 4 == 0 && let Some(g) = grid {
                annihilate(game, g, pos);
            }
        }
        "spatial_io_port" => {
            entity.data.resize(4, 0);
            entity.items.resize(2, None);
            entity.data[0] = i32::from(active);
            if tick % 20 == 0 {
                let size = grid.and_then(|g| spatial_region(game, g)).map_or([0; 3], |(lo, hi)| [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1]);
                entity.data[1..4].copy_from_slice(&size);
            }
            let dim = game.dim.id().to_owned();
            let signal = game.native_neighbor_signal(&dim, pos) > 0;
            let was = nbt_i64(&entity.nbt, "rs") != 0;
            if signal != was {
                set_nbt_i64(&mut entity.nbt, "rs", i64::from(signal));
            }
            if signal && !was && active && let Some(g) = grid {
                spatial_swap(game, g, pos, entity);
            }
        }
        "wireless_access_point" => {
            // AE2's range: 16 blocks and the boosters' (to the power 1.5); a terminal put in is linked here.
            entity.data.resize(2, 0);
            entity.items.resize(3, None);
            entity.data[0] = i32::from(active);
            entity.data[1] = wap_range(entity) as i32;
            // AE2's look: the pearl lights while powered, the status light shows a channel.
            let powered = grid.is_some_and(|g| SERVER.with_borrow(|s| s.grids.get(g).is_some_and(|g| g.online)));
            let look = if active { "has_channel" } else if powered { "on" } else { "off" };
            if game.native_block_property(pos, "state").is_some_and(|v| v != look) {
                game.native_set_block_property_with(pos, "state", look, 2);
            }
            let linkable = entity.items[1].as_ref().is_some_and(|s| is_wireless_terminal(&s.item));
            if linkable && entity.items[2].is_none() {
                let mut t = entity.items[1].take().unwrap();
                set_link(&mut t, pos);
                entity.items[2] = Some(t);
            }
        }
        "level_emitter" => {
            entity.data.resize(5, 0);
            entity.data[0] = i32::from(active);
            if tick % 10 == 0 {
                let level = nbt_i64(&entity.nbt, "level");
                let config = ghosts_of(entity).into_iter().next().flatten();
                let on = active
                    && match (grid, config) {
                        (Some(g), Some((id, _))) => {
                            let key = ghost_key(game, &id);
                            // StorageLevelEmitterPart.updateReportingValue: with a Fuzzy Card, all that matches.
                            let fuzzy = (cards(entity, 1, "ae2:fuzzy_card") > 0).then(|| fuzzy_mode_of(entity));
                            let held = match fuzzy {
                                Some(mode) => with_storage(game, g, None, |st| st.contents().find_fuzzy(&key, mode)).iter().map(|(_, n)| *n).sum(),
                                None => with_storage(game, g, None, |st| st.contents().get(&key)),
                            };
                            held >= level.max(1)
                        }
                        _ => false,
                    };
                // AE2's redstone mode: emit at or above the level, or below it.
                let on = if nbt_i64(&entity.nbt, "below") != 0 { active && !on } else { on };
                // With a Crafting Card: on while a CPU waits for the item (any item with none configured).
                let carded = cards(entity, 1, "ae2:crafting_card") > 0;
                let on = if carded {
                    let wanted = ghosts_of(entity).into_iter().next().flatten().map(|(id, _)| ghost_key(game, &id));
                    active
                        && SERVER.with_borrow(|s| {
                            grid.and_then(|g| s.grids.get(g)).is_some_and(|g| {
                                g.cpus.iter().any(|(c, _, _)| {
                                    s.cpus.get(c).is_some_and(|cpu| match &wanted {
                                        Some(key) => cpu.waiting_for(key) > 0,
                                        None => cpu.has_job(),
                                    })
                                })
                            })
                        })
                } else {
                    on
                };
                entity.data[2] = i32::from(carded);
                entity.data[4] = if cards(entity, 1, "ae2:fuzzy_card") > 0 { nbt_i64(&entity.nbt, "fuzzy") as i32 } else { -1 };
                entity.data[3] = nbt_i64(&entity.nbt, "craft_redstone") as i32;
                entity.data[1] = i32::from(on);
                // Lit as the block shows it too (a block placed again where a lit one stood starts unlit).
                let was = SERVER.with_borrow(|s| s.lit_emitters.contains(&pos)) && game.native_block_property(pos, "powered").as_deref() == Some("true");
                if on != was {
                    SERVER.with_borrow_mut(|s| {
                        if on {
                            s.lit_emitters.insert(pos);
                        } else {
                            s.lit_emitters.remove(&pos);
                        }
                        s.emitter_changes.push((pos, on));
                    });
                }
            }
        }
        "interface" => {
            entity.data.resize(2, 0);
            entity.data[0] = i32::from(active);
            entity.data[1] = if cards(entity, 9, "ae2:fuzzy_card") > 0 { nbt_i64(&entity.nbt, "fuzzy") as i32 } else { -1 };
            if active && let Some(g) = grid {
                interface_tick(game, g, entity);
            }
        }
        "energy_level_emitter" => {
            // AE2's energy level emitter: a signal while the network stores at least this much AE.
            entity.data.resize(2, 0);
            entity.data[0] = i32::from(active);
            if tick % 10 == 0 {
                let level = nbt_i64(&entity.nbt, "level");
                let stored = SERVER.with_borrow(|s| {
                    grid.and_then(|g| s.grids.get(g)).and_then(|g| g.nodes.iter().min().copied()).and_then(|r| s.energy.get(&r).copied()).unwrap_or(0.0)
                });
                let on = active && stored >= level.max(1) as f64;
                // AE2's redstone mode: emit at or above the level, or below it.
                let on = if nbt_i64(&entity.nbt, "below") != 0 { active && !on } else { on };
                entity.data[1] = i32::from(on);
                // Lit as the block shows it too (a block placed again where a lit one stood starts unlit).
                let was = SERVER.with_borrow(|s| s.lit_emitters.contains(&pos)) && game.native_block_property(pos, "powered").as_deref() == Some("true");
                if on != was {
                    SERVER.with_borrow_mut(|s| {
                        if on {
                            s.lit_emitters.insert(pos);
                        } else {
                            s.lit_emitters.remove(&pos);
                        }
                        s.emitter_changes.push((pos, on));
                    });
                }
            }
        }
        "condenser" => condenser_tick(entity),
        "import_bus" | "export_bus" | "storage_bus" | "formation_plane" => bus_tick(game, pos, entity),
        "controller" if tick % 20 == 0 => {
            controller_look(game, pos, grid);
            controller_data(grid, pos, entity);
        }
        "energy_cell" | "dense_energy_cell" if tick % 20 == 0 => {
            // AE2's charge display: the network's stored power over what its cells and controllers hold.
            let ratio = SERVER.with_borrow(|s| {
                let g = grid.and_then(|g| s.grids.get(g))?;
                let root = g.nodes.iter().min().copied()?;
                Some(s.energy.get(&root).copied().unwrap_or(0.0) / grid_capacity(g, &s.devices).max(1.0))
            });
            let stage = match ratio.unwrap_or(0.0) {
                r if r < 0.01 => 0,
                r => ((r * 4.0).ceil() as i32).clamp(1, 4),
            };
            let stage = stage.to_string();
            if game.native_block_property(pos, "stage").is_some_and(|v| v != stage) {
                game.native_set_block_property_with(pos, "stage", &stage, 2);
            }
        }
        "spatial_pylon" if tick % 20 == 0 => {
            let online = SERVER.with_borrow(|s| grid.and_then(|g| s.grids.get(g)).is_some_and(|g| g.online));
            pylon_look(game, pos, online);
        }
        "quantum_ring" if tick % 20 == 0 => {
            let powered = SERVER.with_borrow(|s| grid.and_then(|g| s.grids.get(g)).is_some_and(|g| g.online));
            qnb_look(game, pos, powered);
        }
        "quantum_link" => {
            entity.data.resize(2, 0);
            entity.items.resize(1, None);
            if let Some(s) = entity.items[0].as_mut() {
                give_frequency(s);
            }
            if tick % 20 == 0 {
                entity.data[0] = i32::from(qnb_formed(game, pos));
                let powered = SERVER.with_borrow(|s| grid.and_then(|g| s.grids.get(g)).is_some_and(|g| g.online));
                qnb_look(game, pos, powered);
            }
            entity.data[1] = i32::from(entity.items[0].as_ref().and_then(singularity_freq).is_some());
        }
        "spatial_anchor" => {
            entity.data.resize(1, 0);
            entity.data[0] = i32::from(active);
        }
        "io_port" => {
            entity.data.resize(4, 0);
            entity.data[0] = i32::from(active);
            entity.data[1] = nbt_i64(&entity.nbt, "mode") as i32;
            entity.data[2] = nbt_i64(&entity.nbt, "full") as i32;
            entity.data[3] = if cards(entity, 12, "ae2:redstone_card") > 0 { nbt_i64(&entity.nbt, "redstone") as i32 } else { -1 };
            if active && let Some(g) = grid {
                io_port_tick(game, g, pos, entity);
            }
        }
        "pattern_encoding_terminal" => {
            entity.data.resize(4, 0);
            entity.data[0] = i32::from(active);
            entity.data[1] = nbt_i64(&entity.nbt, "mode") as i32;
            entity.data[2] = nbt_i64(&entity.nbt, "substitute") as i32;
        }
        "drive" | "chest" => {
            // Clients draw the cells from the update tag, sent when the block changes: flip the barrel's
            // `open` when the cells' looks change.
            let looks = cell_looks(&entity.items);
            let sig = looks.join(",");
            let old = match entity.nbt.get(&java("looks")) {
                Some(Tag::String(s)) => s.to_string().unwrap_or_default(),
                _ => String::new(),
            };
            if sig != old {
                entity.nbt.insert(java("looks"), Tag::String(java(&sig)));
                // Not from inside the block entity's own tick: the server tick flips it.
                SERVER.with_borrow_mut(|s| s.flips.push(pos));
            }
        }
        "charger" => {
            let holds = entity.items.first().and_then(|s| s.as_ref()).map_or(String::new(), |s| s.item.to_string());
            let old = match entity.nbt.get(&java("holds")) {
                Some(Tag::String(s)) => s.to_string().unwrap_or_default(),
                _ => String::new(),
            };
            if holds != old {
                entity.nbt.insert(java("holds"), Tag::String(java(&holds)));
                SERVER.with_borrow_mut(|s| s.flips.push(pos));
            }
            // ChargerBlockEntity: a powered item takes its charge rate a tick from the network until full.
            if let Some(Some(stack)) = entity.items.first_mut()
                && let Some((most, rate)) = item_battery(&stack.item)
            {
                let have = portable_power(stack);
                let want = (most - have).min(rate);
                if want > 0.0 && grid.is_some() && take_energy(pos, want) {
                    set_portable_power(stack, have + want);
                }
            }
            let charging = entity.items.first().and_then(|s| s.as_ref()).is_some_and(|s| charger_result(&s.item).is_some());
            let progress = nbt_i64(&entity.nbt, "progress");
            if charging && progress >= CHARGE_TICKS {
                // A crank finished it.
                if let Some(Some(stack)) = entity.items.first_mut() {
                    stack.item = charger_result(&stack.item).unwrap_or("ae2:charged_certus_quartz_crystal").into();
                }
                set_nbt_i64(&mut entity.nbt, "progress", 0);
            } else if !charging {
                if progress != 0 {
                    set_nbt_i64(&mut entity.nbt, "progress", 0);
                }
            } else if grid.is_some() && take_energy(pos, CHARGE_AE_PER_TICK) {
                if progress + 1 >= CHARGE_TICKS {
                    if let Some(Some(stack)) = entity.items.first_mut() {
                        stack.item = charger_result(&stack.item).unwrap_or("ae2:charged_certus_quartz_crystal").into();
                    }
                    set_nbt_i64(&mut entity.nbt, "progress", 0);
                } else {
                    set_nbt_i64(&mut entity.nbt, "progress", progress + 1);
                }
            }
        }
        "molecular_assembler" => {
            // The screen's progress (of 100), and what the client draws in the glass while it works.
            entity.data.resize(2, 0);
            let job = SERVER.with_borrow_mut(|s| {
                let now = s.tick;
                s.assembler_jobs.retain(|_, (_, end, _, _)| *end > now);
                s.assembler_jobs.get(&pos).map(|(from, end, item, _)| (((now - from) * 100 / (end - from).max(1)) as i32, item.clone()))
            });
            entity.data[0] = job.as_ref().map_or(0, |j| j.0);
            entity.data[1] = 100;
            let look = job.map_or(String::new(), |j| j.1);
            let old = match entity.nbt.get(&java("look")) {
                Some(Tag::String(s)) => s.to_string().unwrap_or_default(),
                _ => String::new(),
            };
            if look != old {
                entity.nbt.insert(java("look"), Tag::String(java(&look)));
                // MolecularAssemblerBlockEntity's speed by its cards: the sparks' number.
                let speed = [10, 13, 17, 20, 25, 50][cards(entity, 1, "ae2:speed_card").min(5)];
                set_nbt_i64(&mut entity.nbt, "speed", speed);
                SERVER.with_borrow_mut(|s| s.flips.push(pos));
            }
        }
        "inscriber" => {
            inscriber_tick(game, pos, grid.is_some(), entity);
            let look = inscriber_look(entity);
            let old = match entity.nbt.get(&java("look")) {
                Some(Tag::String(s)) => s.to_string().unwrap_or_default(),
                _ => String::new(),
            };
            if look != old {
                entity.nbt.insert(java("look"), Tag::String(java(&look)));
                SERVER.with_borrow_mut(|s| s.flips.push(pos));
            }
        }
        _ if is_crafting(&entity.id) => {
            // CraftingCPUCluster.updateOutput: a monitor shows its CPU's job and how many are left.
            if entity.id == "ae2:crafting_monitor" {
                let job = SERVER.with_borrow(|s| {
                    let info = grid.and_then(|g| s.grids.get(g))?;
                    let job = s.cpus.get(info.cpu_of.get(&pos)?)?.job()?;
                    (job.remaining_amount > 0).then(|| (wire_id(&job.final_output.what), key_amount(&wire_id(&job.final_output.what), job.remaining_amount, super::port_ae2_core::AmountFormat::Slot)))
                });
                let (show, text) = job.unwrap_or_default();
                if show != nbt_string(&entity.nbt, "show") || text != nbt_string(&entity.nbt, "text") {
                    entity.nbt.insert(java("show"), Tag::String(java(&show)));
                    entity.nbt.insert(java("text"), Tag::String(java(&text)));
                    game.native_block_entity_sync(pos);
                }
            }
            // The CPU status screen: formed, its cluster's storage (kB) and co-processors, busy, progress (per
            // mille), items left, then the grid's CPUs and how many are busy.
            entity.data.resize(8, 0);
            let formed = FORMED.with_borrow(|f| f.contains(&pos));
            let info = SERVER.with_borrow(|s| {
                let g = grid.and_then(|g| s.grids.get(g))?;
                let key = *g.cpu_of.get(&pos)?;
                let &(_, bytes, accel) = g.cpus.iter().find(|c| c.0 == key)?;
                let busy = g.cpus.iter().filter(|c| s.cpus.get(&c.0).is_some_and(|c| c.has_job())).count();
                let cpu = s.cpus.get(&key);
                let job = cpu.and_then(|c| c.job());
                // 1 formed and powered, 2 formed but its network is offline (no power: AE2's CPU can't run).
                Some([
                    if g.online { 1 } else { 2 },
                    (bytes / 1024).min(32_767) as i32,
                    accel,
                    i32::from(job.is_some()),
                    job.map_or(0, |j| (j.time.progress() * 1000.0) as i32),
                    job.map_or(0, |j| j.remaining_amount.min(32_767) as i32),
                    g.cpus.len() as i32,
                    busy as i32,
                ])
            });
            entity.data.copy_from_slice(&info.filter(|_| formed).unwrap_or([0; 8]));
        }
        "controller" => controller_data(grid, pos, entity),
        "flawless_budding_quartz" | "flawed_budding_quartz" | "chipped_budding_quartz" | "damaged_budding_quartz" => {
            budding_tick(game, pos, &entity.id.clone());
        }
        "crystal_resonance_generator" => {
            // EnergyService.onServerStartTick: the grid's current passive generator makes power; any other is
            // suppressed (they all make the same, so the first one keeps it).
            let suppressed = grid.is_some_and(|g| {
                SERVER.with_borrow_mut(|s| {
                    let now = s.tick;
                    let current = s.crg_current.get(&g).copied().filter(|c| {
                        *c == pos
                            || (s.grid_of.get(c) == Some(&g)
                                && s.devices.get(c).is_some_and(|(id, t)| id == "ae2:crystal_resonance_generator" && now.saturating_sub(*t) <= 2))
                    });
                    match current {
                        Some(c) if c != pos => true,
                        _ => {
                            s.crg_current.insert(g, pos);
                            false
                        }
                    }
                })
            });
            if grid.is_some() && !suppressed {
                add_energy(pos, RESONANCE_AE_PER_TICK);
            }
            if (nbt_i64(&entity.nbt, "suppressed") != 0) != suppressed {
                set_nbt_i64(&mut entity.nbt, "suppressed", i64::from(suppressed));
            }
        }
        "cell_workbench" => workbench_tick(entity),
        "pattern_provider" => pattern_provider_tick(game, pos, grid.filter(|_| active), entity),
        "light_detector" => light_detector_tick(game, pos, entity),
        "toggle_bus" | "inverted_toggle_bus" => toggle_tick(game, pos, entity),
        "storage_monitor" | "conversion_monitor" => {
            if SERVER.with_borrow(|s| s.tick) % 20 == 0 {
                let key = key_from_string(&nbt_string(&entity.nbt, "item"));
                let amount = match (key, grid.filter(|_| active)) {
                    (Some(key), Some(g)) => with_storage(game, g, None, |st| st.contents().get(&key)),
                    _ => 0,
                };
                if nbt_i64(&entity.nbt, "amount") != amount {
                    set_nbt_i64(&mut entity.nbt, "amount", amount);
                    game.native_block_entity_sync(pos);
                }
            }
        }
        "vibration_chamber" => {
            // VibrationChamberBlockEntity.tickingRequest: fuel burns faster while the network takes all it makes, and
            // slower as it fills. Energy Cards make more of each fuel tick, Acceleration Cards raise the top speed.
            // (Kept in thousandths: block entity data is whole numbers.)
            let per_fuel_tick = 5.0 * (1.0 + cards(entity, 1, "ae2:energy_card") as f64 / 2.0);
            let top = (40.0 + 40.0 * cards(entity, 1, "ae2:speed_card") as f64 / 2.0) / per_fuel_tick;
            let least = 4.0 / per_fuel_tick;
            let mut left = nbt_i64(&entity.nbt, "burn") as f64 / if nbt_i64(&entity.nbt, "burn_v2") != 0 { 1000.0 } else { 1.0 };
            set_nbt_i64(&mut entity.nbt, "burn_v2", 1);
            let mut rate = match nbt_i64(&entity.nbt, "burn_rate") {
                0 => 5.0 / per_fuel_tick,
                r => r as f64 / 1000.0,
            };
            if left <= 0.0 {
                if let Some(fuel) = entity.items.first_mut().and_then(|s| s.as_mut())
                    && let Some(ticks) = fuel_ticks(game, &fuel.item)
                    && grid.is_some()
                {
                    fuel.count -= 1;
                    if fuel.count == 0 {
                        entity.items[0] = None;
                    }
                    left = ticks as f64;
                    set_nbt_i64(&mut entity.nbt, "burn_total", ticks);
                } else {
                    rate = 5.0 / per_fuel_tick;
                }
            } else {
                let used = rate.min(left);
                left -= used;
                let step = (top - least) / 100.0;
                let room = SERVER.with_borrow(|s| {
                    grid.and_then(|g| s.grids.get(g)).and_then(|g| g.nodes.iter().min().copied().map(|root| grid_capacity(g, &s.devices) - s.energy.get(&root).copied().unwrap_or(0.0))).unwrap_or(0.0)
                });
                let made = used * per_fuel_tick;
                add_energy(pos, made);
                rate += if room < made { -step } else { step };
                rate = rate.clamp(least, top);
            }
            set_nbt_i64(&mut entity.nbt, "burn", (left * 1000.0) as i64);
            set_nbt_i64(&mut entity.nbt, "burn_rate", (rate * 1000.0) as i64);
            // The burn time left and the fuel's whole burn time, scaled to fit the 16-bit data slots alike.
            let (left, total) = (nbt_i64(&entity.nbt, "burn") / 1000, nbt_i64(&entity.nbt, "burn_total").max(1));
            let scale = (total / i64::from(i16::MAX) + 1).max(1);
            entity.data.resize(2, 0);
            entity.data[0] = (left / scale) as i32;
            entity.data[1] = (total / scale) as i32;
            SERVER.with_borrow_mut(|s| s.prop_sets.push((pos, "powered", if left > 0 { "true" } else { "false" })));
        }
        _ => {}
    }
    let _ = tick;
}

/// The charger: AE per tick and ticks to charge one certus quartz crystal (AE2's 1 600 AE a crystal).
const CHARGE_AE_PER_TICK: f64 = 16.0;
const CHARGE_TICKS: i64 = 100;
/// What a crystal resonance generator makes while it's on a network.
const RESONANCE_AE_PER_TICK: f64 = 20.0;

/// Each cell slot's look for clients: the cell's model name and its state (`CellState` as a number), or "".
fn cell_looks(items: &[Option<ItemStack>]) -> Vec<String> {
    items
        .iter()
        .map(|s| match s {
            Some(stack) => match (ae2_name(&stack.item), cell_type(&stack.item)) {
                (Some(name), Some(cell)) => {
                    let state = saved_cell_status(stack).unwrap_or_else(|| {
                        cell_status_number(&BasicCellInventory::new(cell, &[], cell_fuzzy(stack), cell_upgrades(stack), &read_cell(stack)))
                    });
                    let model = name.strip_prefix("item_storage_cell_").map(|t| format!("{t}_item_cell")).or_else(|| name.strip_prefix("fluid_storage_cell_").map(|t| format!("{t}_fluid_cell"))).unwrap_or_else(|| "default_cell".to_owned());
                    format!("{model}:{state}")
                }
                _ => "default_cell:1".to_owned(),
            },
            None => String::new(),
        })
        .collect()
}

/// Takes AE from the grid of `pos`; false when it hasn't that much.
fn take_energy(pos: Pos, ae: f64) -> bool {
    SERVER.with_borrow_mut(|s| {
        let Some(&g) = s.grid_of.get(&pos) else { return false };
        if !s.grids[g].online {
            return false;
        }
        let Some(root) = s.grids[g].nodes.iter().min().copied() else { return false };
        let e = s.energy.entry(root).or_insert(0.0);
        if *e >= ae {
            *e -= ae;
            true
        } else {
            false
        }
    })
}

fn fuel_ticks(game: &Game<'_>, item: &str) -> Option<i64> {
    // The item's furnace burn time, as Horizon's furnace works it out (its `cooking_fuel` is a provider).
    let ticks = game.res.providers().burn_time(game.res.items(), &ItemStack::new(item, 1), false);
    (ticks > 0).then_some(i64::from(ticks))
}

fn nbt_i64(nbt: &Compound, key: &str) -> i64 {
    match nbt.get(&java(key)) {
        Some(Tag::Long(v)) => *v,
        Some(Tag::Int(v)) => i64::from(*v),
        _ => 0,
    }
}

fn set_nbt_i64(nbt: &mut Compound, key: &str, value: i64) {
    nbt.insert(java(key), Tag::Long(value));
}

/// AE a generator at `pos` adds to its grid's buffer.
fn add_energy(pos: Pos, ae: f64) {
    SERVER.with_borrow_mut(|s| {
        if let Some(&g) = s.grid_of.get(&pos)
            && let Some(root) = s.grids[g].nodes.iter().min().copied()
        {
            let cap = grid_capacity(&s.grids[g], &s.devices);
            let e = s.energy.entry(root).or_insert(0.0);
            *e = (*e + ae).min(cap);
        }
    });
}

fn grid_capacity(grid: &GridInfo, devices: &HashMap<Pos, (String, u64)>) -> f64 {
    grid.nodes
        .iter()
        .filter_map(|p| devices.get(p))
        .map(|(id, _)| match ae2_name(id).unwrap_or("") {
            "controller" => 8_000.0,
            "energy_cell" => 200_000.0,
            "dense_energy_cell" => 1_600_000.0,
            "creative_energy_cell" => 0.0,
            _ => 0.0,
        })
        .sum::<f64>()
        .max(800.0)
}

/// `native_server_tick` (end): once a second the grids are worked out again and power is paid; every half
/// second open terminals get their items.
pub fn server_tick(game: &mut Game<'_>) {
    let tick = SERVER.with_borrow_mut(|s| {
        s.tick += 1;
        s.tick
    });
    load_channel_mode();
    deliver_gives(game);
    migrate_legacy_twins(game);
    tiny_tnt_tick(game);
    for (pos, key, value) in SERVER.with_borrow_mut(|s| std::mem::take(&mut s.prop_sets)) {
        if game.native_block_property(pos, key).is_some_and(|v| v != value) {
            game.native_set_block_property_with(pos, key, value, 3);
        }
    }
    for pos in SERVER.with_borrow_mut(|s| std::mem::take(&mut s.flips)) {
        // The block and its block entity's update tag go to the clients again.
        game.native_block_entity_sync(pos);
    }
    if tick % 20 == 0 {
        rebuild(game, tick);
    }
    for pos in SERVER.with_borrow_mut(|s| std::mem::take(&mut s.signal_updates)) {
        game.native_update_signal_neighbors("minecraft:overworld", pos);
    }
    for (pos, on) in SERVER.with_borrow_mut(|s| std::mem::take(&mut s.emitter_changes)) {
        if matches!(block_id(game, pos).as_str(), "ae2:level_emitter" | "ae2:energy_level_emitter") {
            game.native_set_block_property_with(pos, "powered", if on { "true" } else { "false" }, 3);
            game.native_update_signal_neighbors("minecraft:overworld", pos);
        }
    }
    if tick % 1200 == 0 {
        SERVER.with_borrow_mut(|s| {
            let holders: HashSet<Pos> = s.grids.iter().filter_map(|g| g.storage.first().copied()).collect();
            let gone: Vec<Pos> = s.mounted.keys().filter(|k| !holders.contains(*k)).copied().collect();
            for k in gone {
                if let Some(mut m) = s.mounted.remove(&k) {
                    for (i, (_, _, cid, ver)) in m.cells.drain(..).enumerate() {
                        if let Some(inv) = m.storage.unmount(i as u64) {
                            s.cells.insert(cid, (ver, inv, tick));
                        }
                    }
                }
            }
            s.cells.retain(|_, (_, _, used)| tick.saturating_sub(*used) < 6000);
        });
    }
    if tick % 4 == 1 {
        stream_terminals(game);
    }
    run_cpus(game, tick);
    if tick % 10 == 3 {
        run_buses(game);
    }
}

fn is_crafting(block: &str) -> bool {
    matches!(ae2_name(block), Some(n) if n.starts_with("crafting_") && n != "crafting_terminal" || n.ends_with("_crafting_storage"))
}

/// The state properties naming a formed crafting block's neighbours in its cluster, in `SIDES` order.
const CUBE_SIDES: [&str; 6] = ["side_down", "side_up", "side_north", "side_south", "side_west", "side_east"];
/// Whether a crafting block shows its cluster (Horizon wants added properties to sort after the barrel's
/// `facing` and `open`).
const FORMED_KEY: &str = "structure";

thread_local! {
    /// Crafting blocks shown formed, to turn back when their cluster breaks.
    static FORMED: RefCell<HashSet<Pos>> = RefCell::new(HashSet::new());
}

/// AE2's crafting clusters (CraftingCPUCalculator): touching crafting blocks form one when they fill a cuboid
/// with at least one crafting storage. Each formed block, with which of its sides (`SIDES`) the cluster goes on.
fn crafting_clusters(blocks: &HashMap<Pos, String>) -> (HashMap<Pos, [bool; 6]>, Vec<Vec<Pos>>) {
    let mut seen = HashSet::new();
    let mut out = HashMap::new();
    let mut clusters = Vec::new();
    for (start, name) in blocks {
        if !is_crafting(name) || !seen.insert(*start) {
            continue;
        }
        let mut cluster = vec![*start];
        let mut i = 0;
        while i < cluster.len() {
            for d in SIDES {
                let q = add(cluster[i], d);
                if blocks.get(&q).is_some_and(|b| is_crafting(b)) && seen.insert(q) {
                    cluster.push(q);
                }
            }
            i += 1;
        }
        let (lo, hi) = cluster.iter().fold(([i32::MAX; 3], [i32::MIN; 3]), |(lo, hi), p| {
            (std::array::from_fn(|k| lo[k].min(p[k])), std::array::from_fn(|k| hi[k].max(p[k])))
        });
        let volume: i64 = (0..3).map(|k| (hi[k] - lo[k] + 1) as i64).product();
        let storage = cluster.iter().any(|p| blocks[p].ends_with("_crafting_storage"));
        if volume != cluster.len() as i64 || !storage {
            continue;
        }
        let members: HashSet<Pos> = cluster.iter().copied().collect();
        for p in &cluster {
            out.insert(*p, SIDES.map(|d| members.contains(&add(*p, d))));
        }
        clusters.push(cluster);
    }
    (out, clusters)
}

fn rebuild(game: &mut Game<'_>, tick: u64) {
    // Devices not seen for two seconds are gone (broken, or their chunk unloaded).
    let starts: Vec<Pos> = SERVER.with_borrow_mut(|s| {
        s.devices.retain(|_, (_, seen)| tick.saturating_sub(*seen) <= 40);
        s.devices.keys().copied().collect()
    });
    // Walk the cables from every device.
    let mut blocks: HashMap<Pos, String> = HashMap::new();
    let mut net = Network::new();
    for start in starts {
        if blocks.contains_key(&start) {
            continue;
        }
        let name = block_id(game, start);
        if !is_device(&name) {
            continue;
        }
        let mut queue = VecDeque::from([(start, name)]);
        while let Some((p, name)) = queue.pop_front() {
            if blocks.contains_key(&p) {
                continue;
            }
            net.add_node(pack(p), node_kind(&name), p);
            blocks.insert(p, name.clone());
            for d in SIDES {
                let q = add(p, d);
                let other = match blocks.get(&q) {
                    Some(n) => n.clone(),
                    None => block_id(game, q),
                };
                if is_network_block(&other) && links(game, p, &name, q, &other) {
                    if blocks.contains_key(&q) {
                        net.connect(pack(p), pack(q));
                    } else {
                        queue.push_back((q, other));
                    }
                }
            }
        }
    }
    // Connections between blocks found from different starts or after their neighbour.
    let positions: Vec<Pos> = blocks.keys().copied().collect();
    for p in &positions {
        for d in SIDES {
            let q = add(*p, d);
            if let Some(other) = blocks.get(&q)
                && links(game, *p, &blocks[p], q, other)
            {
                net.connect(pack(*p), pack(q));
            }
        }
    }
    // ME P2P tunnels: an input and its outputs carry the network between them (AE2's tunnel channels).
    let tunnels: Vec<(Pos, i64, bool)> = positions
        .iter()
        .filter(|p| blocks[*p] == "ae2:me_p2p_tunnel")
        .filter_map(|p| game.native_with_mod_block_entity(*p, |e| (*p, nbt_i64(&e.nbt, "freq"), nbt_i64(&e.nbt, "output") != 0)))
        .filter(|t| t.1 != 0)
        .collect();
    for (a, fa, out_a) in &tunnels {
        for (b, fb, out_b) in &tunnels {
            if !out_a && *out_b && fa == fb {
                net.connect(pack(*a), pack(*b));
            }
        }
    }
    // Quantum network bridges: formed link chambers whose entangled singularities match join their networks.
    let links: Vec<(Pos, i64)> = positions
        .iter()
        .filter(|p| blocks[*p] == "ae2:quantum_link")
        .filter(|p| [(1usize, 2usize), (0, 2), (0, 1)].iter().any(|&(i, j)| {
            (-1..=1).all(|a| (-1..=1).all(|b| {
                let mut q = **p;
                q[i] += a;
                q[j] += b;
                (a == 0 && b == 0) || blocks.get(&q).is_some_and(|x| x == "ae2:quantum_ring")
            }))
        }))
        .filter_map(|p| {
            let st = game.native_with_mod_block_entity(*p, |e| e.items.first().cloned().flatten()).flatten()?;
            Some((*p, singularity_freq(&st)?))
        })
        .collect();
    for (i, (a, fa)) in links.iter().enumerate() {
        for (b, fb) in &links[i + 1..] {
            if fa == fb {
                net.connect(pack(*a), pack(*b));
            }
        }
    }
    let by_id: HashMap<u64, Pos> = positions.iter().map(|p| (pack(*p), *p)).collect();
    let (formed, clusters) = crafting_clusters(&blocks);
    let mut grids = Vec::new();
    let mut grid_of = HashMap::new();
    for g in net.grids() {
        let nodes: HashSet<Pos> = g.nodes.iter().map(|n| by_id[n]).collect();
        let active: HashSet<Pos> = g.nodes.iter().filter(|n| g.is_active(**n, net.kinds())).map(|n| by_id[n]).collect();
        let mut storage: Vec<Pos> = nodes
            .iter()
            .copied()
            .filter(|p| matches!(blocks.get(p).and_then(|b| ae2_name(b)), Some("drive" | "chest")))
            .filter(|p| active.contains(p))
            .collect();
        storage.sort();
        let idle = g.nodes.iter().map(|n| net.kinds()[n].idle_power).sum();
        // Pattern providers' patterns, and the CPU the crafting storage forms.
        let mut patterns = Vec::new();
        for p in &nodes {
            match blocks.get(p).and_then(|b| ae2_name(b)) {
                Some("pattern_provider") if active.contains(p) => {
                    let items = game.native_with_mod_block_entity(*p, |e| e.items.clone()).unwrap_or_default();
                    for stack in items.iter().flatten() {
                        if let Some(pattern) = decode_pattern(game, stack) {
                            patterns.push((*p, Arc::new(pattern)));
                        }
                    }
                }
                _ => {}
            }
        }
        // Formation planes place into the block their front faces.
        let mut formation_planes = Vec::new();
        for p in &nodes {
            if blocks.get(p).map(String::as_str) == Some("ae2:formation_plane") && active.contains(p) {
                let facing = game.native_block_property(*p, "facing");
                formation_planes.push((*p, add(*p, facing_dir(facing.as_deref()))));
            }
        }
        // Buses work on the block their front faces.
        let mut storage_buses = Vec::new();
        let mut io_buses = Vec::new();
        for p in &nodes {
            let kind = blocks.get(p).and_then(|b| ae2_name(b)).unwrap_or("");
            if !matches!(kind, "import_bus" | "export_bus" | "storage_bus") || !active.contains(p) {
                continue;
            }
            let d = match game.native_block_property(*p, "facing").as_deref() {
                Some("down") => [0, -1, 0],
                Some("up") => [0, 1, 0],
                Some("south") => [0, 0, 1],
                Some("west") => [-1, 0, 0],
                Some("east") => [1, 0, 0],
                _ => [0, 0, -1],
            };
            let target = add(*p, d);
            match kind {
                "storage_bus" => storage_buses.push((*p, target)),
                k => io_buses.push((*p, target, k == "import_bus")),
            }
        }
        // One CPU for each formed cluster on this grid: its storage's bytes and its co-processors.
        let mut cpus = Vec::new();
        let mut cpu_of = HashMap::new();
        for cluster in clusters.iter().filter(|c| nodes.contains(&c[0])) {
            let mut bytes = 0;
            let mut accel = 0;
            let mut key = None::<Pos>;
            for p in cluster {
                match ae2_name(&blocks[p]) {
                    Some(n) if n.ends_with("_crafting_storage") => {
                        let kb: i64 = n.trim_end_matches("_crafting_storage").trim_end_matches('k').parse().unwrap_or(1);
                        bytes += kb * 1024;
                        key = Some(key.map_or(*p, |k| k.min(*p)));
                    }
                    Some("crafting_accelerator") => accel += 1,
                    _ => {}
                }
            }
            let Some(key) = key else { continue };
            cpus.push((key, bytes, accel));
            cpu_of.extend(cluster.iter().map(|p| (*p, key)));
        }
        // The status screen's "CPU #n" stays the same CPU however the clusters were found.
        cpus.sort_by_key(|c| c.0);
        let info = GridInfo {
            online: g.controller_state != ControllerState::ControllerConflict,
            conflict: g.controller_state == ControllerState::ControllerConflict,
            too_many: g.controller_state == ControllerState::NoController
                && g.nodes.iter().filter(|n| net.kinds().get(*n).is_some_and(|k| k.requires_channel)).count() > 8,
            cable_channels: g.used_channels.iter().filter_map(|(n, c)| Some((*by_id.get(n)?, *c))).collect(),
            nodes,
            active,
            storage,
            idle,
            channels: g.channels_in_use,
            patterns,
            cpus,
            cpu_of,
            storage_buses,
            io_buses,
            formation_planes,
        };
        for p in &info.nodes {
            grid_of.insert(*p, grids.len());
        }
        grids.push(info);
    }
    for (i, g) in grids.iter().enumerate() {
        once(&format!(
            "grid {i}: {} blocks, online {}, {} active, {} storage, idle {}",
            g.nodes.len(),
            g.online,
            g.active.len(),
            g.storage.len(),
            g.idle
        ));
    }
    // Quartz fibers (QuartzFiberPart): the grid behind and the grid in front share one energy store, no channels.
    let mut pool: Vec<usize> = (0..grids.len()).collect();
    fn find(pool: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while pool[r] != r {
            r = pool[r];
        }
        pool[i] = r;
        r
    }
    for (p, name) in &blocks {
        if ae2_name(name) != Some("quartz_fiber") {
            continue;
        }
        let back = part_back(game, *p);
        let front = add(*p, [-back[0], -back[1], -back[2]]);
        if let (Some(&a), Some(&b)) = (grid_of.get(p), grid_of.get(&front)) {
            let (a, b) = (find(&mut pool, a), find(&mut pool, b));
            pool[a] = b;
        }
    }
    let pools: Vec<usize> = (0..grids.len()).map(|i| find(&mut pool, i)).collect();
    let changes = SERVER.with_borrow_mut(|s| {
        // Power: each grid pays a second of idle draw from its buffer (carried over by its lowest position); grids a
        // quartz fiber joins pay from, and fill, one store.
        let mut energy = HashMap::new();
        let mut pooled: HashMap<usize, (bool, f64, f64, f64, Pos)> = HashMap::new();
        for (i, g) in grids.iter().enumerate() {
            let Some(root) = g.nodes.iter().min().copied() else { continue };
            let creative = g.nodes.iter().any(|p| s.devices.get(p).is_some_and(|(id, _)| id == "ae2:creative_energy_cell"));
            let had: f64 = s.energy.iter().filter(|(p, _)| g.nodes.contains(*p)).map(|(_, e)| *e).sum();
            let cap: f64 = g
                .nodes
                .iter()
                .filter_map(|p| s.devices.get(p))
                .map(|(id, _)| match ae2_name(id).unwrap_or("") {
                    "controller" => 8_000.0,
                    "energy_cell" => 200_000.0,
                    "dense_energy_cell" => 1_600_000.0,
                    _ => 0.0,
                })
                .sum::<f64>()
                .max(800.0);
            let e = pooled.entry(pools[i]).or_insert((false, 0.0, 0.0, 0.0, root));
            e.0 |= creative;
            e.1 += had;
            e.2 += g.idle * 20.0;
            e.3 += cap;
            e.4 = e.4.min(root);
        }
        for (i, g) in grids.iter_mut().enumerate() {
            let Some(&(creative, had, need, cap, root)) = pooled.get(&pools[i]) else { continue };
            // AE2's grid is powered while it has stored energy for its draw: none stored is off, even drawing none.
            let powered = creative || (had > 0.0 && had >= need);
            once(&format!("power: creative {creative}, had {had}, need {need}"));
            g.online &= powered;
            if g.nodes.contains(&root) {
                energy.insert(root, if creative { cap } else { (had - need).max(0.0).min(cap) });
            }
        }
        for g in &mut grids {
            let Some(root) = g.nodes.iter().min().copied() else { continue };
            if energy.contains_key(&root) {
                continue;
            }
            let cap: f64 = g
                .nodes
                .iter()
                .filter_map(|p| s.devices.get(p))
                .map(|(id, _)| match ae2_name(id).unwrap_or("") {
                    "controller" => 8_000.0,
                    "energy_cell" => 200_000.0,
                    "dense_energy_cell" => 1_600_000.0,
                    _ => 0.0,
                })
                .sum::<f64>()
                .max(800.0);
            // Its pool's store sits at another grid's root: this one keeps none of its own.
            let _ = cap;
            energy.entry(root).or_insert(0.0);
        }
        s.energy = energy;
        // Which lit blocks change.
        let mut changes = Vec::new();
        for (p, name) in &blocks {
            if !shows_power(name) {
                continue;
            }
            let on = grid_of.get(p).is_some_and(|&g| grids[g].online && grids[g].active.contains(p));
            s.powered.insert(*p, on);
            // A part's ring: powered (AE2's isPowered) and its channel (isActive) apart.
            let powered = grid_of.get(p).is_some_and(|&g| grids[g].online);
            // (MolecularAssemblerBlockEntity.isPowered: power alone, whatever its channel.)
            let lit = on || (ae2_name(name) == Some("molecular_assembler") && powered);
            changes.push((*p, lit, powered));
        }
        s.powered.retain(|p, _| blocks.contains_key(p));
        // CPUs follow their grids: made, resized, or turned off (a job stays until its blocks are back).
        for g in &grids {
            for &(key, bytes, accel) in &g.cpus {
                let cpu = s.cpus.entry(key).or_insert_with(|| CraftingCpu::new(bytes, accel));
                cpu.storage_bytes = bytes;
                cpu.co_processors = accel;
                cpu.active = g.online;
            }
        }
        for (key, cpu) in s.cpus.iter_mut() {
            if !grids.iter().any(|g| g.cpus.iter().any(|c| c.0 == *key)) {
                cpu.active = false;
            }
        }
        s.grids = grids;
        s.grid_of = grid_of;
        changes
    });
    // The lit look follows the block's own state (a block placed again starts unlit).
    for (p, on, powered) in changes {
        let channel = game.native_block_property(p, "with_channel");
        let want = if on || (channel.is_some() && powered) { "true" } else { "false" };
        if game.native_block_property(p, "powered").is_some_and(|v| v != want) {
            game.native_set_block_property_with(p, "powered", want, 3);
        }
        let has = if on { "true" } else { "false" };
        if channel.is_some_and(|v| v != has) {
            game.native_set_block_property_with(p, "with_channel", has, 3);
        }
        panel_light(game, p, on);
    }
    // Smart cables light one line for each channel they carry (AE2's channels_0N then channels_1N: up to 8), a
    // dense one a line for each 4 of its 32.
    let channels: HashMap<Pos, u32> = SERVER.with_borrow(|s| {
        s.grid_of.iter().filter_map(|(p, g)| Some((*p, s.grids.get(*g)?.cable_channels.get(p).copied().unwrap_or(0)))).collect()
    });
    for (p, name) in &blocks {
        let per_light = if name.ends_with("_smart_dense_cable") {
            4
        } else if name.ends_with("_smart_cable") {
            1
        } else {
            continue;
        };
        let lit = channels.get(p).copied().unwrap_or(0).div_ceil(per_light).min(8).to_string();
        if is_twin(&game.native_block(*p)) {
            let changed = game
                .native_with_mod_block_entity(*p, |e| {
                    let changed = nbt_string(&e.nbt, "lights") != lit;
                    e.nbt.insert(java("lights"), Tag::String(java(&lit)));
                    changed
                })
                .unwrap_or(false);
            if changed {
                game.native_block_entity_sync(*p);
            }
            continue;
        }
        if game.native_block_property(*p, "x_lights").is_some_and(|v| v != lit) {
            game.native_set_block_property_with(*p, "x_lights", &lit, 2);
        }
    }
    // Buses, planes and tunnels: the cable behind them, drawn in their own block as AE2 draws the part's cable
    // (none when there is no cable: a part alone is only the part). Planes: their edges join the same kind of
    // plane on the same face beside them (PlaneConnectionHelper).
    for (p, name) in &blocks {
        let Some(n) = ae2_name(name) else { continue };
        if !(n.ends_with("_bus") || n.ends_with("_p2p_tunnel") || n.ends_with("_plane")) {
            continue;
        }
        // The wire's block, then the sides its arms reach: "ae2:fluix_glass_cable north,east".
        let wire = match bus_wire(game, *p) {
            Some(w) => {
                let sides: Vec<&str> = SIDES
                    .iter()
                    .zip(["down", "up", "north", "south", "west", "east"])
                    .filter(|(d, _)| {
                        let q = add(*p, **d);
                        let other = block_id(game, q);
                        is_network_block(&other) && self::links(game, *p, name, q, &other)
                    })
                    .map(|(_, k)| k)
                    .collect();
                format!("{w} {}", sides.join(","))
            }
            None => String::new(),
        };
        let changed = game
            .native_with_mod_block_entity(*p, |e| {
                let was = nbt_string(&e.nbt, "wire");
                if was != wire {
                    e.nbt.insert(java("wire"), Tag::String(java(&wire)));
                }
                was != wire
            })
            .unwrap_or(false);
        if changed {
            game.native_block_entity_sync(*p);
        }
        if n.ends_with("_plane") {
            plane_links(game, *p, name);
        }
    }
    // Cable shapes as their neighbours are now (blocks set by commands skip the shape update).
    for p in blocks.keys() {
        cable_sides(game, *p);
    }
    // Crafting blocks show the cluster they form; those no longer in one go back to their own look.
    let was = FORMED.with_borrow_mut(|f| std::mem::replace(f, formed.keys().copied().collect()));
    for p in was.iter().filter(|p| !formed.contains_key(*p)) {
        if is_crafting(&block_id(game, *p)) && game.native_block_property(*p, FORMED_KEY).as_deref() == Some("true") {
            game.native_set_block_property_with(*p, FORMED_KEY, "false", 3);
        }
    }
    for (p, joined) in &formed {
        let want = |on: bool| if on { "true" } else { "false" };
        if game.native_block_property(*p, FORMED_KEY).as_deref() != Some("true") {
            game.native_set_block_property_with(*p, FORMED_KEY, "true", 3);
        }
        for (key, on) in CUBE_SIDES.iter().zip(joined) {
            if game.native_block_property(*p, key).as_deref() != Some(want(*on)) {
                game.native_set_block_property_with(*p, key, want(*on), 3);
            }
        }
    }
    // A charger's side ports show where a cable meets them: `left` is clockwise of its facing.
    for (p, name) in &blocks {
        if ae2_name(name) != Some("charger") {
            continue;
        }
        let left: Pos = match game.native_block_property(*p, "facing").as_deref() {
            Some("south") => [-1, 0, 0],
            Some("west") => [0, 0, -1],
            Some("east") => [0, 0, 1],
            _ => [1, 0, 0],
        };
        let right = [-left[0], 0, -left[2]];
        for (key, d) in [("left", left), ("right", right)] {
            let want = if blocks.get(&add(*p, d)).is_some_and(|b| cable(b).is_some()) { "true" } else { "false" };
            if game.native_block_property(*p, key).as_deref() != Some(want) {
                game.native_set_block_property_with(*p, key, want, 3);
            }
        }
    }
}

/// A grid's storage kept mounted between uses (AE2 keeps its network inventory up the same way): its cells as
/// they sit in its drives (where, which cell, which version), mounted under their index in that list.
#[derive(Default)]
struct MountedStorage {
    storage: NetworkStorage<Box<dyn super::port_ae2_grid::MeStorage>>,
    cells: Vec<(Pos, usize, i64, i64)>,
}

/// The cells of a grid's drives and ME chests as one network storage, kept mounted while they stay where they
/// are; what changed goes back onto the cells. `own` is a block entity out of the world during a hook (its
/// position and entity), read from there.
fn with_storage<R>(
    game: &mut Game<'_>,
    grid: usize,
    mut own: Option<(Pos, &mut ModBlockEntity)>,
    f: impl FnOnce(&mut NetworkStorage<Box<dyn super::port_ae2_grid::MeStorage>>) -> R,
) -> R {
    super::port_ae2_grid::CPU_WATCH.with_borrow_mut(|w| w.grid = grid);
    let holders = SERVER.with_borrow(|s| s.grids.get(grid).filter(|g| g.online).map(|g| g.storage.clone()).unwrap_or_default());
    let buses = SERVER.with_borrow(|s| s.grids.get(grid).filter(|g| g.online).map(|g| g.storage_buses.clone()).unwrap_or_default());
    // The cells there now: where, which type, and the id and version written on each (only these are read).
    let mut found: Vec<(Pos, usize, CellType, Option<(i64, i64)>)> = Vec::new();
    for pos in &holders {
        let look = |e: &ModBlockEntity| -> Vec<(usize, CellType, Option<(i64, i64)>)> {
            e.items
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.as_ref().and_then(|st| Some((i, cell_type(&st.item)?, cell_id(st)))))
                .collect()
        };
        let cells = match own.as_mut() {
            Some((p, e)) if p == pos => look(e),
            _ => game.native_with_mod_block_entity(*pos, |e| look(e)).unwrap_or_default(),
        };
        found.extend(cells.into_iter().map(|(i, t, id)| (*pos, i, t, id)));
    }
    // The grid's mounted storage, keyed by its first holder (grid numbers change as networks are worked out).
    let key = holders.first().copied().unwrap_or([i32::MIN; 3]);
    let mut mounted = SERVER.with_borrow_mut(|s| s.mounted.remove(&key)).unwrap_or_default();
    let same = mounted.cells.len() == found.len()
        && mounted.cells.iter().zip(&found).all(|(m, f)| m.0 == f.0 && m.1 == f.1 && f.3 == Some((m.2, m.3)));
    if !same {
        // Put the old cells back in the cache, then mount what is there now (from the cache, or read once).
        let tick = SERVER.with_borrow(|s| s.tick);
        for (i, (_, _, cid, ver)) in mounted.cells.drain(..).enumerate() {
            if let Some(inv) = mounted.storage.unmount(i as u64) {
                SERVER.with_borrow_mut(|s| s.cells.insert(cid, (ver, inv, tick)));
            }
        }
        mounted.storage = NetworkStorage::new();
        for (pos, slot, cell, id) in &found {
            let cached = id.filter(|(i, _)| mounted.cells.iter().all(|c| c.2 != *i)).and_then(|(i, v)| {
                SERVER.with_borrow_mut(|s| match s.cells.remove(&i) {
                    Some((ver, inv, _)) if ver == v => Some((i, v, inv)),
                    _ => None,
                })
            });
            let (cid, ver, inv) = match cached {
                Some(c) => c,
                None => {
                    let stack = match own.as_mut() {
                        Some((p, e)) if p == pos => e.items.get(*slot).cloned().flatten(),
                        _ => game.native_with_mod_block_entity(*pos, |e| e.items.get(*slot).cloned().flatten()).flatten(),
                    };
                    let Some(stack) = stack else { continue };
                    let mut basic = BasicCellInventory::new(*cell, &cell_config(&stack), cell_fuzzy(&stack), cell_upgrades(&stack), &read_cell(&stack));
                    if &*stack.item == "ae2:matter_cannon" {
                        let ammo = cannon_ammo_items(game);
                        basic.accepts = Some(Box::new(move |k: &AeKey| ammo.contains(k.id()) || is_paint_ball(k.id())));
                    }
                    if &*stack.item == "ae2:color_applicator" {
                        // ColorApplicatorItem.isBlackListed: only what has a colour.
                        let paints = paint_items(game);
                        basic.accepts = Some(Box::new(move |k: &AeKey| paints.contains(k.id())));
                    }
                    let inv: Box<dyn super::port_ae2_grid::MeStorage> = Box::new(basic);
                    // A cell keeps its id, unless one mounted already has it (a copy): then it gets its own, and
                    // the id goes onto the cell now (version -1, so the write below happens).
                    match id.filter(|(i, _)| mounted.cells.iter().all(|c| c.2 != *i)) {
                        Some((i, v)) => (i, v, inv),
                        None => {
                            let i = SERVER.with_borrow_mut(|s| {
                                if s.next_cell_id == 0 {
                                    s.next_cell_id = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map_or(1, |d| d.as_nanos() as i64 & 0x3FFF_FFFF_FFFF_FFFF);
                                }
                                s.next_cell_id += 1;
                                s.next_cell_id
                            });
                            (i, -1, inv)
                        }
                    }
                }
            };
            let priority = match own.as_mut() {
                Some((p, e)) if p == pos => nbt_i64(&e.nbt, "priority") as i32,
                _ => priority_of(game, *pos),
            };
            mounted.storage.mount(mounted.cells.len() as u64, priority, inv);
            mounted.cells.push((*pos, *slot, cid, ver));
        }
    }
    // Storage buses: what the container in front of each holds now, asked each time (it changes outside the network).
    let mut externals = Vec::new();
    if MAX_STACKS.with_borrow(|m| m.is_empty()) {
        let all: HashMap<String, i64> = game.res.items().order.iter().map(|i| (i.to_string(), i64::from(max_stack(game, i)))).collect();
        MAX_STACKS.with_borrow_mut(|m| *m = all);
    }
    let dim = game.dim.id().to_owned();
    let mut fluid_externals = Vec::new();
    for (bus, target) in &buses {
        if block_id(game, *target).starts_with("ae2:") {
            continue;
        }
        let face = face_toward(*target, *bus);
        let Some(raw) = game.native_with_mod_block_entity(*bus, |e| bus_filter_raw(e)) else { continue };
        let filter = raw.resolve(game);
        let priority = priority_of(game, *bus);
        if let Some(view) = sided(game, *target, face) {
            let keyed: Vec<Option<(AeKey, i64, i64)>> = sided_slots(game, &view)
                .iter()
                .map(|s| {
                    s.as_ref().map(|st| {
                        let mut one = st.clone();
                        one.count = 1;
                        (key_of(game, &one), i64::from(st.count), i64::from(max_stack(game, &st.item)))
                    })
                })
                .collect();
            let access = (0..keyed.len()).map(|i| (view.insert[i], view.extract[i] && keyed[i].as_ref().is_none_or(|(k, _, _)| view.can_take(i, k.id())))).collect();
            let id = 1_000_000 + externals.len() as u64;
            mounted.storage.mount_volatile(id, priority, Box::new(External { slots: keyed, access, filter, changed: false }) as Box<dyn super::port_ae2_grid::MeStorage>);
            externals.push((id, view));
        } else if let Some(tanks) = game.native_fluid_tanks(&dim, *target, face) {
            // Tanks (and cauldrons): fluids through the bus too.
            let tanks: Vec<(Option<String>, i64, i64, bool, bool)> =
                tanks.into_iter().map(|t| (t.fluid, i64::from(t.amount), i64::from(t.capacity), t.can_fill, t.can_drain)).collect();
            let before: HashMap<String, i64> = tanks.iter().filter_map(|(f, n, ..)| Some((f.clone()?, *n))).fold(HashMap::new(), |mut m, (f, n)| {
                *m.entry(f).or_default() += n;
                m
            });
            let id = 3_000_000 + fluid_externals.len() as u64;
            mounted.storage.mount_volatile(id, priority, Box::new(ExternalFluids { tanks, filter, changed: false }) as Box<dyn super::port_ae2_grid::MeStorage>);
            fluid_externals.push((id, *target, face, before));
        }
    }
    // Formation planes: the network's last storage (lowest priority), each taking a block item into the air it faces.
    let planes = SERVER.with_borrow(|s| s.grids.get(grid).filter(|g| g.online).map(|g| g.formation_planes.clone()).unwrap_or_default());
    let mut formations = Vec::new();
    for (i, (_, target)) in planes.iter().enumerate() {
        let here = block_id(game, *target);
        let open = matches!(here.as_str(), "minecraft:air" | "minecraft:cave_air" | "minecraft:short_grass" | "minecraft:snow");
        let id = 2_000_000 + i as u64;
        let filter = game.native_with_mod_block_entity(planes[i].0, |e| bus_filter_raw(e)).map(|r| r.resolve(game)).unwrap_or_default();
        let priority = match priority_of(game, planes[i].0) {
            0 => i32::MIN,
            p => p,
        };
        let drop_items = game.native_with_mod_block_entity(planes[i].0, |e| nbt_i64(&e.nbt, "drop_items") != 0).unwrap_or(false);
        let lo = [f64::from(target[0]) - 1.0, f64::from(target[1]) - 1.0, f64::from(target[2]) - 1.0];
        let can_throw = game.native_item_entities(&dim, lo, [lo[0] + 3.0, lo[1] + 3.0, lo[2] + 3.0]).len() < 128;
        mounted.storage.mount_volatile(id, priority, Box::new(Formation { open, placed: None, blocks: block_names(game), filter, drop_items, can_throw, thrown: Vec::new() }) as Box<dyn super::port_ae2_grid::MeStorage>);
        formations.push((id, *target));
    }
    // Creative cells (CreativeCellInventory): what their partition names, without end; mounted fresh each time.
    let mut creative = Vec::new();
    for pos in &holders {
        let stacks: Vec<Option<ItemStack>> = match own.as_mut() {
            Some((p, e)) if p == pos => e.items.clone(),
            _ => game.native_with_mod_block_entity(*pos, |e| e.items.clone()).unwrap_or_default(),
        };
        let priority = match own.as_mut() {
            Some((p, e)) if p == pos => nbt_i64(&e.nbt, "priority") as i32,
            _ => priority_of(game, *pos),
        };
        for st in stacks.iter().flatten().filter(|st| is_creative_cell(&st.item)) {
            let id = 4_000_000 + creative.len() as u64;
            mounted.storage.mount_volatile(id, priority, Box::new(CreativeCell { keys: cell_config(st) }) as Box<dyn super::port_ae2_grid::MeStorage>);
            creative.push(id);
        }
    }
    let result = f(&mut mounted.storage);
    for id in creative {
        mounted.storage.unmount(id);
    }
    let mut changed = false;
    for (id, target) in formations {
        let Some(mut plane) = mounted.storage.unmount(id) else { continue };
        let Some(plane) = plane.as_any().downcast_mut::<Formation>() else { continue };
        if let Some(key) = plane.placed.take()
            && let Some(state) = game.pack.assets.registry.default_state(key.id())
        {
            let dim = game.dim.id().to_owned();
            game.native_set_block(&dim, target, state, 3);
        }
        for (key, n) in std::mem::take(&mut plane.thrown) {
            let at = [f64::from(target[0]) + 0.5, f64::from(target[1]) + 0.5, f64::from(target[2]) + 0.5];
            game.spawn_item(stack_of(&key, n as u32), at, [0.0, 0.0, 0.0], 10);
        }
    }
    for (id, view) in externals {
        let Some(mut ext) = mounted.storage.unmount(id) else { continue };
        let Some(ext) = ext.as_any().downcast_mut::<External>() else { continue };
        if !ext.changed {
            continue;
        }
        changed = true;
        for (i, s) in ext.slots.iter().enumerate() {
            let Some((p, slot)) = view.locate(i) else { continue };
            if let Some(be) = game.world.block_entities.get_mut(&(p[0], p[1], p[2]))
                && let Some(dst) = be.slots_mut().get_mut(slot)
            {
                *dst = s.as_ref().map(|(k, n, _)| stack_of(k, *n as u32));
            }
        }
        for (p, _) in &view.parts {
            game.container_changed((p[0], p[1], p[2]));
        }
    }
    // Tanks: what the network took or put, drained or filled through the bus' face.
    for (id, target, face, before) in fluid_externals {
        let Some(mut ext) = mounted.storage.unmount(id) else { continue };
        let Some(ext) = ext.as_any().downcast_mut::<ExternalFluids>() else { continue };
        if !ext.changed {
            continue;
        }
        changed = true;
        let mut after: HashMap<String, i64> = HashMap::new();
        for (f, n, ..) in &ext.tanks {
            if let Some(f) = f {
                *after.entry(f.clone()).or_default() += n;
            }
        }
        let fluids: HashSet<String> = before.keys().chain(after.keys()).cloned().collect();
        for f in fluids {
            let d = after.get(&f).copied().unwrap_or(0) - before.get(&f).copied().unwrap_or(0);
            if d > 0 {
                game.native_fluid_insert(target, face, &f, d as i32);
            } else if d < 0 {
                game.native_fluid_extract(&dim, target, face, (-d) as i32);
            }
        }
    }
    // Cells that changed (or are new to the network) are saved on their items, a new version each time.
    for i in 0..mounted.cells.len() {
        let Some(inv) = mounted.storage.get_mut(i as u64) else { continue };
        let Some(cell) = inv.as_any().downcast_mut::<BasicCellInventory>() else { continue };
        let contents = cell.persist();
        let (pos, slot, cid, ver) = mounted.cells[i];
        if contents.is_none() && ver >= 0 {
            continue;
        }
        changed |= contents.is_some();
        let status = cell_status_number(cell);
        let ver = ver + 1;
        mounted.cells[i].3 = ver;
        let write = |e: &mut ModBlockEntity| {
            if let Some(Some(stack)) = e.items.get_mut(slot) {
                if let Some(contents) = &contents {
                    write_cell(stack, contents);
                }
                set_cell_id(stack, cid, ver, status);
            }
        };
        match own.as_mut() {
            Some((p, e)) if *p == pos => write(e),
            _ => {
                game.native_with_mod_block_entity(pos, write);
            }
        }
    }
    SERVER.with_borrow_mut(|s| {
        s.mounted.insert(key, mounted);
        if changed {
            *s.storage_gen.entry(grid).or_insert(0) += 1;
        }
    });
    result
}

/// `native_right_click_block`: a player using a terminal becomes its viewer; a charger takes the crystal in
/// the hand, or gives back what it holds.
pub fn right_click(game: &mut Game<'_>, pos: Pos, face: Pos, canceled: &mut bool) {
    let block = block_id(game, pos);
    // The face the player clicked: where a block placed by this click goes, and what it was placed against.
    if let Some(player) = game.native_acting_player() {
        let enchants = game.held().and_then(|h| h.extra.as_ref()).map(|d| d.enchantments.clone()).unwrap_or_default();
        SERVER.with_borrow_mut(|s| {
            s.last_click.insert(player.uuid, (pos, face));
            s.last_enchants.insert(player.uuid, enchants);
        });
    }
    // AE2's machines with no screen (onActivated passes): the click goes on to the held item, a block placed
    // against them as against stone (Horizon would say the barrel they copy "is not available yet").
    if matches!(ae2_name(&block), Some("growth_accelerator" | "crystal_resonance_generator" | "quantum_ring" | "spatial_pylon"))
        && !sneaking(game)
    {
        *canceled = true;
        place_held_against(game, pos, face);
        return;
    }
    // AE2's tools are checked before the block opens its menu.
    if tool_click(game, pos, &block) {
        *canceled = true;
        return;
    }
    // AbstractCraftingUnitBlock.use: only a formed, active cluster opens its screen.
    if is_crafting(&block)
        && !sneaking(game)
        && !(FORMED.with_borrow(|f| f.contains(&pos)) && SERVER.with_borrow(|s| s.grid_of.get(&pos).and_then(|g| s.grids.get(*g)).is_some_and(|g| g.online && g.cpu_of.contains_key(&pos))))
    {
        *canceled = true;
        place_held_against(game, pos, face);
        return;
    }
    if monitor_click(game, pos) {
        *canceled = true;
        return;
    }
    if block == "ae2:chest" && chest_top_click(game, pos, face) {
        *canceled = true;
        return;
    }
    if cable(&block).is_some() && facade_click(game, pos, face) {
        *canceled = true;
        return;
    }
    if block == "ae2:tiny_tnt" && game.held().is_some_and(|h| &*h.item == "minecraft:flint_and_steel") {
        *canceled = true;
        let creative = game.native_acting_player().is_some_and(|p| p.game_mode == 1);
        if !creative && let Some(h) = game.native_carried_stack(game.selected as i32) {
            // ItemStack.hurtAndBreak(1): flint and steel lasts 64 uses.
            h.damage += 1;
            if h.damage >= 64 {
                h.count = 0;
            }
        }
        SERVER.with_borrow_mut(|s| s.tnt_primes.push(pos));
        return;
    }
    if block == "ae2:sky_stone_tank" && !sneaking(game) {
        *canceled = true;
        if !tank_bucket(game, pos) {
            place_held_against(game, pos, face);
        }
        return;
    }
    if block == "ae2:crank" {
        *canceled = true;
        crank_turn(game, pos);
        return;
    }
    if block == "ae2:charger" {
        *canceled = true;
        let held = game.held().cloned();
        let inside = game.native_with_mod_block_entity(pos, |e| e.items.first().cloned().flatten()).flatten();
        match (inside, held) {
            (Some(stack), _) => {
                game.native_with_mod_block_entity(pos, |e| {
                    if let Some(slot) = e.items.first_mut() {
                        *slot = None;
                    }
                });
                if let Some(player) = game.native_acting_player()
                    && let Some(left) = game.native_give_stack(player.uuid, stack)
                {
                    game.native_with_mod_block_entity(pos, |e| {
                        if let Some(slot) = e.items.first_mut() {
                            *slot = Some(left);
                        }
                    });
                }
            }
            // ChargerBlock.onActivated: a charger recipe's input or a powered item goes in.
            (None, Some(hand)) if charger_result(&hand.item).is_some() || item_battery(&hand.item).is_some() => {
                let mut one = hand.clone();
                one.count = 1;
                let placed = game
                    .native_with_mod_block_entity(pos, |e| match e.items.first_mut() {
                        Some(slot @ None) => {
                            *slot = Some(one);
                            true
                        }
                        _ => false,
                    })
                    .unwrap_or(false);
                if placed {
                    let held = game.held_mut();
                    if let Some(h) = held.as_mut() {
                        h.count -= 1;
                        if h.count == 0 {
                            *held = None;
                        }
                    }
                }
            }
            _ => {}
        }
        return;
    }
    if !matches!(canon(&block).as_str(), "ae2:spatial_io_port" | "ae2:terminal" | "ae2:crafting_terminal" | "ae2:molecular_assembler" | "ae2:pattern_encoding_terminal" | "ae2:pattern_access_terminal" | "ae2:interface" | "ae2:level_emitter"
        | "ae2:energy_level_emitter" | "ae2:drive" | "ae2:chest" | "ae2:storage_bus" | "ae2:import_bus" | "ae2:export_bus" | "ae2:formation_plane" | "ae2:cell_workbench" | "ae2:condenser" | "ae2:quantum_link")
        && !ae2_name(&block).is_some_and(is_info_block)
        && !is_crafting(&block)
    {
        return;
    }
    if let Some(player) = game.native_acting_player() {
        SERVER.with_borrow_mut(|s| {
            s.viewers.insert(player.uuid, pos);
            s.sent.remove(&player.uuid);
            s.provider_lists.remove(&player.uuid);
        });
        // The list goes now, not at the next half second: the screen opens with its items.
        stream_terminals(game);
    }
}

/// The network's items, most first: what a terminal shows.
fn terminal_items(game: &mut Game<'_>, grid: usize) -> Vec<(AeKey, i64)> {
    let mut items: Vec<(AeKey, i64)> =
        with_storage(game, grid, None, |st| st.contents().iter().map(|(k, a)| (k.clone(), a)).filter(|(_, a)| *a > 0).collect());
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.id().cmp(b.0.id())));
    // What the network's patterns make and it doesn't hold, after the rest, with 0: AE2's "craftable".
    let craftable: Vec<AeKey> = SERVER.with_borrow(|s| {
        s.grids.get(grid).map_or(Vec::new(), |g| g.patterns.iter().flat_map(|(_, p)| p.outputs.iter().map(|o| o.what.clone())).collect())
    });
    let mut seen: HashSet<AeKey> = items.iter().map(|(k, _)| k.clone()).collect();
    for key in craftable.into_iter().chain(emitable_keys(game, grid)) {
        if seen.insert(key.clone()) {
            items.push((key, 0));
        }
    }
    items
}

/// Each viewer's terminal's items, when they changed: `seq`, then one `id amount` line per item.
fn stream_terminals(game: &mut Game<'_>) {
    let viewers: Vec<(u128, Pos)> = SERVER.with_borrow(|s| s.viewers.iter().map(|(u, p)| (*u, *p)).collect());
    let tick = SERVER.with_borrow(|s| s.tick);
    for (uuid, pos) in viewers {
        let grid = SERVER.with_borrow(|s| s.grid_of.get(&pos).copied());
        let online = grid.is_some_and(|g| SERVER.with_borrow(|s| s.grids.get(g).is_some_and(|x| x.online && x.active.contains(&pos))));
        let block = block_id(game, pos);
        // The small state, whole, when it changed.
        let mut extras = String::new();
        if block == "ae2:molecular_assembler" {
            // The assembler's screen: its nine grid slots ("minecraft:air 0" when empty), then what it makes.
            let job = SERVER.with_borrow(|s| s.assembler_jobs.get(&pos).map(|(_, _, out, grid)| (out.clone(), grid.clone())));
            let (out, grid) = job.unwrap_or_default();
            for i in 0..9 {
                let (id, n) = grid.get(i).cloned().unwrap_or(("minecraft:air".into(), 0));
                extras.push_str(&format!("#a {id} {n}\n"));
            }
            extras.push_str(&format!("#a {} 1\n", if out.is_empty() { "minecraft:air" } else { &out }));
        } else {
            if let Some(g) = grid {
                extras.push_str(&crafting_status(game, g));
                // A crafting block's own screen shows its cluster's CPU.
                if is_crafting(&block)
                    && let Some(i) = SERVER.with_borrow(|s| {
                        let info = s.grids.get(g)?;
                        let key = info.cpu_of.get(&pos)?;
                        info.cpus.iter().position(|c| c.0 == *key)
                    })
                {
                    extras.push_str(&format!("#B {i}\n"));
                }
            }
            if block == "ae2:spatial_io_port"
                && let Some(g) = grid
            {
                let (stored, cap) = SERVER.with_borrow(|s| {
                    s.grids.get(g).map_or((0.0, 0.0), |info| {
                        let root = info.nodes.iter().min().copied();
                        (root.and_then(|r| s.energy.get(&r).copied()).unwrap_or(0.0), grid_capacity(info, &s.devices))
                    })
                });
                extras.push_str(&format!("#d Stored Power: {} AE\n#d Max Power: {} AE\n", readable_number::format_i64(stored as i64, 6), readable_number::format_i64(cap as i64, 6)));
                if let Some((lo, hi)) = spatial_region(game, g) {
                    let (required, efficiency) = spatial_power(game, g, lo, hi);
                    extras.push_str(&format!("#d Required Power: {} AE\n#d Efficiency: {:.2}%\n", readable_number::format_i64(required as i64, 6), efficiency * 100.0));
                }
            }
            if has_priority(&block) || canon(&block) == "ae2:pattern_provider" {
                extras.push_str(&format!("#p {}\n", priority_of(game, pos)));
            }
            if block == "ae2:storage_bus"
                && let Some(target) = SERVER.with_borrow(|s| s.grids.iter().find_map(|g| g.storage_buses.iter().find(|(b, _)| *b == pos).map(|(_, t)| *t)))
            {
                extras.push_str(&format!("#t {}\n", block_id(game, target)));
            }
            if block == "ae2:condenser" {
                let v = game.native_with_mod_block_entity(pos, |e| {
                    let mode = nbt_i64(&e.nbt, "mode");
                    [nbt_i64(&e.nbt, "power"), nbt_i64(&e.nbt, "storage"), [0, 256, 256_000][mode.clamp(0, 2) as usize], mode]
                });
                if let Some([a, b, c, d]) = v {
                    extras.push_str(&format!("#c {a} {b} {c} {d}\n"));
                }
            }
            if matches!(block.as_str(), "ae2:level_emitter" | "ae2:energy_level_emitter") {
                let below = game.native_with_mod_block_entity(pos, |e| nbt_i64(&e.nbt, "below")).unwrap_or(0);
                extras.push_str(&format!("#e {below}\n"));
            }
            if block == "ae2:energy_level_emitter" {
                let level = game.native_with_mod_block_entity(pos, |e| nbt_i64(&e.nbt, "level")).unwrap_or(0);
                extras.push_str(&format!("#l {level}\n"));
            }
            if matches!(block.as_str(), "ae2:import_bus" | "ae2:export_bus" | "ae2:storage_bus" | "ae2:formation_plane") {
                let n = if matches!(block.as_str(), "ae2:storage_bus" | "ae2:formation_plane") { 63 } else { 9 };
                let cells = game.native_with_mod_block_entity(pos, |e| ghosts_n(e, n)).unwrap_or_default();
                for g in cells {
                    match g {
                        Some((id, n)) => extras.push_str(&format!("#g {id} {n}\n")),
                        None => extras.push_str("#g minecraft:air 0\n"),
                    }
                }
            }
            if block == "ae2:cell_workbench" {
                let config = game.native_with_mod_block_entity(pos, |e| wb_config(e)).unwrap_or_default();
                for k in config {
                    let id = key_from_string(&k).map_or("minecraft:air".to_owned(), |k| wire_id(&k));
                    extras.push_str(&format!("#g {id} 1\n"));
                }
            }
            if block == "ae2:level_emitter" {
                let (ghost, level) = game
                    .native_with_mod_block_entity(pos, |e| (ghosts_of(e).into_iter().next().flatten(), nbt_i64(&e.nbt, "level")))
                    .unwrap_or_default();
                let text = ghost.as_ref().map_or(level.to_string(), |(id, _)| amount_text(id, level));
                match ghost {
                    Some((id, n)) => extras.push_str(&format!("#g {id} {n}\n")),
                    None => extras.push_str("#g minecraft:air 0\n"),
                }
                extras.push_str(&format!("#l {text}\n"));
            }
            if ae2_name(&block).is_some_and(is_info_block) {
                for line in describe(game, pos) {
                    extras.push_str(&format!("#d {line}\n"));
                }
            }
            if canon(&block) == "ae2:interface" {
                let ghosts = game.native_with_mod_block_entity(pos, |e| ghosts_of(e)).unwrap_or_default();
                for g in ghosts.iter().take(9) {
                    match g {
                        Some((id, n)) => extras.push_str(&format!("#g {id} {n}\n")),
                        None => extras.push_str("#g minecraft:air 0\n"),
                    }
                }
            }
            if block == "ae2:pattern_encoding_terminal" {
                let (ghosts, mode, pick) = game
                    .native_with_mod_block_entity(pos, |e| (enc_ghosts(e), nbt_i64(&e.nbt, "mode"), nbt_i64(&e.nbt, "stonecut")))
                    .unwrap_or_default();
                let preview = match mode {
                    0 => craft_preview(game, &ghosts),
                    2 => smithing_preview(game, &ghosts).map(|s| (s.item.to_string(), i64::from(s.count))),
                    _ => None,
                };
                if mode == 3 {
                    for o in stonecut_options(game, &ghosts) {
                        extras.push_str(&format!("#o {} {}\n", o.item, o.count));
                    }
                    extras.push_str(&format!("#s {pick}\n"));
                }
                for g in &ghosts {
                    match g {
                        Some((id, n)) => extras.push_str(&format!("#g {id} {n}\n")),
                        None => extras.push_str("#g minecraft:air 0\n"),
                    }
                }
                if let Some((id, n)) = preview {
                    extras.push_str(&format!("#r {id} {n}\n"));
                }
            }
        }
        extras.push_str("#end\n");
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            extras.hash(&mut h);
            h.finish()
        };
        if SERVER.with_borrow(|s| s.sent.get(&uuid).is_none_or(|v| v.extras != hash))
            && game.native_net_send_to_player(uuid, NET_EXTRAS, extras.as_bytes()).is_ok()
        {
            SERVER.with_borrow_mut(|s| s.sent.entry(uuid).or_default().extras = hash);
        }
        if block == "ae2:molecular_assembler" {
            continue;
        }
        if block == "ae2:pattern_access_terminal" {
            let show = game.native_with_mod_block_entity(pos, |e| nbt_i64(&e.nbt, "show")).unwrap_or(0);
            stream_providers(game, uuid, grid.filter(|_| online), show);
            continue;
        }
        // Only terminals (and wireless terminals, through their access point) list the network's items.
        let chest = SERVER.with_borrow(|s| s.chest_terms.contains(&uuid));
        if !matches!(block.as_str(), "ae2:terminal" | "ae2:crafting_terminal" | "ae2:pattern_encoding_terminal" | "ae2:wireless_access_point")
            && !(chest && block == "ae2:chest")
        {
            continue;
        }
        // The items: only what changed since this viewer's last list, when the grid's storage changed (and every
        // two seconds for what crafting patterns add).
        // (A change of view cells sends the list again too.)
        let generation = grid.and_then(|g| SERVER.with_borrow(|s| s.storage_gen.get(&g).copied())).unwrap_or(0) ^ view_sig(game, pos, &block);
        let due = chest || SERVER.with_borrow(|s| s.sent.get(&uuid).is_none_or(|v| v.generation != generation || v.keys.is_empty() || tick >= v.full_after));
        if due {
            let items = match grid {
                Some(_) if online && chest => chest_items(game, pos),
                Some(g) if online => {
                    let items = terminal_items(game, g);
                    view_filter(game, pos, &block, items)
                }
                _ => Vec::new(),
            };
            SERVER.with_borrow_mut(|s| {
                let v = s.sent.entry(uuid).or_default();
                let fresh = v.keys.is_empty() && v.pending.is_empty();
                let mut lines: Vec<String> = Vec::new();
                if fresh {
                    lines.push("#reset".to_owned());
                }
                let mut now: HashSet<usize> = HashSet::with_capacity(items.len());
                for (key, amount) in items {
                    match v.index.get(&key) {
                        Some(&i) => {
                            now.insert(i);
                            if v.keys[i].1 != amount {
                                v.keys[i].1 = amount;
                                lines.push(format!("{i} {amount}"));
                            }
                        }
                        None => {
                            let i = v.free.pop().unwrap_or_else(|| {
                                v.keys.push((key.clone(), -1));
                                v.keys.len() - 1
                            });
                            lines.push(format!("{i} {} {amount}", wire_id(&key)));
                            v.keys[i] = (key.clone(), amount);
                            v.index.insert(key, i);
                            now.insert(i);
                        }
                    }
                }
                for i in 0..v.keys.len() {
                    if v.keys[i].1 >= 0 && !now.contains(&i) {
                        let key = v.keys[i].0.clone();
                        v.index.remove(&key);
                        v.keys[i].1 = -1;
                        v.free.push(i);
                        lines.push(format!("{i} -1"));
                    }
                }
                // At most 29 000 bytes a message.
                let mut body = String::new();
                for line in lines {
                    if body.len() + line.len() + 1 > 29_000 {
                        v.pending.push_back(std::mem::take(&mut body).into_bytes());
                    }
                    body.push_str(&line);
                    body.push('\n');
                }
                if !body.is_empty() {
                    v.pending.push_back(body.into_bytes());
                }
                v.generation = generation;
                v.full_after = tick + 40;
            });
        }
        // Send what is waiting, a few messages at a time.
        for _ in 0..MESSAGES_PER_STREAM {
            let Some(msg) = SERVER.with_borrow_mut(|s| s.sent.get_mut(&uuid).and_then(|v| v.pending.pop_front())) else { break };
            if game.native_net_send_to_player(uuid, NET_ITEMS, &msg).is_err() {
                SERVER.with_borrow_mut(|s| {
                    if let Some(v) = s.sent.get_mut(&uuid) {
                        v.pending.push_front(msg);
                    }
                });
                break;
            }
        }
    }
}

/// The machine a pattern provider works for, by name (AE2 groups the pattern access terminal by it): the first
/// block beside it that has a block entity and isn't AE2's network (a molecular assembler counts, any mod's
/// machine too), else the provider itself.
fn provider_target(game: &Game<'_>, pos: Pos) -> String {
    for d in SIDES {
        let q = add(pos, d);
        let b = block_id(game, q);
        let machine = b == "ae2:molecular_assembler"
            || (!b.starts_with("ae2:") && game.world.block_entities.contains_key(&(q[0], q[1], q[2])));
        if machine {
            return item_words(&b);
        }
    }
    "ME Pattern Provider".to_owned()
}

/// AE2's pattern access terminal (`PatternAccessTermMenu`): every pattern provider of the grid, grouped by what
/// it works for, each slot's pattern shown as what it makes. Sent whole (in pieces) when it changes.
fn stream_providers(game: &mut Game<'_>, uuid: u128, grid: Option<usize>, show: i64) {
    let mut providers: Vec<(String, Pos, Vec<String>)> = Vec::new();
    if let Some(g) = grid {
        let nodes: Vec<Pos> = SERVER.with_borrow(|s| s.grids.get(g).map(|g| g.nodes.iter().copied().collect()).unwrap_or_default());
        for p in nodes {
            if canon(&block_id(game, p)) != "ae2:pattern_provider" {
                continue;
            }
            // Settings.PATTERN_ACCESS_TERMINAL: NO hides it, unless the terminal shows them all.
            if show != 2 && game.native_with_mod_block_entity(p, |e| nbt_i64(&e.nbt, "hide")).unwrap_or(0) != 0 {
                continue;
            }
            let items = game.native_with_mod_block_entity(p, |e| e.items.clone()).unwrap_or_default();
            // ShowPatternProviders.NOT_FULL.
            if show == 1 && (0..9).all(|i| items.get(i).is_some_and(Option::is_some)) {
                continue;
            }
            let slots: Vec<String> = (0..9)
                .map(|i| match items.get(i).cloned().flatten() {
                    Some(stack) => decode_pattern(game, &stack)
                        .map(|pat| wire_id(&pat.primary_output().what))
                        .unwrap_or_else(|| stack.item.to_string()),
                    None => "-".to_owned(),
                })
                .collect();
            providers.push((provider_target(game, p), p, slots));
        }
    }
    providers.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut text = String::new();
    let mut last = String::new();
    for (name, _, slots) in &providers {
        if *name != last {
            let count = providers.iter().filter(|p| p.0 == *name).count();
            text.push_str(&format!("#g {name}|{count}\n"));
            last = name.clone();
        }
        text.push_str(&format!("p {}\n", slots.join(" ")));
    }
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        h.finish()
    };
    if SERVER.with_borrow(|s| s.provider_lists.get(&uuid).is_some_and(|l| l.hash == hash)) {
        return;
    }
    // In pieces of at most 29 000 bytes, the first starting the list over.
    let mut pieces = vec![String::from("#reset\n")];
    for line in text.lines() {
        if pieces.last().is_some_and(|p| p.len() + line.len() + 1 > 29_000) {
            pieces.push(String::new());
        }
        let last = pieces.last_mut().unwrap();
        last.push_str(line);
        last.push('\n');
    }
    for piece in pieces {
        if game.native_net_send_to_player(uuid, NET_PROVIDERS, piece.as_bytes()).is_err() {
            return;
        }
    }
    SERVER.with_borrow_mut(|s| {
        s.provider_lists.insert(uuid, ProviderList { hash, providers: providers.iter().map(|p| p.1).collect() });
    });
}

/// A click on a pattern access terminal's slot: the pattern on the cursor goes into an empty slot; an empty
/// cursor takes the pattern there (shift: into the inventory), as AE2's does.
fn provider_click(game: &mut Game<'_>, pos: Pos, provider: usize, slot: usize, shift: bool, carried: &mut Option<ItemStack>) {
    let Some(uuid) = menu_player(pos) else { return };
    let Some(target) = SERVER.with_borrow(|s| s.provider_lists.get(&uuid).and_then(|l| l.providers.get(provider).copied())) else { return };
    if slot >= 9 || canon(&block_id(game, target)) != "ae2:pattern_provider" {
        return;
    }
    let holding_pattern = carried.as_ref().is_some_and(|c| matches!(&*c.item, "ae2:crafting_pattern" | "ae2:processing_pattern" | "ae2:smithing_table_pattern" | "ae2:stonecutting_pattern"));
    let taken = game
        .native_with_mod_block_entity(target, |e| {
            let cell = e.items.get_mut(slot)?;
            if holding_pattern && cell.is_none() {
                let mut one = carried.clone()?;
                one.count = 1;
                *cell = Some(one);
                if let Some(c) = carried.as_mut() {
                    c.count -= 1;
                    if c.count == 0 {
                        *carried = None;
                    }
                }
                None
            } else if carried.is_none() {
                cell.take()
            } else {
                None
            }
        })
        .flatten();
    if let Some(stack) = taken {
        if shift {
            if let Some(left) = game.native_give_stack(uuid, stack) {
                *carried = Some(left);
            }
        } else {
            *carried = Some(stack);
        }
    }
    // The terminal shows it at once.
    SERVER.with_borrow_mut(|s| {
        if let Some(l) = s.provider_lists.get_mut(&uuid) {
            l.hash = 0;
        }
    });
}

/// The wireless terminals.
fn is_wireless_terminal(item: &str) -> bool {
    matches!(item, "ae2:wireless_terminal" | "ae2:wireless_crafting_terminal")
}

/// An access point's range in blocks (AE2: 16 + boosters ^ 1.5).
fn wap_range(entity: &ModBlockEntity) -> f64 {
    let boosters = entity.items.first().and_then(|s| s.as_ref()).filter(|s| &*s.item == "ae2:wireless_booster").map_or(0, |s| s.count);
    16.0 + f64::from(boosters).powf(1.5)
}

/// The access point a wireless terminal is linked to (written on it as AE2's link).
fn set_link(stack: &mut ItemStack, pos: Pos) {
    let extra = stack.extra.get_or_insert_with(Default::default);
    let mut data = match extra.other.iter().position(|(k, _)| k == CELL_DATA) {
        Some(i) => match extra.other.remove(i).1 {
            Tag::Compound(c) => c,
            _ => Compound::new(),
        },
        None => Compound::new(),
    };
    data.insert(java("ae2:link"), Tag::IntArray(pos.to_vec()));
    extra.other.push((CELL_DATA.to_owned(), Tag::Compound(data)));
    extra.other.sort_by(|a, b| a.0.cmp(&b.0));
}

fn link_of(stack: &ItemStack) -> Option<Pos> {
    let Some(Tag::Compound(data)) = stack.extra.as_ref()?.other.iter().find(|(k, _)| k == CELL_DATA).map(|(_, t)| t) else {
        return None;
    };
    match data.get(&java("ae2:link")) {
        Some(Tag::IntArray(v)) if v.len() == 3 => Some([v[0], v[1], v[2]]),
        _ => None,
    }
}

/// The wireless terminal's messages: server to client "open", client to server "take i right shift", "insert
/// slot", "close".
pub const NET_WIRELESS: &str = "ae2:wireless";

/// `native_use_item`: a linked wireless terminal in range of its access point opens the network's terminal.
pub fn use_item(game: &mut Game<'_>, stack: &ItemStack, claimed: &mut bool) {
    // AE2's portable cells: sneak-use on an empty one takes it apart into its recipe (ME chest, component, energy
    // cell, housing), its charge going back into the energy cell.
    if let Some(name) = ae2_name(&stack.item).map(str::to_owned)
        && portable_type(&stack.item).is_some()
        && sneaking(game)
    {
        *claimed = true;
        if read_cell(stack).item_count > 0 {
            return;
        }
        let Some(player) = game.native_acting_player() else { return };
        let (housing, size) = match name.strip_prefix("portable_item_cell_") {
            Some(size) => ("ae2:item_cell_housing", size.to_owned()),
            None => ("ae2:fluid_cell_housing", name.trim_start_matches("portable_fluid_cell_").to_owned()),
        };
        let power = portable_power(stack);
        if let Some(held) = game.native_carried_stack(game.selected as i32) {
            held.count -= 1;
        }
        let mut cell = ItemStack::new("ae2:energy_cell", 1);
        if power > 0.0 {
            set_custom(&mut cell, "internalCurrentPower", Tag::Double(power.to_bits()));
        }
        for part in [ItemStack::new("ae2:chest", 1), ItemStack::new(&format!("ae2:cell_component_{size}"), 1), cell, ItemStack::new(housing, 1)] {
            let _ = game.native_give_stack(player.uuid, part);
        }
        return;
    }
    if tool_use_air(game, stack) || open_item_screen(game, stack) {
        *claimed = true;
        return;
    }
    // A storage cell in the hand does nothing, but an empty one taken apart while sneaking gives back its housing
    // and component (AE2's `disassembleDrive`).
    if let Some(name) = ae2_name(&stack.item).map(str::to_owned)
        && cell_type(&stack.item).is_some()
    {
        *claimed = true;
        if !sneaking(game) || read_cell(stack).item_count > 0 {
            return;
        }
        let Some(player) = game.native_acting_player() else { return };
        let (housing, size) = match name.strip_prefix("item_storage_cell_") {
            Some(size) => ("ae2:item_cell_housing", size.to_owned()),
            None => ("ae2:fluid_cell_housing", name.trim_start_matches("fluid_storage_cell_").to_owned()),
        };
        if let Some(held) = game.native_carried_stack(game.selected as i32) {
            held.count -= 1;
        }
        let _ = game.native_give_stack(player.uuid, ItemStack::new(housing, 1));
        let _ = game.native_give_stack(player.uuid, ItemStack::new(&format!("ae2:cell_component_{size}"), 1));
        return;
    }
    if !is_wireless_terminal(&stack.item) {
        return;
    }
    *claimed = true;
    let Some(player) = game.native_acting_player() else { return };
    let say = |game: &mut Game<'_>, t: &str| {
        game.native_action_bar(player.uuid, horizon_client::text::Text::legacy(t));
    };
    // WirelessTerminalItem.checkPreconditions, then WirelessTerminalMenuHost's range check (AE2's messages).
    let Some(wap) = link_of(stack) else {
        say(game, "Device is not linked.");
        return;
    };
    if player.game_mode != 1 && portable_power(stack) < 0.5 {
        say(game, "Device is low on power.");
        return;
    }
    if block_id(game, wap) != "ae2:wireless_access_point" {
        say(game, "Linked network cannot be found");
        return;
    }
    let range = game.native_with_mod_block_entity(wap, |e| wap_range(e)).unwrap_or(16.0);
    let d = ((player.position[0] - wap[0] as f64 - 0.5).powi(2) + (player.position[1] - wap[1] as f64 - 0.5).powi(2) + (player.position[2] - wap[2] as f64 - 0.5).powi(2)).sqrt();
    let online = SERVER.with_borrow(|s| s.grid_of.get(&wap).and_then(|g| s.grids.get(*g)).is_some_and(|g| g.online && g.active.contains(&wap)));
    if d > range || !online {
        say(game, "Wireless Out Of Range.");
        return;
    }
    let crafting = &*stack.item == "ae2:wireless_crafting_terminal";
    let slot = game.selected as i32;
    SERVER.with_borrow_mut(|s| {
        s.viewers.insert(player.uuid, wap);
        s.sent.remove(&player.uuid);
        s.wireless.insert(player.uuid);
        s.wcraft_slot.insert(player.uuid, slot);
    });
    let _ = game.native_net_send_to_player(player.uuid, NET_WIRELESS, if crafting { b"open craft" } else { b"open" });
}

/// `native_inventory_tick`: an inventory slot the wireless terminal asked to put in the network.
pub fn inventory_tick(game: &mut Game<'_>, slot: i32) {
    if let Some(s) = game.native_carried_stack(slot) {
        give_frequency(s);
    }
    inventory_tick_rest(game, slot);
}

/// Stacks a terminal took out for a player (shift-click, space-click), into their inventory from the server
/// tick (a menu button reaches no player to give to); what doesn't fit goes back to the network.
fn deliver_gives(game: &mut Game<'_>) {
    for (uuid, grid, stack) in SERVER.with_borrow_mut(|s| std::mem::take(&mut s.pending_gives)) {
        if let Some(left) = game.native_give_stack(uuid, stack) {
            let key = key_of(game, &left);
            with_storage(game, grid, None, |st| st.insert(&key, i64::from(left.count), Actionable::Modulate));
        }
    }
}

fn inventory_tick_rest(game: &mut Game<'_>, slot: i32) {
    item_screen_tick(game, slot);
    let Some(player) = game.native_acting_player() else { return };
    toolbelt_stream(game, player.uuid);
    // The "+" on a crafting terminal: what the network hadn't, from this player's inventory.
    let fills: Vec<(Pos, usize, Vec<String>)> = SERVER.with_borrow_mut(|s| {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut s.terminal_fills).into_iter().partition(|f| f.0 == player.uuid);
        s.terminal_fills = rest;
        mine.into_iter().map(|(_, p, c, ids)| (p, c, ids)).collect()
    });
    for (pos, c, ids) in fills {
        for id in &ids {
            if game.native_take_items(id, 1) == 1 {
                game.native_with_mod_block_entity(pos, |e| {
                    if let Some(cell) = e.items.get_mut(CT_GRID.start + c) {
                        *cell = Some(ItemStack::new(id, 1));
                    }
                });
                break;
            }
        }
    }
    compass_tick(game, slot, player.uuid, player.yaw, player.position);
    // The terminal's own slot (only filled slots tick): the grid's clicks, with the inventory to hand.
    if SERVER.with_borrow(|s| s.wcraft_slot.get(&player.uuid) == Some(&slot)) {
        wcraft_ops(game, player.uuid);
    }
    wireless_drain(game, player.uuid, player.game_mode == 1, player.position, slot);
    let wanted = SERVER.with_borrow_mut(|s| {
        let i = s.wireless_inserts.iter().position(|(u, sl)| *u == player.uuid && *sl == slot)?;
        Some(s.wireless_inserts.remove(i))
    });
    if wanted.is_none() {
        return;
    }
    let Some(stack) = game.native_carried_stack(slot).map(|s| s.clone()) else { return };
    let key = key_of(game, &stack);
    let Some(stored) = with_viewer_storage(game, player.uuid, |st| st.insert(&key, i64::from(stack.count), Actionable::Modulate)) else { return };
    if let Some(s) = game.native_carried_stack(slot) {
        s.count -= stored as u32;
    }
}

/// AE2's interface (`InterfaceLogic`): each storage slot is kept at what its config cell asks (from the network,
/// what's over going back); a slot with no config gives everything back to the network.
fn interface_tick(game: &mut Game<'_>, grid: usize, entity: &mut ModBlockEntity) {
    if entity.items.len() < 9 {
        return;
    }
    let config = ghosts_of(entity);
    for i in 0..9 {
        let want = config.get(i).cloned().flatten();
        let have = entity.items[i].clone();
        // What is there and shouldn't be: back to the network.
        if let Some(st) = &have {
            let keep = match &want {
                Some((id, n)) if *st.item == **id => (*n).min(i64::from(max_stack(game, id))),
                _ => 0,
            };
            let extra = i64::from(st.count) - keep;
            if extra > 0 {
                let key = key_of(game, st);
                let back = with_storage(game, grid, None, |s| s.insert(&key, extra, Actionable::Modulate));
                if let Some(slot) = entity.items[i].as_mut() {
                    slot.count -= back as u32;
                    if slot.count == 0 {
                        entity.items[i] = None;
                    }
                }
            }
        }
        // What's missing: from the network.
        if let Some((id, n)) = want {
            let n = n.min(i64::from(max_stack(game, &id)));
            let now = entity.items[i].as_ref().filter(|s| *s.item == *id).map_or(0, |s| i64::from(s.count));
            if now < n && entity.items[i].as_ref().is_none_or(|s| *s.item == *id) {
                let key = key_of(game, &ItemStack::new(&id, 1));
                // InterfaceLogic: with a Fuzzy Card, what matches the config in the set way, when the slot is free for it.
                let fuzzy = if cards(entity, 9, "ae2:fuzzy_card") > 0 && now == 0 {
                    let mode = fuzzy_mode_of(entity);
                    with_storage(game, grid, None, |s| s.contents().find_fuzzy(&key, mode)).into_iter().find(|(_, c)| *c > 0).map(|(k, _)| k)
                } else {
                    None
                };
                let take = fuzzy.unwrap_or_else(|| key.clone());
                let got = with_storage(game, grid, None, |s| s.extract(&take, n - now, Actionable::Modulate));
                if got > 0 {
                    match entity.items[i].as_mut() {
                        Some(slot) => slot.count += got as u32,
                        None => entity.items[i] = Some(stack_of(&take, got as u32)),
                    }
                }
                // InterfaceLogic.handleCrafting: with a Crafting Card, what the network hasn't is asked for (once a
                // second, and not while a job already makes it). It comes here through the network.
                if now + got < n && cards(entity, 9, "ae2:crafting_card") > 0 && SERVER.with_borrow(|s| s.tick % 20 == 0) {
                    let (busy, craftable) = SERVER.with_borrow(|s| {
                        s.grids.get(grid).map_or((true, false), |g| {
                            (
                                g.cpus.iter().any(|(c, _, _)| s.cpus.get(c).and_then(|cpu| cpu.job()).is_some_and(|j| j.final_output.what == key)),
                                g.patterns.iter().any(|(_, p)| p.outputs.iter().any(|o| o.what == key)),
                            )
                        })
                    });
                    if craftable && !busy {
                        request_craft(game, grid, 0, &key, n - now - got);
                    }
                }
            }
        }
    }
}

/// AE2's IO port: a cell in its input is emptied into the network (or, filling, filled from it), so many items a
/// tick (more with acceleration cards), then moves to the output.
fn io_port_tick(game: &mut Game<'_>, grid: usize, pos: Pos, entity: &mut ModBlockEntity) {
    use super::port_ae2_core::{CellState, KeyCounter};
    if entity.items.len() < 12 {
        return;
    }
    // A redstone card: the redstone mode (always, without signal, with signal, once per pulse).
    if cards(entity, 12, "ae2:redstone_card") > 0 {
        let dim = game.dim.id().to_owned();
        let on = game.native_neighbor_signal(&dim, pos) > 0;
        let was = nbt_i64(&entity.nbt, "rs") != 0;
        set_nbt_i64(&mut entity.nbt, "rs", i64::from(on));
        let enabled = match nbt_i64(&entity.nbt, "redstone") {
            1 => !on,
            2 => on,
            3 => on && !was,
            _ => true,
        };
        if !enabled {
            return;
        }
    }
    let fill = nbt_i64(&entity.nbt, "mode") == 1;
    let fullness = nbt_i64(&entity.nbt, "full");
    // IOPortBlockEntity.tickingRequest: 256 operations a tick, twice that a speed card (up to eight times),
    // shared by the six input cells in turn.
    let mut budget = 256i64 << cards(entity, 12, "ae2:speed_card").min(3);
    for slot in 0..6 {
        let Some(mut stack) = entity.items[slot].clone() else { continue };
        let Some(cell) = cell_type(&stack.item) else {
            io_port_move(entity, slot);
            continue;
        };
        let mut inv = BasicCellInventory::new(cell, &cell_config(&stack), cell_fuzzy(&stack), cell_upgrades(&stack), &read_cell(&stack));
        if budget > 0 {
            // transferContents, in one pass over the source: each key moves what the other side takes, at most
            // the budget left (in operations: a bucket's worth of a fluid is several).
            budget = with_storage(game, grid, None, |st| {
                let mut left = budget;
                let source: Vec<(AeKey, i64)> = if fill {
                    st.contents().iter().filter(|(_, n)| *n > 0).map(|(k, n)| (k.clone(), n)).collect()
                } else {
                    let mut held = KeyCounter::new();
                    inv.available_stacks(&mut held);
                    held.iter().filter(|(_, n)| *n > 0).map(|(k, n)| (k.clone(), n)).collect()
                };
                for (key, total) in source {
                    if left <= 0 {
                        break;
                    }
                    let per = key.key_type().amount_per_operation();
                    let possible = if fill { inv.insert(&key, total, Actionable::Simulate) } else { st.insert(&key, total, Actionable::Simulate) };
                    let possible = possible.min(left.saturating_mul(per));
                    if possible <= 0 {
                        continue;
                    }
                    let got = if fill { st.extract(&key, possible, Actionable::Modulate) } else { inv.extract(&key, possible, Actionable::Modulate) };
                    if got <= 0 {
                        continue;
                    }
                    let put = if fill { inv.insert(&key, got, Actionable::Modulate) } else { st.insert(&key, got, Actionable::Modulate) };
                    if put < got {
                        if fill {
                            st.insert(&key, got - put, Actionable::Modulate);
                        } else {
                            inv.insert(&key, got - put, Actionable::Modulate);
                        }
                    }
                    if put > 0 {
                        left -= (put / per).max(1);
                    }
                }
                left
            });
            if let Some(contents) = inv.persist() {
                write_cell(&mut stack, &contents);
            }
            entity.items[slot] = Some(stack);
        }
        // Done with this cell (budget left over): out when the fullness mode says so.
        let ready = match fullness {
            1 => true,
            2 => inv.status() == CellState::Full,
            _ => inv.status() == CellState::Empty,
        };
        if budget > 0 && ready {
            io_port_move(entity, slot);
        }
    }
}

/// IOPortBlockEntity.moveSlot: the input cell to the first free output slot.
fn io_port_move(entity: &mut ModBlockEntity, slot: usize) {
    if let Some(out) = (6..12).find(|&i| entity.items[i].is_none()) {
        entity.items[out] = entity.items[slot].take();
    }
}

/// The wireless terminal's screen (a mod screen: no menu): AE2's terminal, six rows, the player's inventory under
/// it. Top left, in GUI pixels.
const WIRELESS_ROWS: usize = 6;

fn wireless_origin(game: &Game<'_>) -> [f32; 2] {
    let (w, h) = game.gui_size();
    let height = if CLIENT.with_borrow(|c| c.wireless_craft) { 241.0 } else { 168.0 + 18.0 * (WIRELESS_ROWS - 3) as f32 };
    [((w - 195.0) / 2.0).floor(), ((h - height) / 2.0).floor()]
}

/// The wireless screen's network rows (the crafting terminal's three, the plain one's six) and where the
/// player's inventory starts, from the panel's top.
fn wireless_rows() -> (usize, f32) {
    if CLIENT.with_borrow(|c| c.wireless_craft) { (3, 159.0) } else { (WIRELESS_ROWS, 86.0 + 18.0 * (WIRELESS_ROWS - 3) as f32) }
}

/// `native_screen_render_post` on the wireless terminal's screen.
pub fn wireless_draw(game: &Game<'_>, batch: &mut HudBatch, mouse: [f32; 2]) {
    if game.native_mod_screen() != Some("ae2") || CLIENT.with_borrow(|c| c.wireless_open) != 2 {
        return;
    }
    let (rows, inv_top) = wireless_rows();
    let craft = CLIENT.with_borrow(|c| c.wireless_craft);
    CLIENT.with_borrow_mut(|c| c.rows = rows);
    let [ox, oy] = wireless_origin(game);
    if overlay_shown() {
        CLIENT.with_borrow_mut(|c| c.status_host = if craft { "ae2:wireless_crafting_terminal" } else { "ae2:wireless_terminal" }.to_owned());
        return;
    }
    screen_draw(game, if craft { "ae2:crafting_terminal" } else { "ae2:terminal" }, &[1], batch, [ox, oy]);
    // The player's inventory: three rows, then the hotbar.
    let inv_y = oy + inv_top;
    let mut hovered: Option<(ItemStack, Option<i64>)> = None;
    // The network grid's item under the pointer: the item's own tooltip and what the network holds of it.
    let [gx, gy] = [mouse[0] - ox - TERM_GRID[0], mouse[1] - oy - TERM_GRID[1]];
    if gx >= 0.0 && gy >= 0.0 && gx < TERM_COLS as f32 * 18.0 && gy < rows as f32 * 18.0 {
        let cell = CLIENT.with_borrow(|c| c.scroll * TERM_COLS) + (gy / 18.0) as usize * TERM_COLS + (gx / 18.0) as usize;
        hovered = CLIENT.with_borrow(|c| visible(c).get(cell).and_then(|&i| c.items.get(i)).map(|(id, n)| (ItemStack::new(&icon_of(id), 1), Some(*n))));
    }
    // The crafting grid (kept on the terminal) and what it makes.
    if craft {
        let grid = game.inventory.get(game.selected).cloned().flatten().map(|t| craft_grid(&t)).unwrap_or_default();
        for (i, st) in grid.iter().enumerate() {
            let (x, y) = (ox + CRAFT_GRID[0] + (i % 3) as f32 * 18.0, oy + CRAFT_GRID[1] + (i / 3) as f32 * 18.0);
            if let Some(st) = st {
                game.native_item(batch, &st.item, [x, y], 16.0);
                if st.count > 1 {
                    let l = st.count.to_string();
                    let w = game.native_text_width(&l);
                    game.native_text(batch, &l, [x + 17.0 - w, y + 9.0], 0xFFFFFF, true);
                }
                if mouse[0] >= x && mouse[0] < x + 16.0 && mouse[1] >= y && mouse[1] < y + 16.0 {
                    hovered = Some((st.clone(), None));
                }
            }
        }
        let size = grid.len().min(9);
        if size == 9
            && let Some((_, result)) = game.res.recipes().find(&grid, 3)
        {
            let (x, y) = (ox + CRAFT_RESULT[0], oy + CRAFT_RESULT[1]);
            game.native_item(batch, &result.item, [x, y], 16.0);
            if result.count > 1 {
                let l = result.count.to_string();
                let w = game.native_text_width(&l);
                game.native_text(batch, &l, [x + 17.0 - w, y + 9.0], 0xFFFFFF, true);
            }
            if mouse[0] >= x && mouse[0] < x + 16.0 && mouse[1] >= y && mouse[1] < y + 16.0 {
                hovered = Some((result, None));
            }
        }
    }
    game.native_text(batch, "Inventory", [ox + 8.0, inv_y - 11.0], 0x404040, false);
    for i in 0..36 {
        let (x, y) = if i < 9 { (ox + 8.0 + 18.0 * i as f32, inv_y + 58.0) } else { (ox + 8.0 + 18.0 * ((i - 9) % 9) as f32, inv_y + 18.0 * ((i - 9) / 9) as f32) };
        if let Some(Some(st)) = game.inventory.get(i) {
            if mouse[0] >= x && mouse[0] < x + 16.0 && mouse[1] >= y && mouse[1] < y + 16.0 {
                hovered = Some((st.clone(), None));
            }
            game.native_item(batch, &st.item, [x, y], 16.0);
            if st.count > 1 {
                let l = st.count.to_string();
                let w = game.native_text_width(&l);
                game.native_text(batch, &l, [x + 17.0 - w, y + 9.0], 0xFFFFFF, true);
            }
        }
    }
    if let Some((id, n)) = CLIENT.with_borrow(|c| c.wcursor.clone()) {
        let icon = icon_of(&id);
        game.native_item(batch, &icon, [mouse[0] - 8.0, mouse[1] - 8.0], 16.0);
        if n > 1 {
            let l = n.to_string();
            let w = game.native_text_width(&l);
            game.native_text(batch, &l, [mouse[0] + 9.0 - w, mouse[1] + 1.0], 0xFFFFFF, true);
        }
        hovered = None;
    }
    if let Some((stack, stored)) = hovered {
        super::port_ae2_guide::hovered(&stack.item);
        let mut lines = game.stack_tooltip(&stack);
        match stored {
            Some(0) => lines.push((horizon_client::text::Text::legacy("§7Craftable: click 1, right-click 16, shift 64"), u32::MAX)),
            Some(n) => lines.push((
                horizon_client::text::Text::legacy(&format!(
                    "§7Stored: {}",
                    super::port_ae2_core::KeyType::Items.format_amount(n, super::port_ae2_core::AmountFormat::Full)
                )),
                u32::MAX,
            )),
            None => {}
        }
        game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
    }
}

/// A click on the wireless terminal's screen: an item of the grid to take, or an inventory slot to put in.
pub fn wireless_click(game: &Game<'_>, mouse: [f32; 2], button: i32, canceled: &mut bool) {
    if game.native_mod_screen() != Some("ae2") || CLIENT.with_borrow(|c| c.wireless_open) != 2 {
        return;
    }
    *canceled = true;
    if wireless_jei_click(game, mouse, button) {
        return;
    }
    let [ox, oy] = wireless_origin(game);
    let at = [mouse[0] - ox, mouse[1] - oy];
    let right = button == 1;
    let shift = game.keys.contains(&winit::keyboard::KeyCode::ShiftLeft) || game.keys.contains(&winit::keyboard::KeyCode::ShiftRight);
    // The toolbar and search box work as on a terminal.
    let mut ignored = None;
    let [gx, gy] = [at[0] - TERM_GRID[0], at[1] - TERM_GRID[1]];
    let (rows, inv_top) = wireless_rows();
    let craft = CLIENT.with_borrow(|c| c.wireless_craft);
    let holding = CLIENT.with_borrow(|c| c.wcursor.is_some());
    if gx >= 0.0 && gy >= 0.0 && gx < TERM_COLS as f32 * 18.0 && gy < rows as f32 * 18.0 {
        // With a stack on the cursor, a click on the network's grid puts it in (one with the right button).
        if holding {
            CLIENT.with_borrow_mut(|c| c.wireless_out.push(format!("grid drop {}", u8::from(right))));
            return;
        }
        let cell = CLIENT.with_borrow(|c| c.scroll * TERM_COLS) + (gy / 18.0) as usize * TERM_COLS + (gx / 18.0) as usize;
        if let Some(index) = CLIENT.with_borrow(|c| visible(c).get(cell).copied()) {
            if CLIENT.with_borrow(|c| c.items.get(index).is_some_and(|(_, n)| *n == 0)) && ask_craft(index) {
                return;
            }
            CLIENT.with_borrow_mut(|c| c.wireless_out.push(format!("grid take {index} {} {}", u8::from(right), u8::from(shift))));
        }
        return;
    }
    screen_click(game, if craft { "ae2:crafting_terminal" } else { "ae2:terminal" }, at, right, &mut ignored);
    if craft {
        let r = u8::from(right);
        let s = u8::from(shift);
        let (gx, gy) = (at[0] - CRAFT_GRID[0], at[1] - CRAFT_GRID[1]);
        if (0.0..54.0).contains(&gx) && (0.0..54.0).contains(&gy) {
            let cell = (gy / 18.0) as usize * 3 + (gx / 18.0) as usize;
            CLIENT.with_borrow_mut(|c| c.wireless_out.push(format!("grid cell {cell} {r} {s}")));
            return;
        }
        if let Some(b) = stash_button(at, 3) {
            CLIENT.with_borrow_mut(|c| c.wireless_out.push(if b == CT_STASH { "grid stash" } else { "grid stashp" }.to_owned()));
            return;
        }
        if (0.0..16.0).contains(&(at[0] - CRAFT_RESULT[0])) && (0.0..16.0).contains(&(at[1] - CRAFT_RESULT[1])) {
            CLIENT.with_borrow_mut(|c| c.wireless_out.push(format!("grid result {s}")));
            return;
        }
    }
    let inv_y = inv_top;
    let (cx, cy) = ((at[0] - 8.0) / 18.0, at[1] - inv_y);
    if cx < 0.0 || cx >= 9.0 {
        return;
    }
    let slot = if (0.0..54.0).contains(&cy) {
        Some(9 + (cy / 18.0) as i32 * 9 + cx as i32)
    } else if (58.0..76.0).contains(&cy) {
        Some(cx as i32)
    } else {
        None
    };
    if let Some(slot) = slot {
        // A click picks up and puts down (shift still sends the stack to the network).
        if !shift {
            CLIENT.with_borrow_mut(|c| c.wireless_out.push(format!("grid inv {slot} {}", u8::from(right))));
            return;
        }
        CLIENT.with_borrow_mut(|c| c.wireless_out.push(format!("insert {slot}")));
    }
}

/// What a pattern's machines do now: its crafts a tick and the ticks a craft takes, from the providers that hold it
/// and are on the network as it stands (channels, channel mode, P2P all decide that) and their machines: the speed
/// cards in assemblers, a machine's own recipe time, or what that machine has been measured doing. None: nothing can.
fn pattern_machines(game: &Game<'_>, patterns: &[(Pos, Arc<Pattern>)], pattern: &Pattern) -> Option<(f64, f64)> {
    let mut rate = 0.0;
    let mut latency: f64 = 0.0;
    let processing = pattern.definition.id() == "ae2:processing_pattern";
    for (at, _) in patterns.iter().filter(|(_, p)| **p == *pattern) {
        for q in SIDES.iter().map(|d| add(*at, *d)) {
            let block = block_id(game, q);
            let ticks = if processing {
                if block.starts_with("ae2:") || !game.world.block_entities.contains_key(&(q[0], q[1], q[2])) {
                    continue;
                }
                let measured = SERVER.with_borrow(|s| s.machine_times.get(&q).map(|m| m.1)).filter(|m| *m > 0.0);
                let per = pattern.primary_output().amount.max(1) as f64;
                let ticks = measured.map(|m| m * per).unwrap_or(match block.as_str() {
                    "minecraft:blast_furnace" | "minecraft:smoker" => 100.0,
                    "minecraft:furnace" => 200.0,
                    _ => 40.0,
                });
                // `push_pattern` feeds the first machine it finds beside the provider.
                rate += 1.0 / ticks;
                latency = latency.max(ticks);
                break;
            } else if block == "ae2:molecular_assembler" {
                let speed = match game.world.block_entities.get(&(q[0], q[1], q[2])) {
                    Some(horizon_client::containers::BlockEntity::Mod(m)) => cards(m, 1, "ae2:speed_card"),
                    _ => 0,
                };
                // Its measured cycle at these cards (a craft also waits for the CPU's next push), else its craft time.
                SERVER.with_borrow(|s| s.assembler_cycles.get(&q).filter(|c| c.0 == speed).map(|c| c.2).filter(|c| *c > 0.0)).unwrap_or(ASSEMBLER_TICKS[speed.min(5)] as f64)
            } else {
                continue;
            };
            rate += 1.0 / ticks;
            latency = latency.max(ticks);
        }
    }
    (rate > 0.0).then_some((rate, latency))
}

/// A job's time left in seconds, worked out at most once a second and counted down between (the status screen can
/// ask each tick; nothing is worked out when no one has it open). -1 while nothing can make the job go on.
fn job_seconds_left(game: &Game<'_>, grid: usize, cpu: Pos) -> f64 {
    let tick = SERVER.with_borrow(|s| s.tick);
    if let Some((at, ticks, rate)) = SERVER.with_borrow(|s| s.forecasts.get(&cpu).copied())
        && tick.saturating_sub(at) < 20
    {
        return ticks.map_or(-1.0, |t| ((t - (tick - at) as f64).max(0.0)) / rate);
    }
    let patterns: Vec<(Pos, Arc<Pattern>)> = SERVER.with_borrow(|s| s.grids.get(grid).map(|g| g.patterns.clone()).unwrap_or_default());
    let ticks = SERVER.with_borrow(|s| {
        let c = s.cpus.get(&cpu)?;
        c.forecast(&mut |p| pattern_machines(game, &patterns, p), &mut |k| {
            patterns.iter().find_map(|(at, p)| {
                let out = p.outputs.iter().find(|o| o.what == *k)?;
                let (rate, latency) = pattern_machines(game, &patterns, p)?;
                // A machine's time since its last output is progress on the next.
                let since = SIDES.iter().map(|d| add(*at, *d)).filter_map(|q| s.machine_times.get(&q).map(|m| tick.saturating_sub(m.0) as f64)).fold(0.0, f64::max);
                Some((rate * out.amount as f64, since.min(latency)))
            }).unwrap_or((1.0, 0.0))
        })
    });
    // The real tick rate, measured over each forecast's second.
    let rate = SERVER.with_borrow_mut(|s| {
        let now = std::time::Instant::now();
        let (last_tick, last_time, ema) = s.tick_rate.unwrap_or((tick, now, 20.0));
        let secs = now.duration_since(last_time).as_secs_f64();
        let mut rate = ema;
        if secs >= 0.5 && tick > last_tick {
            let seen = (tick - last_tick) as f64 / secs;
            rate = (ema * 0.7 + seen * 0.3).clamp(1.0, 20.0);
            s.tick_rate = Some((tick, now, rate));
        } else if s.tick_rate.is_none() {
            s.tick_rate = Some((tick, now, rate));
        }
        s.forecasts.insert(cpu, (tick, ticks, rate));
        rate
    });
    ticks.map_or(-1.0, |t| t / rate)
}

/// AE2's crafting status (`CraftingStatusMenu`): each busy CPU of a grid, what it makes, how many are left, how
/// far along it is and the time left, one `#j` line each.
fn crafting_status(game: &Game<'_>, grid: usize) -> String {
    let keys: Vec<Pos> = SERVER.with_borrow(|s| s.grids.get(grid).map(|g| g.cpus.iter().map(|(k, _, _)| *k).collect()).unwrap_or_default());
    let modes: Vec<(Pos, i64)> = keys.iter().map(|k| (*k, provider_nbt(game, *k, "cpu_mode"))).collect();
    let mut lefts: Vec<(Pos, f64)> = keys.iter().filter(|k| SERVER.with_borrow(|s| s.cpus.get(*k).is_some_and(|c| c.job().is_some()))).map(|k| (*k, job_seconds_left(game, grid, *k))).collect();
    // CPUs run in order, and the first with work for a machine keeps it busy: a later job on the same patterns waits
    // for the earlier one to finish first.
    SERVER.with_borrow(|s| {
        let tasks: Vec<Vec<Arc<Pattern>>> = lefts.iter().map(|(k, _)| s.cpus.get(k).map_or(Vec::new(), |c| c.task_patterns())).collect();
        for later in 1..lefts.len() {
            let wait = (0..later)
                .filter(|&first| tasks[first].iter().any(|p| tasks[later].contains(p)) && lefts[first].1 >= 0.0)
                .map(|first| lefts[first].1)
                .fold(0.0, f64::max);
            if lefts[later].1 >= 0.0 {
                lefts[later].1 += wait;
            }
        }
    });
    SERVER.with_borrow(|s| {
        let Some(g) = s.grids.get(grid) else { return String::new() };
        let mut out = String::new();
        for (key, _, _) in &g.cpus {
            let Some(job) = s.cpus.get(key).and_then(|c| c.job()) else { continue };
            let progress = job.time.progress();
            let left = lefts.iter().find(|(k, _)| k == key).map_or(-1.0, |l| l.1);
            out.push_str(&format!(
                "#j {} {} {} {}\n",
                job.final_output.what.id(),
                job.remaining_amount,
                (progress * 100.0) as i64,
                left.round() as i64
            ));
        }
        // CraftingStatusMenu's CPU list, and each busy one's CraftingStatus entries: stored, active, scheduled.
        for (i, (key, bytes, accel)) in g.cpus.iter().enumerate() {
            let cpu = s.cpus.get(key);
            let job = cpu.and_then(|c| c.job());
            let item = job.map_or("-".to_owned(), |j| wire_id(&j.final_output.what));
            let amount = job.map_or(0, |j| j.final_output.amount);
            let permille = job.map_or(0, |j| (j.time.progress() * 1000.0) as i64);
            let elapsed = job.map_or(0, |j| (j.time.elapsed_nanos() / 1_000_000_000) as i64);
            let cant = cpu.is_some_and(|c| c.cant_store_items());
            let left = lefts.iter().find(|(k, _)| k == key).map_or(-1.0, |l| l.1);
            let mode = modes.iter().find(|(k, _)| k == key).map_or(0, |m| m.1);
            out.push_str(&format!("#C {i} {bytes} {accel} {mode} {item} {amount} {permille} {} {elapsed} {}\n", left.round() as i64, u8::from(cant)));
            if let Some(c) = cpu.filter(|c| c.has_job()) {
                let mut all = super::port_ae2_core::KeyCounter::new();
                c.all_items(&mut all);
                for (k, _) in all.iter().take(90) {
                    out.push_str(&format!("#E {i} {} {} {} {}\n", wire_id(k), c.stored(k), c.waiting_for(k), c.pending_outputs(k)));
                }
            }
        }
        out
    })
}

/// Terminal clicks, from `native_screen_click`'s button id: the item's index in the list, shifted by three,
/// and flags (1 right button, 2 shift). With them, the actions of AE2's MEStorageScreen: 0 left
/// (PICKUP_OR_SET_DOWN), 1 right (SPLIT_OR_PLACE_SINGLE), 2 shift-left (SHIFT_CLICK), 3 shift-right
/// (PICKUP_SINGLE), 4 space-click (MOVE_REGION), 5 and 6 shift-scroll (ROLL_DOWN, ROLL_UP).
const RIGHT: i32 = 1;
const SHIFT: i32 = 2;
const MOVE_REGION: i32 = 4;
const ROLL_DOWN: i32 = 5;
const ROLL_UP: i32 = 6;
/// InventoryAction.CREATIVE_DUPLICATE (the middle button in creative).
const CREATIVE_DUPLICATE: i32 = 7;

/// `native_menu_button_v2` on a terminal: put the cursor's stack in, or take an item out to the cursor (or
/// with shift, to the inventory).
pub fn menu_button(game: &mut Game<'_>, pos: Pos, entity: &mut ModBlockEntity, id: i32, carried: &mut Option<ItemStack>) {
    if (TOOLBELT..TOOLBELT + 18).contains(&id) {
        toolbelt_click(game, pos, ((id - TOOLBELT) >> 1) as usize, id & 1 != 0, carried);
        return;
    }
    // CraftingTermMenu.clearCraftingGrid / clearToPlayerInventory: the grid (slots 1 to 9) to the network, or to
    // the player's inventory (what doesn't fit there goes to the network).
    if entity.id == "ae2:crafting_terminal" && (id == CT_STASH || id == CT_STASH_PLAYER) {
        let Some(grid) = SERVER.with_borrow(|s| s.grid_of.get(&pos).copied()) else { return };
        let viewer = menu_player(pos);
        for slot in 1..10 {
            let Some(stack) = entity.items.get_mut(slot).and_then(Option::take) else { continue };
            if id == CT_STASH_PLAYER && let Some(uuid) = viewer {
                SERVER.with_borrow_mut(|s| s.pending_gives.push((uuid, grid, stack)));
                continue;
            }
            let key = key_of(game, &stack);
            let stored = with_storage(game, grid, None, |st| st.insert(&key, i64::from(stack.count), Actionable::Modulate));
            let left = stack.count.saturating_sub(stored as u32);
            if left > 0 {
                entity.items[slot] = Some(ItemStack { count: left, ..stack });
            }
        }
        return;
    }
    if entity.id == "ae2:interface" && id == EMIT_FUZZY {
        let m = nbt_i64(&entity.nbt, "fuzzy");
        set_nbt_i64(&mut entity.nbt, "fuzzy", (m + 1) % 5);
        return;
    }
    if entity.id == "ae2:level_emitter" && id == EMIT_FUZZY {
        let m = nbt_i64(&entity.nbt, "fuzzy");
        set_nbt_i64(&mut entity.nbt, "fuzzy", (m + 1) % 5);
        return;
    }
    if entity.id == "ae2:inscriber" && id == INSCRIBER_EXPORT {
        let b = nbt_i64(&entity.nbt, "auto_export");
        set_nbt_i64(&mut entity.nbt, "auto_export", 1 - b);
        return;
    }
    if entity.id == "ae2:level_emitter" && id == EMIT_MODE && cards(entity, 1, "ae2:crafting_card") > 0 {
        // Settings.CRAFT_VIA_REDSTONE takes the redstone mode's place.
        let b = nbt_i64(&entity.nbt, "craft_redstone");
        set_nbt_i64(&mut entity.nbt, "craft_redstone", 1 - b);
        return;
    }
    if matches!(entity.id.as_str(), "ae2:level_emitter" | "ae2:energy_level_emitter") && id == EMIT_MODE {
        let b = nbt_i64(&entity.nbt, "below");
        set_nbt_i64(&mut entity.nbt, "below", 1 - b);
        return;
    }
    if entity.id == "ae2:level_emitter" {
        if id >= GHOST {
            // A copy of what the cursor holds (one), or cleared; right with a filled bucket, the fluid.
            ghost_click(entity, 0, id & RIGHT != 0, carried.as_ref());
        }
        return;
    }
    if canon(&entity.id) == "ae2:interface" {
        if id >= GHOST {
            // The interface's config takes the stack's amount (processing style).
            set_nbt_i64(&mut entity.nbt, "mode", 1);
            ghost_click(entity, ((id - GHOST) >> 3) as usize, id & RIGHT != 0, carried.as_ref());
        }
        return;
    }
    if entity.id == "ae2:condenser" {
        if id == TOGGLE_MODE {
            let m = nbt_i64(&entity.nbt, "mode");
            set_nbt_i64(&mut entity.nbt, "mode", (m + 1) % 3);
        }
        return;
    }
    if entity.id == "ae2:cell_workbench" {
        workbench_button(game, entity, id, carried);
        return;
    }
    if matches!(entity.id.as_str(), "ae2:import_bus" | "ae2:export_bus" | "ae2:storage_bus" | "ae2:formation_plane") {
        if id >= GHOST {
            bus_ghost_click(entity, ((id - GHOST) >> 3) as usize, carried.as_ref());
        } else if id == BUS_REDSTONE {
            let m = nbt_i64(&entity.nbt, "redstone");
            set_nbt_i64(&mut entity.nbt, "redstone", (m + 1) % 3);
        } else if id == BUS_SCHEDULE {
            let m = nbt_i64(&entity.nbt, "schedule");
            set_nbt_i64(&mut entity.nbt, "schedule", (m + 1) % 3);
        } else if id == BUS_CRAFT_ONLY {
            let m = nbt_i64(&entity.nbt, "craft_only");
            set_nbt_i64(&mut entity.nbt, "craft_only", 1 - m);
        } else if id == BUS_FUZZY {
            let m = nbt_i64(&entity.nbt, "fuzzy");
            set_nbt_i64(&mut entity.nbt, "fuzzy", (m + 1) % 5);
        } else if id == BUS_REPORT {
            let m = nbt_i64(&entity.nbt, "report_all");
            set_nbt_i64(&mut entity.nbt, "report_all", 1 - m);
        } else if id == BUS_EXTRACT_FILTER {
            let m = nbt_i64(&entity.nbt, "no_extract_filter");
            set_nbt_i64(&mut entity.nbt, "no_extract_filter", 1 - m);
        } else if id == BUS_ACCESS {
            let m = nbt_i64(&entity.nbt, "access");
            set_nbt_i64(&mut entity.nbt, "access", (m + 1) % 3);
        } else if id == BUS_PLACE {
            let m = nbt_i64(&entity.nbt, "drop_items");
            set_nbt_i64(&mut entity.nbt, "drop_items", 1 - m);
        } else if id == BUS_CLEAR {
            set_ghosts(entity, &[]);
        } else if id == BUS_PARTITION {
            // StorageBusPart's partition: what the container holds now.
            let target = SERVER.with_borrow(|s| s.grids.iter().find_map(|g| g.storage_buses.iter().find(|(b, _)| *b == pos).map(|(_, t)| *t)));
            if let Some(target) = target {
                let face = face_toward(target, pos);
                let mut ids: Vec<String> = Vec::new();
                if let Some(view) = sided(game, target, face) {
                    for st in sided_slots(game, &view).iter().flatten() {
                        if !ids.contains(&st.item.to_string()) {
                            ids.push(st.item.to_string());
                        }
                    }
                } else if let Some(tanks) = game.native_fluid_tanks(&game.dim.id().to_owned(), target, face) {
                    for fluid in tanks.into_iter().filter_map(|t| t.fluid) {
                        ids.push(format!("~{fluid}"));
                    }
                }
                let cells: Vec<Option<(String, i64)>> = ids.into_iter().take(bus_slots(entity)).map(|id| Some((id, 1))).collect();
                set_ghosts(entity, &cells);
            }
        }
        return;
    }
    if canon(&entity.id) == "ae2:pattern_provider" {
        let (key, modes) = match id {
            PP_BLOCKING => ("blocking", 2),
            PP_LOCK => ("lock_mode", 5),
            PP_ACCESS => ("hide", 2),
            _ => return,
        };
        let m = nbt_i64(&entity.nbt, key);
        set_nbt_i64(&mut entity.nbt, key, (m + 1) % modes);
        if key == "lock_mode" {
            // ConfigManager change: PatternProviderLogic.resetCraftingLock.
            set_nbt_i64(&mut entity.nbt, "unlock_event", 0);
            entity.nbt.remove(&java("lock_until"));
        }
        return;
    }
    if entity.id == "ae2:io_port" {
        let (key, modes) = match id {
            TOGGLE_MODE => ("mode", 2),
            IO_FULLNESS => ("full", 3),
            IO_REDSTONE => ("redstone", 4),
            _ => return,
        };
        let m = nbt_i64(&entity.nbt, key);
        set_nbt_i64(&mut entity.nbt, key, (m + 1) % modes);
        return;
    }
    if entity.id == "ae2:pattern_access_terminal" {
        if id == PAT_SHOW {
            let m = nbt_i64(&entity.nbt, "show");
            set_nbt_i64(&mut entity.nbt, "show", (m + 1) % 3);
        } else if id >= PROVIDER_SLOT {
            let n = (id - PROVIDER_SLOT) >> 2;
            provider_click(game, pos, (n / 9) as usize, (n % 9) as usize, id & SHIFT != 0, carried);
        }
        return;
    }
    let id = if entity.id == "ae2:pattern_encoding_terminal" {
        if id >= GHOST {
            ghost_click(entity, ((id - GHOST) >> 3) as usize, id & RIGHT != 0, carried.as_ref());
            return;
        }
        if id >= STONECUT_PICK {
            set_nbt_i64(&mut entity.nbt, "stonecut", i64::from(id - STONECUT_PICK));
            return;
        }
        if id == ENC_SUBSTITUTE {
            let on = nbt_i64(&entity.nbt, "substitute");
            set_nbt_i64(&mut entity.nbt, "substitute", 1 - on);
            return;
        }
        if id == ENC_CYCLE {
            // cycleProcessingOutput: the next set output becomes the primary one.
            let mut cells = enc_ghosts(entity);
            let set: Vec<(String, i64)> = (0..27).filter_map(|j| cells[enc_output(j)].clone()).collect();
            if set.len() > 1 {
                for j in 0..27 {
                    cells[enc_output(j)] = set.get((j + 1) % set.len()).filter(|_| j < set.len()).cloned();
                }
                set_ghosts(entity, &cells);
            }
            return;
        }
        if id < 8 {
            encoder_button(game, entity, id);
            return;
        }
        id - 8
    } else {
        id
    };
    if !matches!(entity.id.as_str(), "ae2:terminal" | "ae2:crafting_terminal" | "ae2:pattern_encoding_terminal") || id < 0 {
        return;
    }
    log::info!("ae2: terminal button {id} at {pos:?}, carrying {:?}", carried.as_ref().map(|s| (s.item.clone(), s.count)));
    // No player's state is swapped in for a menu button: the player is the one who clicked (`button_clicker`).
    // The cursor stack reaches their screen after the hook (`lan_station_button`).
    let Some(uuid) = menu_player(pos) else {
        log::info!("ae2: terminal button: nobody has this terminal open");
        return;
    };
    let Some(grid) = SERVER.with_borrow(|s| s.grid_of.get(&pos).copied()) else {
        log::info!("ae2: terminal button: the terminal is on no grid");
        return;
    };
    if !SERVER.with_borrow(|s| s.grids.get(grid).is_some_and(|g| g.online && g.active.contains(&pos))) {
        log::info!("ae2: terminal button: the terminal is offline");
        return;
    }
    let (index, action) = ((id >> 3) as usize, id & 7);
    let (right, shift) = (action == 1 || action == 3, action == 2 || action == 3);
    if action >= MOVE_REGION || action == 3 {
        log::info!("ae2: terminal action {action} on item {index}");
        terminal_action(game, grid, uuid, index, action, carried);
        SERVER.with_borrow_mut(|s| {
            s.sent.remove(&uuid);
        });
        stream_terminals(game);
        return;
    }
    if terminal_bucket(game, grid, uuid, index, carried) {
        SERVER.with_borrow_mut(|s| {
            s.sent.remove(&uuid);
        });
        stream_terminals(game);
        return;
    }
    if let Some(stack) = carried.as_mut() {
        let key = key_of(game, stack);
        let amount = if right { 1 } else { i64::from(stack.count) };
        let stored = with_storage(game, grid, None, |st| st.insert(&key, amount, Actionable::Modulate));
        stack.count -= stored as u32;
        if stack.count == 0 {
            *carried = None;
        }
    } else {
        let entry = SERVER.with_borrow(|s| s.sent.get(&uuid).and_then(|v| v.keys.get(index).cloned()).filter(|(_, n)| *n >= 0));
        let Some((key, shown)) = entry else {
            log::info!("ae2: terminal button: no item {index} in what was sent");
            return;
        };
        if key.key_type() == super::port_ae2_core::KeyType::Fluids {
            // A fluid comes out only into a bucket on the cursor.
            return;
        }
        if shown == 0 {
            // A craftable entry: ask the network's CPU for it (left 1, right 16, shift 64).
            let amount = if shift { 64 } else if right { 16 } else { 1 };
            request_craft(game, grid, uuid, &key, amount);
            return;
        }
        let most = i64::from(max_stack(game, key.id()));
        let have = with_storage(game, grid, None, |st| st.extract(&key, most, Actionable::Simulate));
        let want = if right { (have + 1) / 2 } else { have };
        let got = with_storage(game, grid, None, |st| st.extract(&key, want, Actionable::Modulate));
        log::info!("ae2: terminal take {:?}: had {have}, took {got}", key.id());
        if got <= 0 {
            return;
        }
        let stack = stack_of(&key, got as u32);
        if shift {
            // Shift: into the inventory (in the player's inventory tick); what doesn't fit goes back.
            SERVER.with_borrow_mut(|s| s.pending_gives.push((uuid, grid, stack)));
        } else {
            // A click: onto the cursor, as AE2's terminal does.
            *carried = Some(stack);
        }
    }
    // The terminal shows the change at once.
    SERVER.with_borrow_mut(|s| {
        s.sent.remove(&uuid);
    });
    stream_terminals(game);
}

/// MEStorageMenu.handleNetworkInteraction's actions on one item beyond pick up / set down: shift-right and
/// shift-scroll up take one onto the cursor (only onto nothing or the same item, short of a full stack),
/// shift-scroll down puts one of the cursor's in, space-click fills the inventory with stacks of it.
fn terminal_action(game: &mut Game<'_>, grid: usize, uuid: u128, index: usize, action: i32, carried: &mut Option<ItemStack>) {
    if action == ROLL_DOWN {
        if let Some(stack) = carried.as_mut() {
            let key = key_of(game, stack);
            if with_storage(game, grid, None, |st| st.insert(&key, 1, Actionable::Modulate)) > 0 {
                stack.count -= 1;
                if stack.count == 0 {
                    *carried = None;
                }
            }
        }
        return;
    }
    // A full stack of the entry's item on the empty cursor, from nothing (creative; craftable entries too).
    if action == CREATIVE_DUPLICATE {
        let creative = creative_player(game, uuid);
        let key = SERVER.with_borrow(|s| s.sent.get(&uuid).and_then(|v| v.keys.get(index).map(|(k, _)| k.clone())));
        if creative
            && carried.is_none()
            && let Some(key) = key.filter(|k| k.key_type() == super::port_ae2_core::KeyType::Items)
        {
            *carried = Some(stack_of(&key, max_stack(game, key.id())));
        }
        return;
    }
    let entry = SERVER.with_borrow(|s| s.sent.get(&uuid).and_then(|v| v.keys.get(index).cloned()).filter(|(_, n)| *n > 0));
    let Some((key, _)) = entry else {
        log::info!("ae2: terminal action {action}: no item {index} in what was sent");
        return;
    };
    if key.key_type() == super::port_ae2_core::KeyType::Fluids {
        return;
    }
    let most = max_stack(game, key.id());
    if action == MOVE_REGION {
        // A stack for each of the inventory's 36 slots (what doesn't fit comes back).
        for _ in 0..36 {
            let got = with_storage(game, grid, None, |st| st.extract(&key, i64::from(most), Actionable::Modulate));
            if got <= 0 {
                break;
            }
            SERVER.with_borrow_mut(|s| s.pending_gives.push((uuid, grid, stack_of(&key, got as u32))));
        }
        return;
    }
    // PICKUP_SINGLE and ROLL_UP.
    if let Some(stack) = carried.as_ref()
        && (stack.count >= most || key_of(game, stack) != key)
    {
        return;
    }
    if with_storage(game, grid, None, |st| st.extract(&key, 1, Actionable::Modulate)) > 0 {
        match carried.as_mut() {
            Some(stack) => stack.count += 1,
            None => *carried = Some(stack_of(&key, 1)),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Client: AE2's screens.

/// CraftingStatusMenu.CraftingCpuListEntry.
#[derive(Clone, Default)]
struct StatusCpu {
    bytes: i64,
    accel: i64,
    mode: i64,
    /// The job's item ("-" idle), its amount and progress (per mille), the seconds it has run and has left.
    item: String,
    amount: i64,
    permille: i64,
    elapsed: i64,
    secs: i64,
    cant_store: bool,
}

#[derive(Default)]
struct Client {
    /// Where an open terminal's panel is (for clicks the screen hook doesn't see: the middle button).
    term_origin: Option<[f32; 2]>,
    /// The meteorite compass' last answer, and when it was asked (CompassManager).
    compass: Compass,
    compass_asked: Option<std::time::Instant>,
    /// The open screen's data slots as last drawn (for its tooltips).
    screen_data: Vec<i32>,
    /// Shift held as the screen was last drawn (for the scroll hook, which gets no keys).
    shift: bool,
    /// Menu buttons to send from the frame (the scroll hook can't send one itself).
    buttons_out: Vec<i32>,
    /// The open terminal's items by the server's index: id and amount (-1: none there now).
    items: Vec<(String, i64)>,
    /// What the grid shows, as indexes into `items`: made again only after a change (a huge list sorts once).
    view: RefCell<Option<std::rc::Rc<Vec<usize>>>>,
    /// The open molecular assembler's grid and what it makes.
    assembler: Vec<(String, i64)>,
    /// An info screen's lines (P2P tunnels, planes, energy cells): AE2's own words for the block.
    info: Vec<String>,
    /// The open pattern access terminal's rows: a group's name and count, or a provider's nine slots (what each
    /// pattern makes, "" empty).
    provider_rows: Vec<ProviderRow>,
    /// The open pattern encoding terminal's ghost cells (grid, then outputs; "" empty) and crafting result.
    ghosts: Vec<(String, i64)>,
    preview: Option<(String, i64)>,
    /// The open encoder's mode, as its screen last drew it (for clicks).
    encoder_crafting: bool,
    /// The mouse over the open screen, from its top left.
    mouse: [f32; 2],
    /// How far the encoder's lower part moved down for the rows its window fits.
    encoder_extra: f32,
    /// The open level emitter's amount box, and whether it has the keys.
    level_text: String,
    level_focus: bool,
    /// The box changed since it was last sent.
    level_dirty: bool,
    /// The wireless terminal: 1 to open, 2 open, and its clicks to send.
    wireless_open: u8,
    /// The wireless crafting terminal's screen (its grid), and the stack on its cursor (key, count).
    wireless_craft: bool,
    wcursor: Option<(String, u32)>,
    wireless_out: Vec<String>,
    /// The grid's crafting jobs (item, left, percent, seconds left; -1 unknown), and whether the terminal shows
    /// them instead of its items (AE2's crafting status button).
    jobs: Vec<(String, i64, i64, i64)>,
    /// The crafting status screen: each CPU (storage, co-processors, mode, item, percent, seconds), its entries (the
    /// CPU's index, item, stored, active, scheduled), the selected CPU and the table's scroll.
    status_cpus: Vec<StatusCpu>,
    /// The status screen is a crafting block's own (CraftingCPUScreen: that CPU only, with its mode button), the
    /// terminal it goes back to, the CPU list's scroll, the pointer, and the scroll bar being dragged.
    status_block: bool,
    status_host: String,
    status_list_scroll: usize,
    status_mouse: [f32; 2],
    scroll_drag: u8,
    gui_size_seen: (f32, f32),
    /// The terminal toolbar's settings beyond sort and view: type filter (all, items, fluids), descending, and
    /// TerminalStyle (small, medium, tall, full), with the screen to lay out again after a new style.
    /// What the open storage bus faces.
    bus_target: String,
    /// The processing panel's rows scrolled by (0 to 24).
    enc_scroll: usize,
    /// What the search reads: each listed item's name, its tags and its tooltip (these two only once asked for).
    search_names: HashMap<String, String>,
    search_tags: HashMap<String, Vec<String>>,
    search_tips: HashMap<String, String>,
    /// The toast of a finished job: its item, amount and when it first showed.
    job_toast: Option<(String, i64, Option<std::time::Instant>)>,
    /// The Upgrade Toolbelt under the upgrades: the network tool's nine stacks (none without a tool), where its panel
    /// is on screen, and its presses waiting to go to the server as menu buttons.
    toolbelt: Vec<(String, i64)>,
    toolbelt_at: Option<[f32; 2]>,
    toolbelt_presses: Vec<i32>,
    press_taken: bool,
    toolbelt_right: f32,
    type_filter: u8,
    sort_desc: bool,
    term_style: u8,
    relayout: bool,
    status_entries: Vec<(usize, String, i64, i64, i64)>,
    status_cpu: usize,
    status_scroll: usize,
    show_jobs: bool,
    /// The terminal's search box (AE2's: words of the item's name, `@mod` for its mod) and whether it has the keys.
    search: String,
    search_focus: bool,
    /// AE2's sort order (amount, name, mod) and view (everything, stored, craftable).
    sort: u8,
    view_mode: u8,
    /// The terminal's first row shown.
    scroll: usize,
    /// The terminal's rows, set when its screen is laid out.
    rows: usize,
    /// The dialog over the screen (amounts, priority, the crafting plan), and messages for `ae2:ui`.
    dialog: Option<Dialog>,
    ui_out: Vec<String>,
    /// The open block's priority (`#p`).
    priority: i64,
    /// The item screen over the world ("portable", "toolbox", "knife", "status"): 1 opening, 2 open, 3 to close.
    item_screen: String,
    item_screen_state: u8,
    /// The network status' lines, the toolbox's nine stacks, the knife's name box.
    panel: Vec<String>,
    toolbox: Vec<(String, i64)>,
    knife_name: String,
    knife_focus: bool,
    /// AE2's terminal settings, shown over the grid, and whether they were read from their file.
    settings_open: bool,
    settings: [bool; 9],
    /// Pinned at the top of the grid: what the player has auto-crafted (Pin auto-crafted items).
    pinned: Vec<String>,
    /// A crafting terminal is open (Clear crafting grid on close), and the search as JEI last had it from here.
    crafting_term_open: bool,
    term_open: bool,
    synced_search: String,
    settings_loaded: bool,
    /// The encoder's mode (0 crafting, 1 processing, 2 smithing, 3 stonecutting) and what its stonecutter offers.
    encoder_mode: i32,
    stonecut: Vec<String>,
    stonecut_pick: usize,
    /// The open condenser's power, capacity, power a product needs and mode (`#c`: bigger than data slots hold).
    condenser: [i64; 4],
    /// The open level emitter's mode: emit below the level instead of at or above it (`#e`).
    emit_below: bool,
}

const TERM_COLS: usize = 9;
const TERM_GRID: [f32; 2] = [8.0, 18.0];
/// Where the terminal's one container slot sits: off the screen. Nothing draws it, but the menu has the slot its
/// type says, and shift-clicks from the inventory land in it (the terminal hands them to the network).
const HIDDEN_SLOT: [f32; 2] = [-10_000.0, -10_000.0];

/// The terminal's rows: as many as fit the window, as AE2's tall terminal (between 3 and 12).
/// TerminalStyle.getRows: a quarter of the rows that fit for each step (this port never shows fewer than its three).
fn style_rows(possible: usize) -> usize {
    let style = CLIENT.with_borrow_mut(|c| {
        settings(c);
        c.term_style
    });
    (possible * (usize::from(style) + 1) / 4).max(3)
}

fn term_rows() -> usize {
    CLIENT.with_borrow(|c| c.rows.max(3))
}

/// `native_net_from_server`: the open terminal's items (only what changed: `index id amount` for an item it
/// hadn't, `index amount` for a new amount, -1 when gone, `#reset` first on a new screen), and its extras.
pub fn from_server(channel: &str, data: &[u8]) {
    if channel == NET_ITEMS {
        let text = String::from_utf8_lossy(data);
        CLIENT.with_borrow_mut(|c| {
            for l in text.lines() {
                if l == "#reset" {
                    c.items.clear();
                    c.scroll = 0;
                    // AE2's terminal settings: the search box focused, the last search kept or not.
                    let s = settings(c);
                    if !s[1] {
                        c.search.clear();
                    }
                    c.search_focus = s[0] && !s[6];
                    c.view.take();
                    c.ui_out.push(format!("notify {}", u8::from(s[4])));
                    continue;
                }
                let f: Vec<&str> = l.split(' ').collect();
                let (i, id, amount) = match f.as_slice() {
                    [i, amount] => (i.parse::<usize>().ok(), None, amount.parse::<i64>().ok()),
                    [i, id, amount] => (i.parse::<usize>().ok(), Some(*id), amount.parse::<i64>().ok()),
                    _ => continue,
                };
                let (Some(i), Some(amount)) = (i, amount) else { continue };
                if c.items.len() <= i {
                    c.items.resize(i + 1, (String::new(), -1));
                }
                if let Some(id) = id {
                    c.items[i].0 = id.to_owned();
                }
                c.items[i].1 = amount;
            }
            c.view.take();
        });
        return;
    }
    if channel == NET_PROVIDERS {
        let text = String::from_utf8_lossy(data);
        CLIENT.with_borrow_mut(|c| {
            for l in text.lines() {
                if l == "#reset" {
                    c.provider_rows.clear();
                } else if let Some(g) = l.strip_prefix("#g ") {
                    let (name, n) = g.rsplit_once('|').unwrap_or((g, "0"));
                    c.provider_rows.push(ProviderRow::Group(name.to_owned(), n.parse().unwrap_or(0)));
                } else if let Some(p) = l.strip_prefix("p ") {
                    let index = c.provider_rows.iter().filter(|r| matches!(r, ProviderRow::Slots(..))).count();
                    let slots = p.split(' ').map(|s| if s == "-" { String::new() } else { s.to_owned() }).collect();
                    c.provider_rows.push(ProviderRow::Slots(index, slots));
                }
            }
        });
        return;
    }
    if channel == NET_UI {
        ui_from_server(data);
        return;
    }
    if channel == NET_WIRELESS {
        if data == b"open" || data == b"open craft" {
            let craft = data == b"open craft";
            CLIENT.with_borrow_mut(|c| {
                c.wireless_open = 1;
                c.wireless_craft = craft;
                c.wcursor = None;
                c.items.clear();
                c.view.take();
                c.scroll = 0;
            });
        } else if let Some(rest) = data.strip_prefix(b"cursor ") {
            let text = String::from_utf8_lossy(rest).into_owned();
            let cursor = text.rsplit_once(' ').and_then(|(k, n)| Some((k.to_owned(), n.parse::<u32>().ok()?))).filter(|(_, n)| *n > 0);
            CLIENT.with_borrow_mut(|c| c.wcursor = cursor);
        } else if data == b"close" {
            // Out of power or range: the frame shuts the screen.
            CLIENT.with_borrow_mut(|c| c.wireless_open = 3);
        }
        return;
    }
    if channel != NET_EXTRAS {
        return;
    }
    let text = String::from_utf8_lossy(data);
    // `#g id amount` lines are the encoder's ghost cells in order, `#r id amount` its crafting result.
    let mut ghosts = Vec::new();
    let mut preview = None;
    let mut jobs = Vec::new();
    let mut status_cpus = Vec::new();
    let mut status_entries = Vec::new();
    for l in text.lines() {
        let parse = |rest: &str| -> Option<(String, i64)> {
            let (id, amount) = rest.split_once(' ')?;
            Some((id.to_owned(), amount.parse().ok()?))
        };
        if let Some(rest) = l.strip_prefix("#g ") {
            ghosts.push(parse(rest).unwrap_or_default());
        } else if let Some(rest) = l.strip_prefix("#r ") {
            preview = parse(rest);
        } else if let Some(rest) = l.strip_prefix("#l ") {
            let level = rest.trim().to_owned();
            CLIENT.with_borrow_mut(|c| {
                if !c.level_focus {
                    c.level_text = level;
                }
            });
        } else if let Some(rest) = l.strip_prefix("#c ") {
            let v: Vec<i64> = rest.split(' ').filter_map(|n| n.parse().ok()).collect();
            if let [a, b, c, d] = v[..] {
                CLIENT.with_borrow_mut(|cl| cl.condenser = [a, b, c, d]);
            }
        } else if let Some(rest) = l.strip_prefix("#e ") {
            let below = rest.trim() == "1";
            CLIENT.with_borrow_mut(|c| c.emit_below = below);
        } else if let Some(rest) = l.strip_prefix("#p ") {
            let p = rest.trim().parse().unwrap_or(0);
            CLIENT.with_borrow_mut(|c| c.priority = p);
        } else if let Some(rest) = l.strip_prefix("#s ") {
            let p = rest.trim().parse().unwrap_or(0);
            CLIENT.with_borrow_mut(|c| c.stonecut_pick = p);
        } else if let Some(rest) = l.strip_prefix("#C ") {
            let f: Vec<&str> = rest.split(' ').collect();
            if let [_, bytes, accel, mode, item, amount, permille, secs, elapsed, cant] = f.as_slice() {
                status_cpus.push(StatusCpu {
                    bytes: bytes.parse().unwrap_or(0),
                    accel: accel.parse().unwrap_or(0),
                    mode: mode.parse().unwrap_or(0),
                    item: (*item).to_owned(),
                    amount: amount.parse().unwrap_or(0),
                    permille: permille.parse().unwrap_or(0),
                    elapsed: elapsed.parse().unwrap_or(0),
                    secs: secs.parse().unwrap_or(-1),
                    cant_store: *cant == "1",
                });
            }
        } else if let Some(rest) = l.strip_prefix("#t ") {
            let target = rest.trim().to_owned();
            CLIENT.with_borrow_mut(|c| c.bus_target = target);
        } else if let Some(rest) = l.strip_prefix("#B ") {
            let cpu = rest.trim().parse().unwrap_or(0);
            CLIENT.with_borrow_mut(|c| c.status_cpu = cpu);
        } else if let Some(rest) = l.strip_prefix("#E ") {
            let f: Vec<&str> = rest.split(' ').collect();
            if let [cpu, id, stored, active, pending] = f.as_slice() {
                status_entries.push((cpu.parse().unwrap_or(0), (*id).to_owned(), stored.parse().unwrap_or(0), active.parse().unwrap_or(0), pending.parse().unwrap_or(0)));
            }
        } else if let Some(rest) = l.strip_prefix("#j ") {
            let f: Vec<&str> = rest.split(' ').collect();
            if let [id, left, pct, secs] = f.as_slice() {
                jobs.push(((*id).to_owned(), left.parse().unwrap_or(0), pct.parse().unwrap_or(0), secs.parse().unwrap_or(-1)));
            }
        }
    }
    let assembler: Vec<(String, i64)> = text
        .lines()
        .filter_map(|l| {
            let (id, amount) = l.strip_prefix("#a ")?.split_once(' ')?;
            Some((id.to_owned(), amount.parse().ok()?))
        })
        .collect();
    let info: Vec<String> = text.lines().filter_map(|l| l.strip_prefix("#d ").map(str::to_owned)).collect();
    let stonecut: Vec<String> = text.lines().filter_map(|l| l.strip_prefix("#o ").and_then(|r| r.split(' ').next()).map(str::to_owned)).collect();
    CLIENT.with_borrow_mut(|c| {
        c.stonecut = stonecut;
        c.info = info;
        c.assembler = assembler;
        c.ghosts = ghosts;
        c.preview = preview;
        c.jobs = jobs;
        c.status_cpus = status_cpus;
        c.status_entries = status_entries;
    });
}

/// `native_screen`: AE2's layouts over its GUI textures.
pub fn screen(game: &Game<'_>, block: &str, out: &mut Option<super::native_hooks::NativeScreen>) {
    use super::native_hooks::NativeScreen;
    *out = match ae2_name(block) {
        Some("drive") => Some(NativeScreen {
            width: 176.0,
            height: 199.0,
            slots: (0..10).map(|i| Some([71.0 + (i % 2) as f32 * 18.0, 14.0 + (i / 2) as f32 * 18.0])).collect(),
            inventory_y: 117.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        Some("vibration_chamber") => Some(NativeScreen {
            width: 176.0 + UPGRADE_PANEL,
            height: 166.0,
            slots: std::iter::once(Some([80.0, 37.0])).chain(upgrade_panel(176.0, 3)).collect(),
            inventory_y: 84.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        Some("chest") => Some(NativeScreen {
            width: 176.0,
            height: 166.0,
            slots: vec![Some([80.0, 37.0])],
            inventory_y: 84.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        Some("crafting_terminal") => {
            // The terminal's head and three rows, then the crafting grid and its result, then the inventory.
            let (_, gui_h) = game.gui_size();
            let rows = style_rows(((gui_h - 241.0 + 54.0 - 20.0) / 18.0).floor() as usize).clamp(3, 10);
            CLIENT.with_borrow_mut(|c| c.rows = rows);
            let extra = 18.0 * (rows - 3) as f32;
            let mut slots: Vec<Option<[f32; 2]>> = vec![Some(HIDDEN_SLOT)];
            slots.extend((0..9).map(|i| Some([CRAFT_GRID[0] + (i % 3) as f32 * 18.0, CRAFT_GRID[1] + extra + (i / 3) as f32 * 18.0])));
            slots.push(Some([CRAFT_RESULT[0], CRAFT_RESULT[1] + extra]));
            slots.extend(upgrade_panel(195.0, VIEW_CELLS));
            Some(NativeScreen { width: 176.0, height: 241.0 + extra, slots, inventory_y: 159.0 + extra, inventory_x: 8.0, own_labels: true })
        }
        Some("pattern_encoding_terminal") => {
            // As many terminal rows as fit the window (AE2's terminal style: header, rows, bottom), the rest below.
            let (_, gui_h) = game.gui_size();
            let rows = style_rows(((gui_h - 249.0 + 54.0 - 20.0) / 18.0).floor() as usize).clamp(3, 10);
            let extra = 18.0 * (rows - 3) as f32;
            CLIENT.with_borrow_mut(|c| {
                c.rows = rows;
                c.encoder_extra = extra;
            });
            let slots: Vec<Option<[f32; 2]>> = vec![Some([ENC_BLANK[0], ENC_BLANK[1] + extra]), Some([ENC_ENCODED[0], ENC_ENCODED[1] + extra])]
                .into_iter()
                .chain(upgrade_panel(195.0, VIEW_CELLS))
                .collect();
            Some(NativeScreen { width: 195.0, height: 249.0 + extra, slots, inventory_y: 167.0 + extra, inventory_x: 8.0, own_labels: true })
        }
        // AE2's screens/terminals/pattern_access_terminal.json over patternaccessterminal.png: six rows of name
        // bars and slots, the scroll bar at 175, the inventory from y 140.
        // AE2's screens/condenser.json: input 51, 52, output 105, 52, the storage component at 101, 26.
        Some("condenser") => Some(NativeScreen {
            width: 176.0,
            height: 197.0,
            slots: vec![Some([51.0, 52.0]), Some([105.0, 52.0]), Some([101.0, 26.0])],
            inventory_y: 115.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        // AE2's screens/cell_workbench.json: the cell at 152, 8, its config (7 rows of 9) from 8, 29; the cell's
        // upgrades in the panel beside it.
        Some("cell_workbench") => Some(NativeScreen {
            width: 176.0 + UPGRADE_PANEL,
            height: 251.0,
            slots: std::iter::once(Some([152.0, 8.0])).chain(upgrade_panel(176.0, WB_UPGRADES)).collect(),
            inventory_y: 169.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        // AE2's screens/energy_level_emitter.json: the amount box at 23, 42.
        Some("energy_level_emitter") => Some(NativeScreen {
            width: 176.0,
            height: 184.0,
            slots: vec![Some(HIDDEN_SLOT)],
            inventory_y: 102.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        // AE2's screens/qnb.json: the singularity at 80, 37 over chest.png.
        Some("quantum_link") => Some(NativeScreen {
            width: 176.0,
            height: 166.0,
            slots: vec![Some([80.0, 37.0])],
            inventory_y: 84.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        // AE2's screens/spatial_anchor.json: no inventory, its statistics.
        Some("spatial_anchor") => Some(NativeScreen {
            width: 195.0,
            height: 100.0,
            slots: vec![Some(HIDDEN_SLOT)],
            inventory_y: HIDDEN_SLOT[1],
            inventory_x: HIDDEN_SLOT[0],
            own_labels: true,
        }),
        // The info screens: AE2's lines for the block, and the formation plane's filter row.
        Some(n) if is_info_block(n) => Some(NativeScreen {
            width: 176.0,
            height: 166.0,
            slots: vec![if n == "item_p2p_tunnel" { Some([152.0, 8.0]) } else { Some(HIDDEN_SLOT) }],
            inventory_y: 84.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        // AE2's screens/spatial_io_port.json: input at 52, 48, output at 113, 48.
        Some("spatial_io_port") => Some(NativeScreen {
            width: 176.0,
            height: 197.0,
            slots: vec![Some([52.0, 48.0]), Some([113.0, 48.0])],
            inventory_y: 115.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        // AE2's screens/wireless_access_point.json: boosters at 80, 47; the link panel's in and out at the right.
        Some("wireless_access_point") => Some(NativeScreen {
            // The link panel outside the window's right edge (AE2: right -3), its two slots in its wells.
            width: 222.0,
            height: 166.0,
            slots: vec![Some([80.0, 47.0]), Some([193.0, 10.0]), Some([193.0, 68.0])],
            inventory_y: 84.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        // AE2's screens/level_emitter.json: the config item at 137, 40, the amount box at 23, 42.
        Some("level_emitter") => Some(NativeScreen {
            width: 176.0 + UPGRADE_PANEL,
            height: 184.0,
            slots: std::iter::once(Some(HIDDEN_SLOT)).chain(upgrade_panel(176.0, 1)).collect(),
            inventory_y: 102.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        // AE2's screens/interface.json: config ghosts at y 53, storage slots at y 71.
        Some("interface") => Some(NativeScreen {
            width: 176.0 + UPGRADE_PANEL,
            height: 205.0,
            slots: (0..9).map(|i| Some([8.0 + 18.0 * i as f32, 71.0])).chain(upgrade_panel(176.0, 1)).collect(),
            inventory_y: 123.0,
            inventory_x: 8.0,
            own_labels: true,
        }),
        // AE2's screens/io_port.json: two columns of input at 19, output at 122, the mode button between.
        Some("io_port") => Some(NativeScreen {
            width: 176.0 + UPGRADE_PANEL,
            height: 166.0,
            slots: (0..6)
                .map(|i| Some([19.0 + 18.0 * (i % 2) as f32, 17.0 + 18.0 * (i / 2) as f32]))
                .chain((0..6).map(|i| Some([122.0 + 18.0 * (i % 2) as f32, 17.0 + 18.0 * (i / 2) as f32])))
                .chain(upgrade_panel(176.0, 3))
                .collect(),
            inventory_y: 84.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        Some("pattern_access_terminal") => Some(NativeScreen {
            width: 195.0,
            height: 220.0,
            slots: vec![Some(HIDDEN_SLOT)],
            inventory_y: 140.0,
            inventory_x: 8.0,
            own_labels: true,
        }),
        // CraftingCPUScreen has no slots of its own or the player's: `status_draw` is all of it.
        Some(_) if is_crafting(block) => Some(NativeScreen {
            width: 2.0,
            height: 2.0,
            slots: vec![Some(HIDDEN_SLOT)],
            inventory_y: HIDDEN_SLOT[1],
            inventory_x: HIDDEN_SLOT[0],
            own_labels: true,
        }),
        // AE2's screens/sky_chest.json: four rows of nine at (8, 24), the inventory 82 above the bottom.
        Some("sky_stone_chest" | "smooth_sky_stone_chest") => Some(NativeScreen {
            width: 176.0,
            height: 195.0,
            slots: (0..36).map(|i| Some([8.0 + 18.0 * (i % 9) as f32, 24.0 + 18.0 * (i / 9) as f32])).collect(),
            inventory_y: 113.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        Some("molecular_assembler") => Some(NativeScreen {
            // The upgrade panel beside it is part of the screen (its clicks are the screen's, as AE2's).
            width: 176.0 + UPGRADE_PANEL,
            height: 197.0,
            slots: std::iter::once(Some(HIDDEN_SLOT)).chain(upgrade_panel(176.0, 5)).collect(),
            inventory_y: 116.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        Some("controller") => Some(NativeScreen {
            width: 176.0,
            height: 166.0,
            slots: vec![Some(HIDDEN_SLOT)],
            inventory_y: 84.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        Some("inscriber") => Some(NativeScreen {
            width: 176.0 + UPGRADE_PANEL,
            height: 176.0,
            slots: [Some([45.0, 16.0]), Some([63.0, 39.0]), Some([45.0, 62.0]), Some([113.0, 40.0])].into_iter().chain(upgrade_panel(176.0, 4)).collect(),
            inventory_y: 94.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        // AE2's screens/io_bus.json and storage_bus.json over storagebus.png: config cells from 8, 29.
        Some("import_bus" | "export_bus" | "storage_bus" | "formation_plane") => Some(NativeScreen {
            width: 176.0 + UPGRADE_PANEL,
            height: 251.0,
            slots: upgrade_panel(176.0, upgrade_slots(block)).collect(),
            inventory_y: 169.0,
            inventory_x: 8.0,
            own_labels: false,
        }),
        Some("pattern_provider") => Some(NativeScreen {
            width: 176.0,
            height: 213.0,
            slots: (0..9)
                .map(|i| Some([8.0 + i as f32 * 18.0, 45.0]))
                .chain((0..9).map(|i| Some([8.0 + i as f32 * 18.0, 97.0])))
                .collect(),
            inventory_y: 131.0,
            inventory_x: 8.0,
            own_labels: true,
        }),
        Some("terminal") => {
            // AE2's texture: a 17-pixel head, 18-pixel rows, then 95 pixels from the last row's foot down.
            let (_, gui_h) = game.gui_size();
            let rows = style_rows(((gui_h - 17.0 - 95.0 - 20.0) / 18.0).floor() as usize).clamp(3, 12);
            CLIENT.with_borrow_mut(|c| c.rows = rows);
            let extra = 18.0 * (rows - 3) as f32;
            Some(NativeScreen {
                width: 176.0,
                height: 166.0 + extra,
                slots: std::iter::once(Some(HIDDEN_SLOT)).chain(upgrade_panel(195.0, VIEW_CELLS)).collect(),
                // AE2's texture puts the inventory two pixels lower than vanilla's (86).
                inventory_y: 86.0 + extra,
                inventory_x: 8.0,
                own_labels: true,
            })
        }
        _ => return,
    };
}

/// The upgrade panel's width (AE2's extra_panels.png frame and the gap to the screen).
const UPGRADE_PANEL: f32 = 34.0;

/// The upgrade panel's slots for a screen `width` wide: AE2's panel two pixels past its right edge.
fn upgrade_panel(width: f32, n: usize) -> impl Iterator<Item = Option<[f32; 2]>> {
    (0..n).map(move |i| Some([width + 2.0 + 8.0, 8.0 + 18.0 * i as f32]))
}

/// ToolboxPanel (screens/common/toolbox.json): where the network tool's nine stacks go, 90 up from the screen's foot
/// beside it. Drawn over everything once the frame is done (`toolbelt_draw`).
fn draw_toolbelt(game: &Game<'_>, _batch: &mut HudBatch, ox: f32, oy: f32, width: f32) {
    let shown = CLIENT.with_borrow(|c| !c.toolbelt.is_empty());
    let at = game.native_screen_height().filter(|_| shown).map(|height| [ox + width + 2.0, oy + height - 90.0]);
    CLIENT.with_borrow_mut(|c| {
        c.toolbelt_at = at;
        c.toolbelt_right = width + 2.0 + 68.0;
    });
}

fn toolbelt_draw(game: &Game<'_>, batch: &mut HudBatch) {
    let (at, stacks) = CLIENT.with_borrow(|c| (c.toolbelt_at, c.toolbelt.clone()));
    let Some([x, y]) = at.filter(|_| !overlay_shown()) else { return };
    game.native_sprite(batch, "ae2:gui/extra_panels", [60.0, 60.0, 68.0, 68.0], [x, y, 68.0, 68.0]);
    for (i, (id, n)) in stacks.iter().enumerate().take(9) {
        if id.is_empty() || *n <= 0 {
            continue;
        }
        let (sx, sy) = (x + 8.0 + 18.0 * (i % 3) as f32, y + 8.0 + 18.0 * (i / 3) as f32);
        game.native_item(batch, id, [sx, sy], 16.0);
        if *n > 1 {
            let label = n.to_string();
            let w = game.native_text_width(&label);
            game.native_text(batch, &label, [sx + 17.0 - w, sy + 9.0], 0xFFFFFF, true);
        }
    }
}

/// The toolbelt's tooltip: the stack under the pointer, or ToolboxPanel.getTooltip.
fn toolbelt_tooltip(game: &Game<'_>, batch: &mut HudBatch, mouse: [f32; 2]) {
    let (at, stacks) = CLIENT.with_borrow(|c| (c.toolbelt_at, c.toolbelt.clone()));
    let Some([x, y]) = at.filter(|_| !overlay_shown()) else { return };
    if mouse[0] < x || mouse[0] >= x + 68.0 || mouse[1] < y || mouse[1] >= y + 68.0 {
        return;
    }
    let cell = toolbelt_cell([mouse[0] - x, mouse[1] - y]);
    let legacy = |l: &str| (horizon_client::text::Text::legacy(l), u32::MAX);
    let lines = match cell.and_then(|c| stacks.get(c)).filter(|s| s.1 > 0) {
        Some((id, _)) => vec![legacy(&key_words(game, id))],
        None => vec![legacy("Network Tool"), legacy("§7Upgrade Toolbelt")],
    };
    game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
}

/// The toolbelt's slot at `at`, from the panel's corner.
fn toolbelt_cell(at: [f32; 2]) -> Option<usize> {
    let (cx, cy) = ((at[0] - 8.0) / 18.0, (at[1] - 8.0) / 18.0);
    ((0.0..3.0).contains(&cx) && (0.0..3.0).contains(&cy)).then(|| cy as usize * 3 + cx as usize)
}

/// AE2's upgrade panel (extra_panels.png): a framed slot per upgrade, stacked, beside the screen.
fn draw_upgrade_panel(game: &Game<'_>, batch: &mut HudBatch, ox: f32, oy: f32, width: f32, height: f32, n: usize) {
    if n == 0 {
        return;
    }
    draw_toolbelt(game, batch, ox, oy, width);
    let x = ox + width + 2.0;
    // The frame's top, a middle per slot, its plain middle down to the screen's foot (the screen's rectangle takes
    // the panel in), its bottom; each slot's well from the panel's art.
    game.native_sprite(batch, "ae2:gui/extra_panels", [0.0, 0.0, 32.0, 7.0], [x, oy, 32.0, 7.0]);
    for i in 0..n {
        game.native_sprite(batch, "ae2:gui/extra_panels", [0.0, 7.0, 32.0, 18.0], [x, oy + 7.0 + 18.0 * i as f32, 32.0, 18.0]);
    }
    let rest = oy + 7.0 + 18.0 * n as f32;
    let foot = oy + height - 7.0;
    if foot > rest {
        game.native_sprite(batch, "ae2:gui/extra_panels", [0.0, 6.0, 32.0, 1.0], [x, rest, 32.0, foot - rest]);
    }
    game.native_sprite(batch, "ae2:gui/extra_panels", [0.0, 25.0, 32.0, 7.0], [x, foot.max(rest), 32.0, 7.0]);
}

/// `native_screen_draw`.
pub fn screen_draw(game: &Game<'_>, block: &str, data: &[i32], batch: &mut HudBatch, origin: [f32; 2]) {
    let shift = game.keys.contains(&winit::keyboard::KeyCode::ShiftLeft) || game.keys.contains(&winit::keyboard::KeyCode::ShiftRight);
    CLIENT.with_borrow_mut(|c| {
        c.screen_data = data.to_vec();
        c.shift = shift;
        c.term_origin = matches!(ae2_name(block), Some("terminal" | "crafting_terminal" | "pattern_encoding_terminal")).then_some(origin);
    });
    ghost_seen(block, origin);
    CLIENT.with_borrow_mut(|c| c.toolbelt_at = None);
    if overlay_shown() {
        if matches!(ae2_name(block), Some("terminal" | "crafting_terminal" | "pattern_encoding_terminal")) {
            CLIENT.with_borrow_mut(|c| c.status_host = block.to_owned());
        }
        return;
    }
    let [ox, oy] = origin;
    let height = match ae2_name(block) {
        Some("inscriber") => 176.0,
        Some("io_port") => 166.0,
        Some("molecular_assembler") => 197.0,
        Some("interface") => 205.0,
        Some("level_emitter") => 184.0,
        Some("import_bus" | "export_bus" | "storage_bus" | "formation_plane") => 251.0,
        _ => 166.0,
    };
    draw_upgrade_panel(game, batch, ox, oy, 176.0, height, upgrade_slots(block));
    if has_priority(block) {
        // AE2's priority button (the wrench tab), on the left as the terminals' toolbar.
        draw_button_hover(game, batch, [ox + TOOLBAR[0][0], oy + TOOLBAR[0][1], 18.0, 18.0], "", [ox, oy]);
        game.native_item(batch, "minecraft:comparator", [ox + TOOLBAR[0][0] + 1.0, oy + TOOLBAR[0][1] + 1.0], 16.0);
    }
    match ae2_name(block) {
        Some("condenser") => {
            game.native_sprite(batch, "ae2:gui/condenser", [0.0, 0.0, 176.0, 197.0], [ox, oy, 176.0, 197.0]);
            let [power, storage, required, mode] = CLIENT.with_borrow(|c| c.condenser);
            if required > 0 {
                let h = (18.0 * power.min(required) as f32 / required as f32).floor();
                if h > 0.0 {
                    game.native_sprite(batch, "ae2:gui/condenser", [178.0, 25.0 + 18.0 - h, 6.0, h], [ox + 120.0, oy + 25.0 + 18.0 - h, 6.0, h]);
                }
            }
            let icon = ["minecraft:lava_bucket", "ae2:matter_ball", "ae2:singularity"][mode.clamp(0, 2) as usize];
            draw_button_hover(game, batch, [ox + 128.0, oy + 52.0, 18.0, 18.0], "", [ox, oy]);
            game.native_item(batch, icon, [ox + 129.0, oy + 53.0], 16.0);
            let label = ["Trash", "Matter Balls", "Singularity"][mode.clamp(0, 2) as usize];
            game.native_text(batch, label, [ox + 8.0, oy + 78.0], 0x404040, false);
            let need = if mode == 0 { "Destroys what is put in".to_owned() } else if storage < required { "§4Needs a bigger storage component".to_owned() } else { format!("{power} / {required}") };
            game.native_text(batch, &need, [ox + 8.0, oy + 90.0], 0x404040, false);
        }
        Some("cell_workbench") => {
            game.native_sprite(batch, "ae2:gui/cellworkbench", [0.0, 0.0, 176.0, 251.0], [ox, oy, 176.0, 251.0]);
            for i in 0..63 {
                draw_slot(game, batch, ox + 8.0 + 18.0 * (i % 9) as f32, oy + 29.0 + 18.0 * (i / 9) as f32);
            }
            CLIENT.with_borrow(|c| {
                for (i, (id, _)) in c.ghosts.iter().enumerate().take(63) {
                    if !id.is_empty() && id != "minecraft:air" {
                        draw_key(game, batch, id, [ox + 8.0 + 18.0 * (i % 9) as f32, oy + 29.0 + 18.0 * (i / 9) as f32], 16.0);
                    }
                }
            });
            // AE2's left toolbar: fuzzy mode (with a fuzzy card), partition (wrench), clear, copy mode.
            let d = |i: usize| data.get(i).copied().unwrap_or(0);
            for (button, at) in wb_toolbar(d(1) >= 0) {
                let icon = match button {
                    WB_FUZZY => ICON_FUZZY[d(1).clamp(0, 4) as usize],
                    PARTITION => [32.0, 64.0],
                    CLEAR_CONFIG => [96.0, 0.0],
                    _ => if d(0) == 0 { [80.0, 176.0] } else { [80.0, 192.0] },
                };
                draw_state_button(game, batch, [ox + at[0], oy + at[1]], icon);
            }
            draw_upgrade_panel(game, batch, ox, oy, 176.0, 251.0, WB_UPGRADES);
        }
        Some("energy_level_emitter") => {
            game.native_sprite(batch, "ae2:gui/energy_level_emitter", [0.0, 0.0, 176.0, 184.0], [ox, oy, 176.0, 184.0]);
            draw_level_widget(game, batch, ox, oy, 133.0);
        }
        Some("quantum_link") => {
            game.native_sprite(batch, "ae2:gui/chest", [0.0, 0.0, 176.0, 166.0], [ox, oy, 176.0, 166.0]);
            let formed = data.first().copied().unwrap_or(0) != 0;
            let linked = data.get(1).copied().unwrap_or(0) != 0;
            game.native_text(batch, if formed { "§2Formed" } else { "§4Not formed: surround it with 8 rings" }, [ox + 8.0, oy + 18.0], 0x404040, false);
            game.native_text(batch, if linked { "§2Entangled singularity in" } else { "§8Put an entangled singularity in" }, [ox + 8.0, oy + 58.0], 0x404040, false);
        }
        Some("spatial_anchor") => {
            game.native_sprite(batch, "ae2:gui/spatialanchor", [0.0, 0.0, 195.0, 100.0], [ox, oy, 195.0, 100.0]);
            game.native_text(batch, "Spatial Anchor", [ox + 8.0, oy + 6.0], 0x404040, false);
            let lines = CLIENT.with_borrow(|c| c.info.clone());
            let at = [21.0, 31.0, 71.0, 81.0];
            for (l, y) in lines.iter().skip(1).zip(at) {
                let l = l.replace("§7", "§8").replace("§f", "§0");
                batch.text_styled(&game.gui.atlas, &horizon_client::text::Text::legacy(&l), ox + 13.0, oy + y, horizon_client::hud::rgba(64, 64, 64, 255), false);
            }
            game.native_text(batch, "Statistics", [ox + 8.0, oy + 56.0], 0x404040, false);
        }
        Some("drive") => {
            game.native_sprite(batch, "ae2:gui/drive", [0.0, 0.0, 176.0, 199.0], [ox, oy, 176.0, 199.0]);
        }
        Some("chest") => {
            // AE2's screens/chest.json: guis/chest.png with the cell slot at 80, 37.
            game.native_sprite(batch, "ae2:gui/chest", [0.0, 0.0, 176.0, 166.0], [ox, oy, 176.0, 166.0]);
        }
        Some("vibration_chamber") => {
            // AE2's VibrationChamberScreen: guis/vibchamber.png, the flame burning down over the fuel slot (the
            // percent of the fuel's burn time left) and the generation rate bar.
            game.native_sprite(batch, "ae2:gui/vibchamber", [0.0, 0.0, 176.0, 166.0], [ox, oy, 176.0, 166.0]);
            let burn = data.first().copied().unwrap_or(0);
            let total = data.get(1).copied().unwrap_or(0).max(1);
            if burn > 0 {
                let f = ((burn * 100 / total).clamp(0, 100) * 13 / 100) as f32;
                game.native_sprite(batch, "ae2:gui/vibchamber", [176.0, 13.0 - f, 14.0, f], [ox + 80.0, oy + 20.0 + 13.0 - f, 14.0, f]);
                game.native_sprite(batch, "ae2:gui/vibchamber", [176.0, 14.0, 6.0, 18.0], [ox + 99.0, oy + 36.0, 6.0, 18.0]);
            }
        }
        Some("pattern_encoding_terminal") => {
            draw_upgrade_panel(game, batch, ox, oy, 195.0, 14.0 + 18.0 * VIEW_CELLS as f32, VIEW_CELLS);
            let mode = data.get(1).copied().unwrap_or(0);
            let crafting = mode == 0;
            let (rows, extra) = CLIENT.with_borrow(|c| (c.rows.max(3), c.encoder_extra));
            let top = oy;
            game.native_sprite(batch, "ae2:gui/pattern", [0.0, 0.0, 195.0, 17.0], [ox, top, 195.0, 17.0]);
            for r in 0..rows {
                let src = if r == 0 { 17.0 } else if r + 1 == rows { 53.0 } else { 35.0 };
                game.native_sprite(batch, "ae2:gui/pattern", [0.0, src, 195.0, 18.0], [ox, top + 17.0 + 18.0 * r as f32, 195.0, 18.0]);
            }
            game.native_sprite(batch, "ae2:gui/pattern", [0.0, 71.0, 195.0, 178.0], [ox, top + 17.0 + 18.0 * rows as f32, 195.0, 178.0]);
            draw_terminal_items(game, batch, ox, top, data.first().copied().unwrap_or(0) != 0);
            // Below the rows, everything moves down with them.
            let oy = top + extra;
            // The mode's panel (AE2's pattern_modes.png): crafting's grid and result, or processing's grid
            // and three outputs, its grid at the same place.
            CLIENT.with_borrow_mut(|c| {
                c.encoder_crafting = crafting;
                c.encoder_mode = mode;
            });
            let panel = match mode {
                0 => [0.0, 0.0, 126.0, 68.0],
                1 => [0.0, 70.0, 126.0, 68.0],
                2 => [128.0, 70.0, 126.0, 68.0],
                _ => [0.0, 141.0, 126.0, 68.0],
            };
            game.native_sprite(batch, "ae2:gui/pattern_modes", panel, [ox + 9.0, oy + 85.0, 126.0, 68.0]);
            game.native_text(batch, "Pattern Encoding", [ox + 8.0, oy + 74.0], 0x404040, false);
            // The mode tabs, the selected one joined to the panel, each with its station's icon.
            for (i, (at, icon)) in ENC_TABS.iter().zip(["minecraft:crafting_table", "minecraft:furnace", "minecraft:smithing_table", "minecraft:stonecutter"]).enumerate() {
                let selected = i as i32 == mode;
                let [x, y] = [ox + at[0] - if selected { 2.0 } else { 0.0 }, oy + at[1]];
                draw_button_hover(game, batch, [x, y, 20.0 + if selected { 2.0 } else { 0.0 }, 20.0], "", [ox, top]);
                game.native_item(batch, icon, [ox + at[0] + 2.0, oy + at[1] + 2.0], 16.0);
            }
            let clear = ENC_CLEAR[usize::from(mode == 1)];
            draw_button_hover(game, batch, [ox + clear[0], oy + clear[1], 9.0, 9.0], "", [ox, top]);
            game.native_text(batch, "x", [ox + clear[0] + 2.0, oy + clear[1] - 0.5], 0xFFFFFF, false);
            if mode == 0 {
                draw_button_hover(game, batch, [ox + ENC_SUBSTITUTE_AT[0], oy + ENC_SUBSTITUTE_AT[1], 9.0, 9.0], "", [ox, top]);
                let icon = if data.get(2).copied().unwrap_or(0) != 0 { [64.0, 48.0] } else { [112.0, 48.0] };
                game.native_sprite(batch, "ae2:gui/states", [icon[0], icon[1], 16.0, 16.0], [ox + ENC_SUBSTITUTE_AT[0] + 0.5, oy + ENC_SUBSTITUTE_AT[1] + 0.5, 8.0, 8.0]);
            }
            if mode == 1 {
                // Scrollbar.SMALL over the panel's scroll track.
                let (scroll, drag, mouse_y) = CLIENT.with_borrow(|c| (c.enc_scroll, c.scroll_drag, c.mouse[1]));
                let scroll = if drag == 5 {
                    let row = scrollbar_row(mouse_y - (oy - top), 92.0, 52.0, 24);
                    CLIENT.with_borrow_mut(|c| c.enc_scroll = row);
                    row
                } else {
                    scroll
                };
                let y = oy + 92.0 + (37.0 * scroll as f32 / 24.0).floor();
                game.native_sprite(batch, "ae2:gui/pattern_modes", [242.0, 0.0, 7.0, 15.0], [ox + 17.0, y, 7.0, 15.0]);
            }
            if mode == 1 && CLIENT.with_borrow(|c| encoder_outputs(c)) > 1 {
                draw_button_hover(game, batch, [ox + ENC_CYCLE_AT[0], oy + ENC_CYCLE_AT[1], 9.0, 9.0], "", [ox, top]);
                game.native_sprite(batch, "ae2:gui/states", [0.0, 240.0, 16.0, 16.0], [ox + ENC_CYCLE_AT[0] + 0.5, oy + ENC_CYCLE_AT[1] + 0.5, 8.0, 8.0]);
            }
            draw_button_hover(game, batch, [ox + ENC_ENCODE[0], oy + ENC_ENCODE[1], 16.0, 16.0], "", [ox, top]);
            game.native_text(batch, "↓", [ox + ENC_ENCODE[0] + 5.0, oy + ENC_ENCODE[1] + 4.0], 0xFFFFFF, true);
            CLIENT.with_borrow(|c| {
                let cells = mode_cells(mode);
                for i in 0..cells {
                    let Some((id, n)) = c.ghosts.get(enc_visible(i, mode, c.enc_scroll)) else { continue };
                    if id == "minecraft:air" || id.is_empty() {
                        continue;
                    }
                    let [x, y] = ghost_cell_mode(i, mode);
                    draw_key(game, batch, id, [ox + x, oy + y], 16.0);
                    if *n > 1 {
                        let label = if id.starts_with('~') { key_amount(id, *n, super::port_ae2_core::AmountFormat::Slot) } else { n.to_string() };
                        let w = game.native_text_width(&label);
                        game.native_text(batch, &label, [ox + x + 17.0 - w, oy + y + 9.0], 0xFFFFFF, true);
                    }
                }
                if mode == 3 {
                    // The stonecutter's results, four a row; the picked one lit.
                    for (i, id) in c.stonecut.iter().enumerate().take(12) {
                        let (x, y) = (ox + 52.0 + 16.0 * (i % 4) as f32, oy + 92.0 + 18.0 * (i / 4) as f32);
                        game.native_fill(batch, [x, y, 16.0, 18.0], if i == c.stonecut_pick { 0xFF8BC34A } else { 0xFF8B8B8B });
                        game.native_item(batch, id, [x, y + 1.0], 16.0);
                    }
                }
                if (mode == 0 || mode == 2) && let Some((id, n)) = &c.preview {
                    let at = if mode == 2 { [111.0, 111.0] } else { [110.0, 111.0] };
                    game.native_item(batch, id, [ox + at[0], oy + at[1]], 16.0);
                    if *n > 1 {
                        let label = n.to_string();
                        let w = game.native_text_width(&label);
                        game.native_text(batch, &label, [ox + 127.0 - w, oy + 120.0], 0xFFFFFF, true);
                    }
                }
            });
        }
        Some("pattern_provider") => {
            // PatternProviderScreen: its texture and texts, the left toolbar (blocking, lock crafting, pattern
            // access terminal), the priority tab and the lock reason (with a lock mode set).
            game.native_sprite(batch, "ae2:gui/pattern_provider", [0.0, 0.0, 176.0, 213.0], [ox, oy, 176.0, 213.0]);
            game.native_text(batch, "ME Pattern Provider", [ox + 8.0, oy + 6.0], 0x404040, false);
            game.native_text(batch, "Patterns", [ox + 8.0, oy + 34.0], 0x404040, false);
            game.native_text(batch, "Return Inventory", [ox + 8.0, oy + 86.0], 0x404040, false);
            game.native_text(batch, "Inventory", [ox + 8.0, oy + 120.0], 0x404040, false);
            // RestrictedInputSlot.PlacableItemType.ENCODED_PATTERN: the slot's background icon (items cover it).
            for i in 0..9 {
                game.native_sprite(batch, "ae2:gui/states", [240.0, 112.0, 16.0, 16.0], [ox + 8.0 + 18.0 * i as f32, oy + 45.0, 16.0, 16.0]);
            }
            let d = |i: usize| data.get(i).copied().unwrap_or(0);
            for (button, at) in pp_toolbar() {
                let icon = match button {
                    PP_BLOCKING => if d(0) != 0 { [80.0, 16.0] } else { [64.0, 16.0] },
                    PP_LOCK => PP_LOCK_ICONS[d(1).clamp(0, 4) as usize],
                    _ => if d(2) != 0 { [80.0, 80.0] } else { [64.0, 80.0] },
                };
                draw_state_button(game, batch, [ox + at[0], oy + at[1]], icon);
            }
            draw_button_hover(game, batch, [ox + PP_PRIORITY[0], oy + PP_PRIORITY[1], 22.0, 22.0], "", [ox, oy]);
            game.native_sprite(batch, "ae2:gui/states", [32.0, 64.0, 16.0, 16.0], [ox + PP_PRIORITY[0] + 3.0, oy + PP_PRIORITY[1] + 3.0, 16.0, 16.0]);
            if d(1) != 0 {
                let locked = d(3) != 0;
                let icon = if locked { [144.0, 0.0] } else { [160.0, 0.0] };
                game.native_sprite(batch, "ae2:gui/states", [icon[0], icon[1], 16.0, 16.0], [ox + 5.0, oy + 15.0, 16.0, 16.0]);
                let text = if locked { "§4Crafting is locked" } else { "§2Crafting is unlocked" };
                game.native_text(batch, text, [ox + 20.0, oy + 20.0], 0x404040, false);
            }
        }
        Some(name @ ("import_bus" | "export_bus" | "storage_bus" | "formation_plane")) => {
            game.native_sprite(batch, "ae2:gui/storagebus", [0.0, 0.0, 176.0, 251.0], [ox, oy, 176.0, 251.0]);
            let d = |i: usize| data.get(i).copied().unwrap_or(0);
            let cells = if matches!(name, "storage_bus" | "formation_plane") { 63 } else { 9 };
            // AE2's texture has two rows of config slots; the rest it draws in code.
            for i in 18..cells {
                draw_slot(game, batch, ox + 8.0 + 18.0 * (i % 9) as f32, oy + 29.0 + 18.0 * (i / 9) as f32);
            }
            if cells == 9 {
                // The import/export bus has one row: the rest of AE2's texture stays plain.
                game.native_fill(batch, [ox + 7.0, oy + 46.0, 162.0, 110.0], 0xFFC6C6C6);
            }
            let open = d(0).max(1) as usize;
            CLIENT.with_borrow(|c| {
                for i in 0..cells {
                    let (x, y) = (ox + 8.0 + 18.0 * (i % 9) as f32, oy + 29.0 + 18.0 * (i / 9) as f32);
                    if i >= open {
                        game.native_fill(batch, [x, y, 16.0, 16.0], 0x80404040);
                        continue;
                    }
                    if let Some((id, _)) = c.ghosts.get(i).filter(|(id, _)| !id.is_empty() && id != "minecraft:air") {
                        draw_key(game, batch, id, [x, y], 16.0);
                    }
                }
            });
            // The left toolbar, and what the storage bus is attached to.
            let (first, buttons) = bus_buttons(block, &d);
            for (n, (_, icon, _, _)) in buttons.iter().enumerate() {
                let at = bar_place(first + n);
                draw_state_button(game, batch, [ox + at[0], oy + at[1]], *icon);
            }
            if name == "storage_bus" {
                let target = CLIENT.with_borrow(|c| c.bus_target.clone());
                let text = if target.is_empty() || target == "minecraft:air" { "Unattached".to_owned() } else { format!("Attached to: {}", key_words(game, &target)) };
                let start = batch.vertices.len();
                game.native_text(batch, &text, [ox + 10.0, oy + 17.0], TEXT, false);
                batch.scale_since(start, ox + 10.0, oy + 17.0, 0.6, 0.6);
            }
        }
        Some(_) if is_crafting(block) => {
            CLIENT.with_borrow_mut(|c| {
                c.show_jobs = true;
                c.status_block = true;
            });
        }
        Some(n) if is_info_block(n) => {
            let lines = CLIENT.with_borrow(|c| c.info.clone());
            for (i, l) in lines.iter().take(5).enumerate() {
                // AE2's light colours are for the dark Jade box: darker on the grey panel.
                let l = l.replace("§7", "§8").replace("§f", "§0").replace("§a", "§2").replace("§e", "§6");
                batch.text_styled(&game.gui.atlas, &horizon_client::text::Text::legacy(&l), ox + 8.0, oy + 20.0 + 10.0 * i as f32, horizon_client::hud::rgba(64, 64, 64, 255), false);
            }
        }
        Some("spatial_io_port") => {
            game.native_sprite(batch, "ae2:gui/spatialio", [0.0, 0.0, 176.0, 197.0], [ox, oy, 176.0, 197.0]);
            let s = [data.get(1).copied().unwrap_or(0), data.get(2).copied().unwrap_or(0), data.get(3).copied().unwrap_or(0)];
            // SpatialIOPortScreen.drawFG.
            let size = if s[0] > 0 { format!("SCS Size: {}x{}x{}", s[0], s[1], s[2]) } else { "SCS Size: Invalid".to_owned() };
            game.native_text(batch, &size, [ox + 13.0, oy + 83.0], 0x404040, false);
            let info = CLIENT.with_borrow(|c| c.info.clone());
            for (i, line) in info.iter().take(4).enumerate() {
                game.native_text(batch, line, [ox + 13.0, oy + 21.0 + 10.0 * i as f32], 0x404040, false);
            }
        }
        Some("wireless_access_point") => {
            game.native_sprite(batch, "ae2:gui/wireless_access_point", [0.0, 0.0, 176.0, 166.0], [ox, oy, 176.0, 166.0]);
            // The link panel, run down to the window's foot (the screen's rectangle takes it in).
            let px = ox + 178.0;
            game.native_sprite(batch, "ae2:gui/wireless_access_point", [198.0, 0.0, 44.0, 94.0], [px, oy, 44.0, 94.0]);
            game.native_sprite(batch, "ae2:gui/wireless_access_point", [198.0, 90.0, 44.0, 1.0], [px, oy + 94.0, 44.0, 68.0]);
            game.native_sprite(batch, "ae2:gui/wireless_access_point", [198.0, 94.0, 44.0, 4.0], [px, oy + 162.0, 44.0, 4.0]);
            let range = data.get(1).copied().unwrap_or(16);
            let text = if data.first().copied().unwrap_or(0) != 0 { format!("Range: {range} blocks") } else { "§4Offline".to_owned() };
            let w = game.native_text_width(&text);
            game.native_text(batch, &text, [ox + 88.0 - w / 2.0, oy + 20.0], 0x404040, false);
        }
        Some("level_emitter") => {
            game.native_sprite(batch, "ae2:gui/level_emitter", [0.0, 0.0, 176.0, 184.0], [ox, oy, 176.0, 184.0]);
            draw_level_widget(game, batch, ox, oy, 103.0);
            // With a Crafting Card the crafting mode's button replaces the redstone mode's.
            if data.get(2).copied().unwrap_or(0) != 0 {
                let icon = if data.get(3).copied().unwrap_or(0) != 0 { [48.0, 144.0] } else { [16.0, 144.0] };
                draw_state_button(game, batch, [ox + TOOLBAR[0][0], oy + TOOLBAR[0][1]], icon);
            }
            if let Some(mode) = data.get(4).copied().filter(|m| *m >= 0) {
                draw_state_button(game, batch, [ox + TOOLBAR[1][0], oy + TOOLBAR[1][1]], FUZZY_BUTTON[mode.clamp(0, 4) as usize].0);
            }
            let config = CLIENT.with_borrow(|c| c.ghosts.first().cloned());
            draw_slot(game, batch, ox + 137.0, oy + 40.0);
            if let Some((id, _)) = config.filter(|(id, _)| !id.is_empty() && id != "minecraft:air") {
                draw_key(game, batch, &id, [ox + 137.0, oy + 40.0], 16.0);
            }

        }
        Some("interface") => {
            game.native_sprite(batch, "ae2:gui/interface", [0.0, 0.0, 176.0, 205.0], [ox, oy, 176.0, 205.0]);
            if let Some(mode) = data.get(1).copied().filter(|m| *m >= 0) {
                draw_state_button(game, batch, [ox + TOOLBAR[0][0], oy + TOOLBAR[0][1]], FUZZY_BUTTON[mode.clamp(0, 4) as usize].0);
            }
            game.native_text(batch, "ME Interface", [ox + 8.0, oy + 6.0], 0x404040, false);
            game.native_text(batch, "Config", [ox + 8.0, oy + 24.0], 0x404040, false);
            game.native_text(batch, "Stored Items", [ox + 8.0, oy + 91.0], 0x404040, false);
            game.native_text(batch, "Inventory", [ox + 8.0, oy + 112.0], 0x404040, false);
            CLIENT.with_borrow(|c| {
                for (i, (id, n)) in c.ghosts.iter().enumerate().take(9) {
                    if id.is_empty() || id == "minecraft:air" {
                        continue;
                    }
                    let [x, y] = [ox + 8.0 + 18.0 * i as f32, oy + 53.0];
                    game.native_item(batch, id, [x, y], 16.0);
                    let label = readable_number::format_i64(*n, 4);
                    let w = game.native_text_width(&label) * 0.5;
                    let start = batch.vertices.len();
                    game.native_text(batch, &label, [x + 16.0 - w, y + 12.0], 0xFFFFFF, true);
                    batch.scale_since(start, x + 16.0 - w, y + 12.0, 0.5, 0.5);
                }
            });
        }
        Some("io_port") => {
            // AE2's screens/io_port.json and IOPortScreen: the title, a cell and a drive either side of the
            // transfer direction button, the fullness and redstone modes on the left toolbar.
            game.native_sprite(batch, "ae2:gui/io_port", [0.0, 0.0, 176.0, 166.0], [ox, oy, 176.0, 166.0]);
            game.native_item(batch, "ae2:item_storage_cell_1k", [ox + 58.0, oy + 17.0], 16.0);
            game.native_item(batch, "ae2:drive", [ox + 102.0, oy + 17.0], 16.0);
            let d = |i: usize| data.get(i).copied().unwrap_or(0);
            draw_state_button(game, batch, [ox + 80.0, oy + 17.0], if d(1) == 1 { ICON_ARROW_LEFT } else { ICON_ARROW_RIGHT });
            draw_state_button(game, batch, [ox + TOOLBAR[0][0], oy + TOOLBAR[0][1]], ICON_FULLNESS[d(2).clamp(0, 2) as usize]);
            if d(3) >= 0 {
                draw_state_button(game, batch, [ox + TOOLBAR[1][0], oy + TOOLBAR[1][1]], ICON_REDSTONE[d(3).clamp(0, 3) as usize]);
            }
        }
        Some("pattern_access_terminal") => {
            // The frame without its printed rows; the rows drawn as the list has them.
            game.native_sprite(batch, "ae2:gui/patternaccessterminal", [0.0, 0.0, 195.0, 17.0], [ox, oy, 195.0, 17.0]);
            game.native_sprite(batch, "ae2:gui/patternaccessterminal", [0.0, 125.0, 195.0, 95.0], [ox, oy + 125.0, 195.0, 95.0]);
            game.native_text(batch, "Pattern Access", [ox + 8.0, oy + 6.0], 0x404040, false);
            game.native_text(batch, "Inventory", [ox + 8.0, oy + 129.0], 0x404040, false);
            // The search box (AE2's at 104, 4).
            let (search, focus) = CLIENT.with_borrow(|c| (c.search.clone(), c.search_focus));
            game.native_fill(batch, [ox + 103.0, oy + 3.0, 67.0, 14.0], if focus { 0xFFFFFFFF } else { 0xFFA0A0A0 });
            game.native_fill(batch, [ox + 104.0, oy + 4.0, 65.0, 12.0], 0xFF000000);
            game.native_text(batch, &if focus { format!("{search}_") } else { search }, [ox + 106.0, oy + 6.0], 0xFFFFFF, false);
            if data.first().copied().unwrap_or(0) == 0 {
                game.native_text(batch, "Network offline", [ox + 8.0, oy + 30.0], 0xFF5555, true);
                return;
            }
            draw_state_button(game, batch, [ox + TOOLBAR[0][0], oy + TOOLBAR[0][1]], PAT_BUTTON[data.get(1).copied().unwrap_or(0).clamp(0, 2) as usize].0);
            let rows = CLIENT.with_borrow(provider_rows_shown);
            let scroll = CLIENT.with_borrow(|c| c.scroll).min(rows.len().saturating_sub(6));
            for (r, row) in rows.iter().skip(scroll).take(6).enumerate() {
                let y = oy + 17.0 + 18.0 * r as f32;
                match row {
                    ProviderRow::Group(name, n) => {
                        game.native_sprite(batch, "ae2:gui/patternaccessterminal", [0.0, 17.0, 195.0, 18.0], [ox, y, 195.0, 18.0]);
                        game.native_text(batch, &format!("{name} ({n})"), [ox + 10.0, y + 5.0], 0x404040, false);
                    }
                    ProviderRow::Slots(_, slots) => {
                        game.native_sprite(batch, "ae2:gui/patternaccessterminal", [0.0, 35.0, 195.0, 18.0], [ox, y, 195.0, 18.0]);
                        for (i, id) in slots.iter().enumerate().take(9) {
                            if !id.is_empty() {
                                draw_key(game, batch, id, [ox + 8.0 + 18.0 * i as f32, y + 1.0], 16.0);
                            }
                        }
                    }
                }
            }
        }
        Some("sky_stone_chest" | "smooth_sky_stone_chest") => {
            game.native_sprite(batch, "ae2:gui/skychest", [0.0, 0.0, 176.0, 195.0], [ox, oy, 176.0, 195.0]);
        }
        Some("molecular_assembler") => {
            game.native_sprite(batch, "ae2:gui/molecular_assembler", [0.0, 0.0, 176.0, 197.0], [ox, oy, 176.0, 197.0]);
            // AE2's crafting grid slots (drawn in code, not in the texture).
            for i in 0..9 {
                draw_slot(game, batch, ox + 29.0 + 18.0 * (i % 3) as f32, oy + 30.0 + 18.0 * (i / 3) as f32);
            }
            // The crafting grid (AE2's MACHINE_CRAFTING_GRID at 29, 30: the ingredients it holds, drawn on the
            // panel as AE2 does) and the item it makes (MACHINE_OUTPUT at 126, 48).
            CLIENT.with_borrow(|c| {
                for i in 0..9 {
                    let (x, y) = (ox + 29.0 + 18.0 * (i % 3) as f32, oy + 30.0 + 18.0 * (i / 3) as f32);
                    if let Some((id, n)) = c.assembler.get(i).filter(|(id, _)| id != "minecraft:air") {
                        game.native_item(batch, id, [x, y], 16.0);
                        if *n > 1 {
                            let label = n.to_string();
                            let w = game.native_text_width(&label);
                            game.native_text(batch, &label, [x + 17.0 - w, y + 9.0], 0xFFFFFF, true);
                        }
                    }
                }
                if let Some((id, _)) = c.assembler.get(9).filter(|(id, _)| id != "minecraft:air") {
                    game.native_item(batch, id, [ox + 126.0, oy + 48.0], 16.0);
                }
            });
            // The progress bar fills upwards beside the output.
            let (done, total) = (data.first().copied().unwrap_or(0), data.get(1).copied().unwrap_or(0).max(1));
            let h = 18.0 * done.clamp(0, total) as f32 / total as f32;
            if h > 0.0 {
                game.native_sprite(batch, "ae2:gui/molecular_assembler", [148.0, 219.0 - h, 6.0, h], [ox + 148.0, oy + 66.0 - h, 6.0, h]);
            }
        }
        Some("controller") => {
            let d = |i: usize| data.get(i).copied().unwrap_or(0);
            let state = if d(0) != 0 { "§2Online" } else { "§4Offline" };
            let lines = [
                format!("Network: {state}"),
                format!("Stored power: {} / {} kAE", d(3), d(4)),
                format!("Usage: {:.1} AE/t", d(5) as f32 / 10.0),
                format!("Channels in use: {}", d(1)),
                format!("Devices: {}", d(2)),
            ];
            for (i, line) in lines.iter().enumerate() {
                game.native_text(batch, line, [ox + 8.0, oy + 18.0 + 11.0 * i as f32], 0x404040, false);
            }
        }
        Some("inscriber") => {
            game.native_sprite(batch, "ae2:gui/inscriber", [0.0, 0.0, 176.0, 176.0], [ox, oy, 176.0, 176.0]);
            let exporting = data.get(2).copied().unwrap_or(0) != 0;
            draw_state_button(game, batch, [ox + TOOLBAR[0][0], oy + TOOLBAR[0][1]], if exporting { [128.0, 96.0] } else { [112.0, 96.0] });
            // The progress bar fills downwards (AE2's sprite under the panel, 6 by 18).
            let (done, total) = (data.first().copied().unwrap_or(0), data.get(1).copied().unwrap_or(0).max(1));
            let h = 18.0 * done.clamp(0, total) as f32 / total as f32;
            if h > 0.0 {
                game.native_sprite(batch, "ae2:gui/inscriber", [135.0, 177.0, 6.0, h], [ox + 135.0, oy + 39.0, 6.0, h]);
            }
        }
        Some("terminal") | Some("crafting_terminal") => {
            // Head, the row strip once per row, then the foot with the inventory (the crafting terminal's foot is
            // the plain panel under its crafting grid).
            let rows = term_rows();
            draw_upgrade_panel(game, batch, ox, oy, 195.0, 14.0 + 18.0 * VIEW_CELLS as f32, VIEW_CELLS);
            // AE2's terminalStyle: header, first row, middle rows, last row, then the bottom (base_terminal.json;
            // the crafting terminal's from crafting.png, its bottom holding the grid and the inventory).
            let crafting = block != "ae2:terminal";
            let tex = if crafting { "ae2:gui/crafting" } else { "ae2:gui/terminal" };
            game.native_sprite(batch, tex, [0.0, 0.0, 195.0, 17.0], [ox, oy, 195.0, 17.0]);
            for r in 0..rows {
                let y = oy + 17.0 + 18.0 * r as f32;
                let src = if r == 0 { 17.0 } else if r + 1 == rows { 53.0 } else { 35.0 };
                game.native_sprite(batch, tex, [0.0, src, 195.0, 18.0], [ox, y, 195.0, 18.0]);
            }
            let foot = oy + 17.0 + 18.0 * rows as f32;
            let bottom = if crafting { 170.0 } else { 97.0 };
            game.native_sprite(batch, tex, [0.0, 71.0, 195.0, bottom], [ox, foot, 195.0, bottom]);
            if crafting {
                game.native_text(batch, "Crafting Terminal", [ox + 8.0, foot + 2.0], 0x404040, false);
                for (x, icon) in [(STASH_X[0], [0.0, 48.0]), (STASH_X[1], [16.0, 48.0])] {
                    let at = [ox + x, foot + STASH_Y];
                    let [bx, by] = ICON_BUTTON_BACKGROUND;
                    game.native_sprite(batch, "ae2:gui/states", [bx, by, 16.0, 16.0], [at[0], at[1], 8.0, 8.0]);
                    game.native_sprite(batch, "ae2:gui/states", [icon[0], icon[1], 16.0, 16.0], [at[0], at[1], 8.0, 8.0]);
                }
            }
            let online = data.first().copied().unwrap_or(0) != 0;
            draw_terminal_items(game, batch, ox, oy, online);
        }
        _ => {}
    }
}

#[derive(Clone)]
enum ProviderRow {
    Group(String, usize),
    Slots(usize, Vec<String>),
}

/// The pattern access terminal's rows the search keeps (a group whose name has the words, with its providers).
fn provider_rows_shown(c: &Client) -> Vec<ProviderRow> {
    let words: Vec<String> = c.search.to_lowercase().split_whitespace().map(str::to_owned).collect();
    if words.is_empty() {
        return c.provider_rows.clone();
    }
    let hits = |text: &str| words.iter().all(|w| text.contains(w.as_str()));
    let mut out = Vec::new();
    let mut group: Option<&ProviderRow> = None;
    let mut name_hit = false;
    let mut group_shown = false;
    for row in &c.provider_rows {
        match row {
            ProviderRow::Group(name, _) => {
                group = Some(row);
                name_hit = hits(&name.to_lowercase());
                group_shown = false;
            }
            ProviderRow::Slots(_, slots) => {
                let slot_hit = slots.iter().any(|id| !id.is_empty() && hits(&item_words(id).to_lowercase()));
                if name_hit || slot_hit {
                    if !group_shown && let Some(g) = group {
                        out.push(g.clone());
                        group_shown = true;
                    }
                    out.push(row.clone());
                }
            }
        }
    }
    out
}

/// The items a terminal's grid shows, as indexes into what the server sent: the search's and the view's, sorted.
fn visible(c: &Client) -> std::rc::Rc<Vec<usize>> {
    if let Some(v) = c.view.borrow().as_ref() {
        return v.clone();
    }
    let v = std::rc::Rc::new(make_visible(c));
    *c.view.borrow_mut() = Some(v.clone());
    v
}

fn make_visible(c: &Client) -> Vec<usize> {
    // RepoSearch.fromString: `|` between alternatives, spaces between what each needs; @mod, #tooltip, $tag, *id, or
    // the name.
    let query = c.search.to_lowercase();
    let keeps = |id: &str| {
        let bare = id.trim_start_matches('~');
        let ns = bare.split_once(':').map_or("minecraft", |(n, _)| n);
        query.split('|').any(|part| {
            part.split_whitespace().all(|w| {
                if let Some(m) = w.strip_prefix('@') {
                    ns.contains(m)
                } else if let Some(t) = w.strip_prefix('#') {
                    c.search_tips.get(id).is_some_and(|tips| tips.contains(t))
                } else if let Some(t) = w.strip_prefix('$') {
                    c.search_tags.get(bare).is_some_and(|tags| tags.iter().any(|tag| tag.contains(t)))
                } else if let Some(t) = w.strip_prefix('*') {
                    bare.contains(t)
                } else {
                    c.search_names.get(id).map_or_else(|| item_words(bare).to_lowercase().contains(w), |name| name.contains(w))
                }
            })
        })
    };
    let mut out: Vec<usize> = (0..c.items.len())
        .filter(|&i| {
            let (id, n) = &c.items[i];
            *n >= 0 && keeps(id) && match c.view_mode {
                1 => *n > 0,
                2 => *n == 0,
                _ => true,
            } && match c.type_filter {
                1 => !id.starts_with('~'),
                2 => id.starts_with('~'),
                _ => true,
            }
        })
        .collect();
    // SortOrder: name, amount, mod, by SortDir.
    match c.sort {
        1 => out.sort_by(|&a, &b| c.items[a].1.cmp(&c.items[b].1).then_with(|| c.items[a].0.cmp(&c.items[b].0))),
        2 => out.sort_by_key(|&i| (c.items[i].0.trim_start_matches('~').split_once(':').map_or(String::new(), |(n, _)| n.to_owned()), item_words(c.items[i].0.trim_start_matches('~')))),
        _ => out.sort_by_key(|&i| item_words(c.items[i].0.trim_start_matches('~'))),
    }
    if c.sort_desc {
        out.reverse();
    }
    // Pin auto-crafted items: those first, in the order they were asked for.
    if !c.pinned.is_empty() {
        out.sort_by_key(|&i| c.pinned.iter().position(|p| *p == c.items[i].0).unwrap_or(usize::MAX));
    }
    out
}

/// MEStorageScreen's left toolbar (VerticalButtonBar: 16 pixels, 4 between): sort by, view, type filter, sort order,
/// terminal settings, terminal style.
const TOOLBAR: [[f32; 2]; 6] = [[-20.0, 8.0], [-20.0, 28.0], [-20.0, 48.0], [-20.0, 68.0], [-20.0, 88.0], [-20.0, 108.0]];
/// The crafting status tab (screens/terminals/base_terminal.json).
const STATUS_TAB: [f32; 4] = [170.0, -4.0, 25.0, 22.0];

/// SettingToggleButton's icons and words for the toolbar, as they stand now.
fn toolbar_entries(c: &Client) -> [([f32; 2], &'static str, &'static str); 6] {
    [
        (
            [[0.0, 64.0], [16.0, 64.0], [96.0, 64.0]][usize::from(c.sort.min(2))],
            "Sort By",
            ["Item name", "Number of items", "Mod"][usize::from(c.sort.min(2))],
        ),
        (
            [[32.0, 16.0], [0.0, 16.0], [48.0, 16.0]][usize::from(c.view_mode.min(2))],
            "View",
            ["Stored / Craftable", "Stored Items", "Craftable"][usize::from(c.view_mode.min(2))],
        ),
        (
            [[160.0, 16.0], [128.0, 16.0], [144.0, 16.0]][usize::from(c.type_filter.min(2))],
            "Filter Types",
            ["Show All", "Show Items only", "Show Fluids only"][usize::from(c.type_filter.min(2))],
        ),
        (if c.sort_desc { [16.0, 48.0] } else { [0.0, 48.0] }, "Sort Order", if c.sort_desc { "Descending" } else { "Ascending" }),
        ([32.0, 64.0], "Terminal Settings", ""),
        (
            [[0.0, 208.0], [16.0, 208.0], [32.0, 208.0], [48.0, 208.0]][usize::from(c.term_style.min(3))],
            "Terminal Style",
            ["Small Centered Terminal", "Medium Centered Terminal", "Tall Centered Terminal", "Full-Height Terminal"][usize::from(c.term_style.min(3))],
        ),
    ]
}

/// The toolbar's and the status tab's tooltips, over everything.
fn toolbar_tooltip(game: &Game<'_>, batch: &mut HudBatch, mouse: [f32; 2]) {
    if overlay_shown() {
        return;
    }
    let Some(origin) = CLIENT.with_borrow(|c| c.term_origin) else { return };
    let at = [mouse[0] - origin[0], mouse[1] - origin[1]];
    let inside = |r: [f32; 4]| at[0] >= r[0] && at[0] < r[0] + r[2] && at[1] >= r[1] && at[1] < r[1] + r[3];
    let legacy = |l: &str| (horizon_client::text::Text::legacy(l), u32::MAX);
    if inside(STATUS_TAB) {
        game.draw_tooltip_lines(batch, &[legacy("Crafting Status")], mouse[0], mouse[1]);
        return;
    }
    // MEStorageScreen.updateSearch: what the box understands.
    if inside(SEARCH_BOX) && !CLIENT.with_borrow_mut(|c| settings(c)[6]) {
        let lines: Vec<_> = ["Search in Name", "Use @ to search by mod (@ae2)", "Use $ to search by tags ($ingot)", "Use # to search in tooltips (#looting)", "Use * to search by id (*cell)"]
            .iter()
            .map(|l| legacy(l))
            .collect();
        game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
        return;
    }
    let entries = CLIENT.with_borrow(toolbar_entries);
    for (b, (_, title, value)) in TOOLBAR.iter().zip(entries) {
        if inside([b[0], b[1], 16.0, 16.0]) {
            let mut lines = vec![legacy(title)];
            if !value.is_empty() {
                lines.push(legacy(&format!("§7{value}")));
            }
            game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
            return;
        }
    }
}

/// The search box in the header.
const SEARCH_BOX: [f32; 4] = [80.0, 4.0, 89.0, 12.0];

/// The names, tags and tooltips the next listing's search reads (it has no game to ask).
fn search_prepare(game: &Game<'_>) {
    CLIENT.with_borrow_mut(|c| {
        if c.view.borrow().is_some() {
            return;
        }
        let tips = c.search.contains('#');
        let tags = c.search.contains('$');
        let ids: Vec<String> = c.items.iter().map(|(id, _)| id.clone()).collect();
        for id in ids {
            c.search_names.entry(id.clone()).or_insert_with(|| key_words(game, &id).to_lowercase());
            if tips && !c.search_tips.contains_key(&id) {
                let lines = game.stack_tooltip(&ItemStack::new(&icon_of(&id), 1));
                let text = lines.iter().map(|l| l.0.plain().to_lowercase()).collect::<Vec<_>>().join(" ");
                c.search_tips.insert(id, text);
            }
        }
        if tags && c.search_tags.is_empty() {
            for (tag, items) in &game.res.items().item_tags {
                for item in items {
                    c.search_tags.entry(item.to_string()).or_default().push(tag.to_lowercase());
                }
            }
        }
    });
}

/// A terminal's grid of network items (or "Network offline") and its scroll bar's handle.
fn draw_terminal_items(game: &Game<'_>, batch: &mut HudBatch, ox: f32, oy: f32, online: bool) {
    search_prepare(game);
    // The title, as AE2 names its terminals, the inventory's label, and the search box.
    game.native_text(batch, "Terminal", [ox + 8.0, oy + 6.0], 0x404040, false);
    if let Some(inv) = game.native_screen_inventory_y() {
        game.native_text(batch, "Inventory", [ox + 8.0, oy + inv - 11.0], 0x404040, false);
    }
    let (search, focus, external, shown_of, entries, jobs) = CLIENT.with_borrow_mut(|c| {
        let external = settings(c)[6];
        let all = c.items.iter().filter(|(_, n)| *n >= 0).count();
        (c.search.clone(), c.search_focus, external, (visible(c).len(), all), toolbar_entries(c), c.jobs.len())
    });
    let [sx, sy, sw, sh] = SEARCH_BOX;
    if external {
        // updateSearch: JEI has the search; "entriesShown" says what it leaves.
        if shown_of.0 != shown_of.1 {
            let text = format!("Showing {} of {}", shown_of.0, shown_of.1);
            let (x, y) = (ox + 195.0 - 26.0 - game.native_text_width(&text) * 0.6, oy + 8.0);
            let start = batch.vertices.len();
            game.native_text(batch, &text, [x, y], TEXT, false);
            batch.scale_since(start, x, y, 0.6, 0.6);
        }
    } else {
        // AETextField: guis/text_field.png's ends and middle, its focused look 24 down.
        let v = if focus { 24.0 } else { 0.0 };
        let sprite = "ae2:gui/text_field";
        game.native_sprite(batch, sprite, [0.0, v, 1.0, 12.0], [ox + sx, oy + sy, 1.0, 12.0]);
        game.native_sprite(batch, sprite, [1.0, v, (sw - 2.0).min(126.0), 12.0], [ox + sx + 1.0, oy + sy, sw - 2.0, 12.0]);
        game.native_sprite(batch, sprite, [127.0, v, 1.0, 12.0], [ox + sx + sw - 1.0, oy + sy, 1.0, 12.0]);
        if search.is_empty() && !focus {
            game.native_text(batch, "Search...", [ox + sx + 2.0, oy + sy + 2.0], 0xAEAEAE, false);
        } else {
            let shown = if focus { format!("{search}_") } else { search };
            game.native_text(batch, &shown, [ox + sx + 2.0, oy + sy + 2.0], 0xFFFFFF, true);
        }
    }
    let _ = sh;
    // The toolbar, and the crafting status tab with how many jobs are running.
    for (at, (icon, _, _)) in TOOLBAR.iter().zip(entries) {
        draw_state_button(game, batch, [ox + at[0], oy + at[1]], icon);
    }
    let [tx, ty, tw, th] = STATUS_TAB;
    game.native_sprite(batch, "ae2:gui/states", [128.0, 192.0, tw, th], [ox + tx, oy + ty, tw, th]);
    game.native_sprite(batch, "ae2:gui/states", [48.0, 144.0, 16.0, 16.0], [ox + tx + 4.0, oy + ty + 3.0, 16.0, 16.0]);
    let label = jobs.to_string();
    let (lx, ly) = (ox + tx + 4.0 + 16.0 - game.native_text_width(&label) * 0.5, oy + ty + 3.0 + 12.0);
    let start = batch.vertices.len();
    game.native_text(batch, &label, [lx, ly], 0xFFFFFF, true);
    batch.scale_since(start, lx, ly, 0.5, 0.5);
    // Offline (no power, or no channel), the grid stays empty with AE2's notice in its middle. What the server sent
    // still shows while the screen's state catches up (a terminal opened a moment after placing it).
    if !online && CLIENT.with_borrow(|c| c.items.iter().all(|(_, n)| *n < 0)) {
        // AE2 dims the grid of a terminal that isn't connected.
        let rows = CLIENT.with_borrow(|c| c.rows.max(3)) as f32;
        game.native_fill(batch, [ox + TERM_GRID[0], oy + TERM_GRID[1], 18.0 * TERM_COLS as f32 - 2.0, 18.0 * rows - 2.0], 0xB0101010);
        let w = game.native_text_width("Network offline");
        game.native_text(batch, "Network offline", [ox + 8.0 + 81.0 - w / 2.0, oy + 18.0 + 9.0 * rows - 4.0], 0xFF6060, true);
        return;
    }
    {
        {
            let mut dragged = None;
            CLIENT.with_borrow(|c| {
                let start = c.scroll * TERM_COLS;
                let shown = visible(c);
                for (i, &n) in shown.iter().skip(start).take(TERM_COLS * c.rows.max(3)).enumerate() {
                    let (id, amount) = &c.items[n];
                    let x = ox + TERM_GRID[0] + (i % TERM_COLS) as f32 * 18.0;
                    let y = oy + TERM_GRID[1] + (i / TERM_COLS) as f32 * 18.0;
                    draw_key(game, batch, id, [x, y], 16.0);
                    if c.pinned.contains(id) && c.jobs.iter().any(|j| j.0 == *id) {
                        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
                        let frame = (START.get_or_init(std::time::Instant::now).elapsed().as_millis() / 100 % 12) as f32;
                        game.native_sprite(batch, "ae2:gui/hz/molecular_assembler_lights", [2.0, 2.0 + frame * 16.0, 12.0, 12.0], [x - 1.0, y - 1.0, 18.0, 18.0]);
                    }
                    // The cell as a widget (the narrator, horizon-ctl's `ui` and `click`), named as its tooltip is.
                    if !id.starts_with('~') {
                        let name = format!("Network {}", game.pack.names.display_item(id));
                        horizon_client::widgets::record_as(&name, [x, y, 16.0, 16.0], true, horizon_client::widgets::Kind::Button);
                    }
                    // AE2's amounts: small, in the slot's bottom right; "Craft" for what only a pattern makes; fluids
                    // in buckets.
                    let label = if *amount == 0 {
                        "Craft".to_owned()
                    } else if id.starts_with('~') {
                        key_amount(id, (*amount).max(0), super::port_ae2_core::AmountFormat::Slot)
                    } else {
                        readable_number::format_i64((*amount).max(0), 4)
                    };
                    let w = game.native_text_width(&label) * 0.5;
                    let start = batch.vertices.len();
                    game.native_text(batch, &label, [x + 16.0 - w, y + 12.0], 0xFFFFFF, true);
                    batch.scale_since(start, x + 16.0 - w, y + 12.0, 0.5, 0.5);
                }
                // The scroll bar's handle, grey when there's nothing to scroll.
                let rows = shown.len().div_ceil(TERM_COLS);
                let height = 18.0 * c.rows.max(3) as f32 - 2.0;
                let most = rows.saturating_sub(c.rows.max(3));
                draw_scrollbar(game, batch, [ox + 175.0, oy + 18.0], height, c.scroll.min(most), most);
                dragged = (c.scroll_drag == 4).then(|| scrollbar_row(c.mouse[1], 18.0, height, most));
            });
            if let Some(row) = dragged {
                CLIENT.with_borrow_mut(|c| c.scroll = row);
            }
        }
    }
}

/// `native_screen_click`: a press on the terminal's grid becomes a menu button for the server.
pub fn screen_click(game: &Game<'_>, block: &str, at: [f32; 2], right: bool, button: &mut Option<i32>) {
    screen_click_inner(game, block, at, right, button);
    // The left toolbar lies outside the window, where Horizon sends no menu button: its buttons go as a UI
    // message instead (`button N`, run on the viewed block's entity).
    if at[0] < 0.0
        && let Some(id) = button.take()
    {
        CLIENT.with_borrow_mut(|c| c.ui_out.push(format!("button {id}")));
    }
}

fn screen_click_inner(game: &Game<'_>, block: &str, at: [f32; 2], right: bool, button: &mut Option<i32>) {
    if block == "ae2:crafting_terminal"
        && let Some(b) = stash_button(at, CLIENT.with_borrow(|c| c.rows))
    {
        *button = Some(b);
        return;
    }
    let shift_held = game.keys.contains(&winit::keyboard::KeyCode::ShiftLeft) || game.keys.contains(&winit::keyboard::KeyCode::ShiftRight);
    let on = |r: [f32; 4]| at[0] >= r[0] && at[0] < r[0] + r[2] && at[1] >= r[1] && at[1] < r[1] + r[3];
    if has_priority(block) && on([TOOLBAR[0][0], TOOLBAR[0][1], 18.0, 18.0]) {
        let p = CLIENT.with_borrow(|c| c.priority);
        open_dialog(DialogKind::Priority, p.to_string());
        return;
    }
    if canon(block) == "ae2:pattern_provider" {
        let inside = |r: [f32; 2]| at[0] >= r[0] && at[0] < r[0] + 16.0 && at[1] >= r[1] && at[1] < r[1] + 16.0;
        if let Some((b, _)) = pp_toolbar().into_iter().find(|(_, r)| inside(*r)) {
            *button = Some(b);
        } else if on([PP_PRIORITY[0], PP_PRIORITY[1], 22.0, 22.0]) {
            let p = CLIENT.with_borrow(|c| c.priority);
            open_dialog(DialogKind::Priority, p.to_string());
        }
        return;
    }
    match block {
        "ae2:import_bus" | "ae2:export_bus" | "ae2:storage_bus" | "ae2:formation_plane" => {
            let cells = if matches!(block, "ae2:storage_bus" | "ae2:formation_plane") { 63 } else { 9 };
            let d = |n: usize| CLIENT.with_borrow(|c| c.screen_data.get(n).copied().unwrap_or(0));
            let (first, buttons) = bus_buttons(block, &d);
            for (n, (id, _, _, _)) in buttons.iter().enumerate() {
                let at = bar_place(first + n);
                if on([at[0], at[1], 16.0, 16.0]) {
                    *button = Some(*id);
                }
            }
            if on([8.0, 29.0, 162.0, 18.0 * (cells / 9) as f32]) {
                let cell = ((at[1] - 29.0) / 18.0) as i32 * 9 + ((at[0] - 8.0) / 18.0) as i32;
                *button = Some(GHOST + (cell << 3) + if right { RIGHT } else { 0 });
            }
            return;
        }
        "ae2:condenser" => {
            if on([128.0, 52.0, 18.0, 18.0]) {
                *button = Some(TOGGLE_MODE);
            }
            return;
        }
        "ae2:cell_workbench" => {
            let fuzzy = CLIENT.with_borrow(|c| c.screen_data.get(1).copied().unwrap_or(-1)) >= 0;
            if let Some((b, _)) = wb_toolbar(fuzzy).into_iter().find(|(_, r)| on([r[0], r[1], 16.0, 16.0])) {
                *button = Some(b);
            } else if on([8.0, 29.0, 162.0, 126.0]) {
                let cell = ((at[1] - 29.0) / 18.0) as i32 * 9 + ((at[0] - 8.0) / 18.0) as i32;
                *button = Some(GHOST + (cell << 3));
            }
            return;
        }
        "ae2:energy_level_emitter" => {
            level_widget_click(at, right, 133.0, button);
            return;
        }
        _ => {}
    }
    if matches!(block, "ae2:terminal" | "ae2:crafting_terminal" | "ae2:pattern_encoding_terminal") {
        let inside = |x: f32, y: f32, w: f32, h: f32| at[0] >= x && at[0] < x + w && at[1] >= y && at[1] < y + h;
        let [sx, sy, sw, sh] = SEARCH_BOX;
        let in_search = inside(sx, sy, sw, sh) && !CLIENT.with_borrow_mut(|c| settings(c)[6]);
        CLIENT.with_borrow_mut(|c| {
            c.search_focus = in_search;
            // Right-click clears it, as AE2's does.
            if in_search && right {
                c.search.clear();
                c.view.take();
            }
        });
        if in_search {
            return;
        }
        if inside(STATUS_TAB[0], STATUS_TAB[1], STATUS_TAB[2], STATUS_TAB[3]) {
            // MEStorageScreen.showCraftingStatus.
            CLIENT.with_borrow_mut(|c| {
                c.show_jobs = true;
                c.status_cpu = usize::MAX;
                c.status_scroll = 0;
                c.status_list_scroll = 0;
            });
            return;
        }
        for (i, b) in TOOLBAR.iter().enumerate() {
            if inside(b[0], b[1], 16.0, 16.0) {
                CLIENT.with_borrow_mut(|c| {
                    settings(c);
                    // SettingToggleButton: right the one before.
                    let cycle = |v: u8, n: u8| if right { (v + n - 1) % n } else { (v + 1) % n };
                    match i {
                        0 => c.sort = cycle(c.sort, 3),
                        1 => c.view_mode = cycle(c.view_mode, 3),
                        2 => c.type_filter = cycle(c.type_filter, 3),
                        3 => c.sort_desc = !c.sort_desc,
                        4 => c.settings_open = true,
                        _ => {
                            c.term_style = cycle(c.term_style, 4);
                            c.relayout = true;
                        }
                    }
                    save_settings(c);
                    c.view.take();
                    c.scroll = 0;
                });
                return;
            }
        }
        let rows = CLIENT.with_borrow(|c| c.rows.max(3)) as f32;
        if inside(175.0, 18.0, 12.0, 18.0 * rows - 2.0) {
            CLIENT.with_borrow_mut(|c| c.scroll_drag = 4);
            return;
        }
        let over_grid = at[0] >= TERM_GRID[0] && at[0] < 170.0 && at[1] >= TERM_GRID[1] && at[1] < TERM_GRID[1] + 18.0 * rows;
        if over_grid && CLIENT.with_borrow(|c| c.show_jobs) {
            return;
        }
    }
    if block == "ae2:interface" && on([TOOLBAR[0][0], TOOLBAR[0][1], 16.0, 16.0]) && CLIENT.with_borrow(|c| c.screen_data.get(1).copied().unwrap_or(-1) >= 0) {
        *button = Some(EMIT_FUZZY);
        return;
    }
    if block == "ae2:level_emitter" && on([TOOLBAR[1][0], TOOLBAR[1][1], 16.0, 16.0]) && CLIENT.with_borrow(|c| c.screen_data.get(4).copied().unwrap_or(-1) >= 0) {
        *button = Some(EMIT_FUZZY);
        return;
    }
    if block == "ae2:inscriber" && on([TOOLBAR[0][0], TOOLBAR[0][1], 16.0, 16.0]) {
        *button = Some(INSCRIBER_EXPORT);
        return;
    }
    if block == "ae2:level_emitter" {
        if level_widget_click(at, right, 103.0, button) {
            return;
        }
        if at[0] >= 137.0 && at[0] < 153.0 && at[1] >= 40.0 && at[1] < 56.0 {
            *button = Some(GHOST + if right { RIGHT } else { 0 });
        }
        return;
    }
    if canon(&block) == "ae2:interface" {
        if at[1] >= 53.0 && at[1] < 69.0 && at[0] >= 8.0 && at[0] < 170.0 {
            let cell = ((at[0] - 8.0) / 18.0) as i32;
            // Shift-click: AE2's stock amount for the config cell.
            let ghost = CLIENT.with_borrow(|c| c.ghosts.get(cell as usize).cloned()).filter(|(id, _)| !id.is_empty() && id != "minecraft:air");
            if shift_held && let Some((id, n)) = ghost {
                let text = amount_text(&id, n);
                open_dialog(DialogKind::Stock(cell as usize, id), text);
                return;
            }
            *button = Some(GHOST + (cell << 3) + if right { RIGHT } else { 0 });
        }
        return;
    }
    if block == "ae2:io_port" {
        let on = |r: [f32; 2]| at[0] >= r[0] && at[0] < r[0] + 16.0 && at[1] >= r[1] && at[1] < r[1] + 16.0;
        if on([80.0, 17.0]) {
            *button = Some(TOGGLE_MODE);
        } else if on(TOOLBAR[0]) {
            *button = Some(IO_FULLNESS);
        } else if on(TOOLBAR[1]) {
            *button = Some(IO_REDSTONE);
        }
        return;
    }
    if block == "ae2:pattern_access_terminal" {
        if on([TOOLBAR[0][0], TOOLBAR[0][1], 16.0, 16.0]) {
            *button = Some(PAT_SHOW);
            return;
        }
        let in_search = at[0] >= 104.0 && at[0] < 169.0 && at[1] >= 4.0 && at[1] < 16.0;
        CLIENT.with_borrow_mut(|c| {
            c.search_focus = in_search;
            if in_search && right {
                c.search.clear();
            }
        });
        if in_search || at[0] < 8.0 || at[0] >= 170.0 || at[1] < 17.0 || at[1] >= 125.0 {
            return;
        }
        let rows = CLIENT.with_borrow(provider_rows_shown);
        let scroll = CLIENT.with_borrow(|c| c.scroll).min(rows.len().saturating_sub(6));
        let r = ((at[1] - 17.0) / 18.0) as usize;
        if let Some(ProviderRow::Slots(index, _)) = rows.get(scroll + r) {
            let slot = ((at[0] - 8.0) / 18.0) as i32;
            let shift = game.keys.contains(&winit::keyboard::KeyCode::ShiftLeft) || game.keys.contains(&winit::keyboard::KeyCode::ShiftRight);
            *button = Some(PROVIDER_SLOT + ((*index as i32 * 9 + slot) << 2) + if shift { SHIFT } else { 0 });
        }
        return;
    }
    let mut shift_ids = 0;
    if block == "ae2:pattern_encoding_terminal" {
        // Below the terminal rows, positions move down with the rows the window fits.
        let extra = CLIENT.with_borrow(|c| c.encoder_extra);
        let low = [at[0], at[1] - extra];
        let inside = |b: [f32; 2], w: f32, h: f32| low[0] >= b[0] && low[0] < b[0] + w && low[1] >= b[1] && low[1] < b[1] + h;
        let crafting = CLIENT.with_borrow(|c| c.encoder_crafting);
        let mode = CLIENT.with_borrow(|c| c.encoder_mode);
        if inside(ENC_TABS[2], 20.0, 20.0) {
            *button = Some(SET_SMITHING);
            return;
        }
        if inside(ENC_TABS[3], 20.0, 22.0) {
            *button = Some(SET_STONECUTTING);
            return;
        }
        if mode == 3 && inside([52.0, 92.0], 64.0, 54.0) {
            let pick = ((low[1] - 92.0) / 18.0) as i32 * 4 + ((low[0] - 52.0) / 16.0) as i32;
            *button = Some(STONECUT_PICK + pick);
            return;
        }
        if inside(ENC_ENCODE, 16.0, 16.0) {
            *button = Some(ENCODE);
            return;
        }
        if inside(ENC_TABS[0], 20.0, 20.0) {
            *button = Some(SET_CRAFTING);
            return;
        }
        if inside(ENC_TABS[1], 20.0, 20.0) {
            *button = Some(SET_PROCESSING);
            return;
        }
        if inside(ENC_CLEAR[usize::from(mode == 1)], 9.0, 9.0) {
            *button = Some(CLEAR);
            return;
        }
        if mode == 0 && inside(ENC_SUBSTITUTE_AT, 9.0, 9.0) {
            *button = Some(ENC_SUBSTITUTE);
            return;
        }
        if mode == 1 && inside(ENC_CYCLE_AT, 9.0, 9.0) && CLIENT.with_borrow(encoder_outputs) > 1 {
            *button = Some(ENC_CYCLE);
            return;
        }
        let _ = crafting;
        if mode == 1 && inside([17.0, 92.0], 7.0, 52.0) {
            CLIENT.with_borrow_mut(|c| c.scroll_drag = 5);
            return;
        }
        if let Some(place) = (0..mode_cells(mode)).find(|&i| inside(ghost_cell_mode(i, mode), 16.0, 16.0)) {
            let cell = enc_visible(place, mode, CLIENT.with_borrow(|c| c.enc_scroll));
            // Shift-click on a processing cell: AE2's amount dialog.
            let ghost = CLIENT.with_borrow(|c| c.ghosts.get(cell).cloned()).filter(|(id, _)| !id.is_empty() && id != "minecraft:air");
            if mode == 1 && shift_held && let Some((id, n)) = ghost {
                let text = amount_text(&id, n);
                open_dialog(DialogKind::Amount(cell, id), text);
                return;
            }
            *button = Some(GHOST + ((cell as i32) << 3) + if right { RIGHT } else { 0 });
            return;
        }
        shift_ids = 8;
    } else if !matches!(block, "ae2:terminal" | "ae2:crafting_terminal") {
        return;
    }
    let [x, y] = [at[0] - TERM_GRID[0], at[1] - TERM_GRID[1]];
    if x < 0.0 || y < 0.0 || x >= TERM_COLS as f32 * 18.0 || y >= term_rows() as f32 * 18.0 {
        return;
    }
    let cell = CLIENT.with_borrow(|c| c.scroll * TERM_COLS) + (y / 18.0) as usize * TERM_COLS + (x / 18.0) as usize;
    // The cell's item as the server sent the list (the client filters and sorts it); an empty cell still takes
    // what the cursor holds (AE2 stores it wherever it is dropped on the grid).
    let index = CLIENT.with_borrow(|c| visible(c).get(cell).copied()).unwrap_or(EMPTY_CELL);
    let shift = shift_held;
    // A craftable entry: AE2 asks how many.
    if CLIENT.with_borrow(|c| c.items.get(index).is_some_and(|(_, n)| *n == 0)) && ask_craft(index) {
        return;
    }
    // Space held over the grid (AE2 reads the raw keyboard: a screen takes Space from the movement keys).
    let action = if game.physical_keys.contains(&winit::keyboard::KeyCode::Space) {
        MOVE_REGION
    } else {
        (if right { RIGHT } else { 0 }) | (if shift { SHIFT } else { 0 })
    };
    *button = Some((((index as i32) << 3) | action) + shift_ids);
    log::info!("ae2: terminal click at {at:?} -> item {index}, button {button:?}");
}

/// `native_screen_tooltip`: an item's name and full amount on the terminal's grid.
pub fn screen_tooltip(block: &str, at: [f32; 2], lines: &mut Vec<String>) {
    CLIENT.with_borrow_mut(|c| c.mouse = at);
    if block == "ae2:interface" && at[0] >= TOOLBAR[0][0] && at[0] < TOOLBAR[0][0] + 16.0 && at[1] >= TOOLBAR[0][1] && at[1] < TOOLBAR[0][1] + 16.0 {
        let mode = CLIENT.with_borrow(|c| c.screen_data.get(1).copied().unwrap_or(-1));
        if mode >= 0 {
            lines.push("Fuzzy Comparison".to_owned());
            lines.push(format!("§7{}", FUZZY_BUTTON[mode.clamp(0, 4) as usize].1));
            return;
        }
    }
    if block == "ae2:level_emitter" && at[0] >= TOOLBAR[1][0] && at[0] < TOOLBAR[1][0] + 16.0 && at[1] >= TOOLBAR[1][1] && at[1] < TOOLBAR[1][1] + 16.0 {
        let mode = CLIENT.with_borrow(|c| c.screen_data.get(4).copied().unwrap_or(-1));
        if mode >= 0 {
            lines.push("Fuzzy Comparison".to_owned());
            lines.push(format!("§7{}", FUZZY_BUTTON[mode.clamp(0, 4) as usize].1));
            return;
        }
    }
    if block == "ae2:pattern_access_terminal" && at[0] >= TOOLBAR[0][0] && at[0] < TOOLBAR[0][0] + 16.0 && at[1] >= TOOLBAR[0][1] && at[1] < TOOLBAR[0][1] + 16.0 {
        let show = CLIENT.with_borrow(|c| c.screen_data.get(1).copied().unwrap_or(0));
        lines.push("Display Mode".to_owned());
        lines.push(format!("§7{}", PAT_BUTTON[show.clamp(0, 2) as usize].1));
        return;
    }
    if block == "ae2:inscriber" && at[0] >= TOOLBAR[0][0] && at[0] < TOOLBAR[0][0] + 16.0 && at[1] >= TOOLBAR[0][1] && at[1] < TOOLBAR[0][1] + 16.0 {
        let on = CLIENT.with_borrow(|c| c.screen_data.get(2).copied().unwrap_or(0) != 0);
        lines.push("Auto-Export".to_owned());
        lines.push(format!("§7{}", if on { "Produced items will be auto-exported through any extraction side." } else { "Produced items will be not be auto-exported." }));
        return;
    }
    if block == "ae2:pattern_encoding_terminal" {
        let (mode, scroll, extra, empty) = CLIENT.with_borrow(|c| (c.encoder_mode, c.enc_scroll, c.encoder_extra, c.ghosts.clone()));
        let low = [at[0], at[1] - extra];
        if mode == 1 && let Some(row) = (0..3).find(|r| low[0] >= 110.0 && low[0] < 126.0 && low[1] >= 93.0 + 18.0 * *r as f32 && low[1] < 109.0 + 18.0 * *r as f32) {
            let j = scroll + row;
            let unset = empty.get(enc_output(j)).is_none_or(|(id, _)| id.is_empty() || id == "minecraft:air");
            if unset {
                if j == 0 {
                    lines.push("Primary Processing Result".to_owned());
                    lines.push("§8Can be requested through the automated crafting system.".to_owned());
                } else {
                    lines.push("Secondary Processing Result".to_owned());
                    lines.push("§8Can not be directly requested through the automated crafting system, but will be used before stored items in multi-step recipes.".to_owned());
                }
                return;
            }
        }
        if mode == 0 && low[0] >= ENC_SUBSTITUTE_AT[0] && low[0] < ENC_SUBSTITUTE_AT[0] + 9.0 && low[1] >= ENC_SUBSTITUTE_AT[1] && low[1] < ENC_SUBSTITUTE_AT[1] + 9.0 {
            let on = CLIENT.with_borrow(|c| c.screen_data.get(2).copied().unwrap_or(0) != 0);
            lines.push(if on { "Substitutions Enabled" } else { "Substitutions Disabled" }.to_owned());
            lines.push(format!("§7{}", if on { "Click to prevent alternate items in the recipe." } else { "Click to allow alternate items in the recipe." }));
            return;
        }
        if mode == 1 && low[0] >= ENC_CYCLE_AT[0] && low[0] < ENC_CYCLE_AT[0] + 9.0 && low[1] >= ENC_CYCLE_AT[1] && low[1] < ENC_CYCLE_AT[1] + 9.0 {
            lines.push("Cycle Outputs".to_owned());
            lines.push("§7Change the primary output of this pattern".to_owned());
            return;
        }
    }
    let slots = upgrade_slots(block);
    if slots > 0 && at[0] >= 178.0 && at[0] < 210.0 && at[1] >= 0.0 && at[1] < 14.0 + 18.0 * slots as f32 {
        let name = ae2_name(block).unwrap_or("");
        lines.push("Compatible Upgrades:".to_owned());
        for (card, _, max, machines) in UPGRADES {
            if machines.contains(&name) {
                lines.push(format!("§7{} ({max})", item_words(card)));
            }
        }
        return;
    }
    if matches!(block, "ae2:level_emitter" | "ae2:energy_level_emitter") && at[0] >= TOOLBAR[0][0] && at[0] < TOOLBAR[0][0] + 18.0 && at[1] >= TOOLBAR[0][1] && at[1] < TOOLBAR[0][1] + 18.0 {
        let d = |i: usize| CLIENT.with_borrow(|c| c.screen_data.get(i).copied().unwrap_or(0));
        if block == "ae2:level_emitter" && d(2) != 0 {
            lines.push("Crafting Emitter Mode".to_owned());
            lines.push(format!("§7{}", if d(3) != 0 { "Emit Redstone to craft item." } else { "Emit Redstone while item is crafting." }));
        } else {
            lines.push("Redstone Mode".to_owned());
            lines.push(format!("§7{}", if CLIENT.with_borrow(|c| c.emit_below) { "Emit when levels are below limit." } else { "Emit when levels are above or equal to limit." }));
        }
        return;
    }
    if block == "ae2:crafting_terminal"
        && let Some(b) = stash_button(at, CLIENT.with_borrow(|c| c.rows))
    {
        if b == CT_STASH {
            lines.push("Store Items".to_owned());
            lines.push("§7Return items on the crafting grid to network storage.".to_owned());
        } else {
            lines.push("Take Items".to_owned());
            lines.push("§7Return items on the crafting grid to player inventory.".to_owned());
        }
        return;
    }
    if canon(block) == "ae2:pattern_provider" {
        let inside = |r: [f32; 2]| at[0] >= r[0] && at[0] < r[0] + 16.0 && at[1] >= r[1] && at[1] < r[1] + 16.0;
        let d = |i: usize| CLIENT.with_borrow(|c| c.screen_data.get(i).copied().unwrap_or(0));
        match pp_toolbar().into_iter().find(|(_, r)| inside(*r)).map(|(b, _)| b) {
            Some(PP_BLOCKING) => {
                lines.push("Blocking Mode".to_owned());
                lines.push(format!("§7{}", if d(0) != 0 { "Do not push crafting ingredients if inventory contains a pattern input." } else { "Ignore the contents of the target inventory." }));
            }
            Some(PP_LOCK) => {
                lines.push("Lock Crafting".to_owned());
                const TIPS: [&str; 5] = ["Never", "Until redstone pulse is received", "With redstone signal", "Without redstone signal", "Until primary crafting result is returned"];
                lines.push(format!("§7{}", TIPS[d(1).clamp(0, 4) as usize]));
            }
            Some(_) => {
                lines.push("Pattern Access Terminal".to_owned());
                lines.push("§7Show Or Hide on Pattern Access Terminal.".to_owned());
            }
            None if at[0] >= PP_PRIORITY[0] && at[0] < PP_PRIORITY[0] + 22.0 && at[1] >= PP_PRIORITY[1] && at[1] < PP_PRIORITY[1] + 22.0 => {
                lines.push("Priority".to_owned());
            }
            None if d(1) != 0 && at[0] >= 5.0 && at[0] < 131.0 && at[1] >= 15.0 && at[1] < 31.0 => {
                // PatternProviderLockReason.getTooltip.
                match d(3) {
                    1 => lines.push("Waiting for redstone pulse to unlock".to_owned()),
                    2 => lines.push("Locked by redstone signal".to_owned()),
                    3 => lines.push("Locked by lack of redstone signal".to_owned()),
                    4 => lines.push("Waiting for the primary result to unlock".to_owned()),
                    _ => {}
                }
            }
            None => {}
        }
        return;
    }
    if block == "ae2:cell_workbench" {
        let inside = |r: [f32; 2]| at[0] >= r[0] && at[0] < r[0] + 16.0 && at[1] >= r[1] && at[1] < r[1] + 16.0;
        let d = |i: usize| CLIENT.with_borrow(|c| c.screen_data.get(i).copied().unwrap_or(0));
        let fuzzy = CLIENT.with_borrow(|c| c.screen_data.get(1).copied().unwrap_or(-1));
        match wb_toolbar(fuzzy >= 0).into_iter().find(|(_, r)| inside(*r)).map(|(b, _)| b) {
            Some(WB_FUZZY) => {
                lines.push("Fuzzy Comparison".to_owned());
                lines.push(format!("§7{}", FUZZY_TIPS[fuzzy.clamp(0, 4) as usize]));
            }
            Some(PARTITION) => {
                lines.push("Partition Storage".to_owned());
                lines.push("§7Configures Partition based on currently stored items.".to_owned());
            }
            Some(CLEAR_CONFIG) => {
                lines.push("Clear".to_owned());
                lines.push("§7Clear Config/Settings".to_owned());
            }
            Some(_) => {
                lines.push("Copy Mode".to_owned());
                lines.push("§7Controls if the contents of the configuration pane are cleared when you remove the cell.".to_owned());
                lines.push(format!("§7{}", if d(0) == 0 { "Clear on remove" } else { "Keep on remove" }));
            }
            None => {}
        }
        return;
    }
    if block == "ae2:io_port" {
        let inside = |r: [f32; 2]| at[0] >= r[0] && at[0] < r[0] + 16.0 && at[1] >= r[1] && at[1] < r[1] + 16.0;
        let d = |i: usize| CLIENT.with_borrow(|c| c.screen_data.get(i).copied().unwrap_or(0));
        if inside([80.0, 17.0]) {
            lines.push("Transfer Direction".to_owned());
            lines.push(format!("§7{}", if d(1) == 1 { "Transfer data to Storage Cell" } else { "Transfer data to Network" }));
        } else if inside(TOOLBAR[0]) {
            lines.push("Operation Mode".to_owned());
            lines.push(format!("§7{}", ["Move to output when empty.", "Move to output when work is done.", "Move to output when full."][d(2).clamp(0, 2) as usize]));
        } else if inside(TOOLBAR[1]) && d(3) >= 0 {
            lines.push("Redstone Mode".to_owned());
            lines.push(format!("§7{}", ["Always active", "Active without signal", "Active with signal", "Activate once per pulse"][d(3).clamp(0, 3) as usize]));
        }
        return;
    }
    if matches!(block, "ae2:import_bus" | "ae2:export_bus" | "ae2:storage_bus" | "ae2:formation_plane") {
        let d = |n: usize| CLIENT.with_borrow(|c| c.screen_data.get(n).copied().unwrap_or(0));
        let (first, buttons) = bus_buttons(block, &d);
        for (n, (_, _, title, value)) in buttons.iter().enumerate() {
            let place = bar_place(first + n);
            if at[0] >= place[0] && at[0] < place[0] + 16.0 && at[1] >= place[1] && at[1] < place[1] + 16.0 {
                lines.push((*title).to_owned());
                lines.push(format!("§7{value}"));
            }
        }
        return;
    }
    if !matches!(block, "ae2:terminal" | "ae2:crafting_terminal" | "ae2:pattern_encoding_terminal") {
        return;
    }
    let [x, y] = [at[0] - TERM_GRID[0], at[1] - TERM_GRID[1]];
    if x < 0.0 || y < 0.0 || x >= TERM_COLS as f32 * 18.0 || y >= term_rows() as f32 * 18.0 {
        return;
    }
    let cell = CLIENT.with_borrow(|c| c.scroll * TERM_COLS) + (y / 18.0) as usize * TERM_COLS + (x / 18.0) as usize;
    CLIENT.with_borrow(|c| {
        if let Some((id, amount)) = visible(c).get(cell).and_then(|&i| c.items.get(i)) {
            let fluid = id.starts_with('~');
            let id = id.trim_start_matches('~');
            super::port_ae2_guide::hovered(id);
            lines.push(item_words(id));
            if *amount == 0 {
                lines.push("§7Craftable: click 1, right-click 16, shift 64".to_owned());
            } else if fluid {
                lines.push(format!("§7Stored: {}", key_amount("~", *amount, super::port_ae2_core::AmountFormat::Full)));
                lines.push("§8Click with a bucket to fill it".to_owned());
            } else {
                lines.push(format!("§7Stored: {}", super::port_ae2_core::KeyType::Items.format_amount(*amount, super::port_ae2_core::AmountFormat::Full)));
            }
        }
    });
}

/// `native_screen_mouse_scrolled_pre`: the wheel scrolls the terminal's grid.
pub fn scrolled(menu: Option<&str>, scroll: [f64; 2], canceled: &mut bool) {
    super::port_ae2_guide::scroll(scroll[1], canceled);
    if *canceled {
        return;
    }
    if dialog_scroll(scroll[1]) {
        *canceled = true;
        return;
    }
    if menu == Some("ae2:pattern_access_terminal") {
        CLIENT.with_borrow_mut(|c| {
            let most = provider_rows_shown(c).len().saturating_sub(6);
            c.scroll = if scroll[1] > 0.0 { c.scroll.saturating_sub(1) } else { (c.scroll + 1).min(most) };
        });
        *canceled = true;
        return;
    }
    if !matches!(menu, Some("ae2:terminal" | "ae2:crafting_terminal" | "ae2:pattern_encoding_terminal")) {
        return;
    }
    // ProcessingEncodingPanel.onMouseWheel.
    if menu == Some("ae2:pattern_encoding_terminal")
        && CLIENT.with_borrow_mut(|c| {
            let y = c.mouse[1] - c.encoder_extra;
            let over = c.encoder_mode == 1 && c.mouse[0] >= 9.0 && c.mouse[0] < 135.0 && (85.0..153.0).contains(&y);
            if over {
                c.enc_scroll = if scroll[1] > 0.0 { c.enc_scroll.saturating_sub(1) } else { (c.enc_scroll + 1).min(24) };
            }
            over
        })
    {
        *canceled = true;
        return;
    }
    // Shift and the wheel over an item: one in (up) or out (down), AE2's ROLL_DOWN and ROLL_UP.
    let rolled = CLIENT.with_borrow_mut(|c| {
        let [x, y] = [c.mouse[0] - TERM_GRID[0], c.mouse[1] - TERM_GRID[1]];
        if !c.shift || c.wireless_open == 2 || x < 0.0 || y < 0.0 || x >= TERM_COLS as f32 * 18.0 || y >= c.rows.max(3) as f32 * 18.0 {
            return false;
        }
        let cell = c.scroll * TERM_COLS + (y / 18.0) as usize * TERM_COLS + (x / 18.0) as usize;
        let index = visible(c).get(cell).copied().unwrap_or(EMPTY_CELL);
        let offset = if menu == Some("ae2:pattern_encoding_terminal") { 8 } else { 0 };
        let action = if scroll[1] > 0.0 { ROLL_DOWN } else { ROLL_UP };
        for _ in 0..(scroll[1].abs().round() as usize).max(1) {
            c.buttons_out.push((((index as i32) << 3) | action) + offset);
        }
        true
    });
    if rolled {
        *canceled = true;
        return;
    }
    CLIENT.with_borrow_mut(|c| {
        let rows = visible(c).len().div_ceil(TERM_COLS);
        let most = rows.saturating_sub(c.rows.max(3));
        if scroll[1] > 0.0 {
            c.scroll = c.scroll.saturating_sub(1);
        } else if scroll[1] < 0.0 {
            c.scroll = (c.scroll + 1).min(most);
        }
    });
    *canceled = true;
}

/// `native_block_entity_update_tag`: drives and ME chests send their cells' looks.
pub fn update_tag(entity: &ModBlockEntity, tag: &mut Option<Compound>) {
    if is_paint(&entity.id) {
        let mut out = Compound::new();
        out.insert(java("dots"), Tag::String(java(&nbt_string(&entity.nbt, "dots"))));
        *tag = Some(out);
        return;
    }
    if cable(&entity.id).is_some() || is_twin(&entity.id) {
        let mut out = Compound::new();
        for key in ["facades", "cable", "lights"] {
            if let Some(f) = entity.nbt.get(&java(key)) {
                out.insert(java(key), f.clone());
            }
        }
        *tag = Some(out);
        return;
    }
    if entity.id == "ae2:sky_stone_tank" {
        let mut out = Compound::new();
        if let Some(fluids) = entity.nbt.get(&java("fluids")) {
            out.insert(java("fluids"), fluids.clone());
        }
        *tag = Some(out);
        return;
    }
    if ae2_name(&entity.id).is_some_and(|n| n.ends_with("_bus") || n.ends_with("_p2p_tunnel") || n.ends_with("_plane")) {
        let mut out = Compound::new();
        out.insert(java("wire"), Tag::String(java(&nbt_string(&entity.nbt, "wire"))));
        *tag = Some(out);
        return;
    }
    if entity.id == "ae2:crafting_monitor" {
        let mut out = Compound::new();
        for key in ["show", "text", "paint"] {
            out.insert(java(key), Tag::String(java(&nbt_string(&entity.nbt, key))));
        }
        *tag = Some(out);
        return;
    }
    if is_monitor(&entity.id) {
        let key = key_from_string(&nbt_string(&entity.nbt, "item"));
        let show = key.as_ref().map_or(String::new(), |k| k.id().to_owned());
        // The amount as AE2's monitor writes it under the item (AmountFormat.SLOT: "1.5M").
        let text = key.map_or(String::new(), |k| k.format_amount(nbt_i64(&entity.nbt, "amount"), super::port_ae2_core::AmountFormat::Slot));
        let mut out = Compound::new();
        out.insert(java("show"), Tag::String(java(&show)));
        out.insert(java("text"), Tag::String(java(&text)));
        *tag = Some(out);
        return;
    }
    if matches!(entity.id.as_str(), "ae2:sky_stone_chest" | "ae2:smooth_sky_stone_chest") {
        let mut out = Compound::new();
        out.insert(java("open"), Tag::Long(nbt_i64(&entity.nbt, "open")));
        *tag = Some(out);
        return;
    }
    if entity.id == "ae2:crank" {
        let mut out = Compound::new();
        out.insert(java("turns"), Tag::Long(nbt_i64(&entity.nbt, "turns")));
        *tag = Some(out);
        return;
    }
    if entity.id == "ae2:molecular_assembler" {
        let mut out = Compound::new();
        if let Some(look) = entity.nbt.get(&java("look")) {
            out.insert(java("look"), look.clone());
        }
        out.insert(java("speed"), Tag::Long(nbt_i64(&entity.nbt, "speed")));
        *tag = Some(out);
        return;
    }
    if entity.id == "ae2:inscriber" {
        let mut out = Compound::new();
        out.insert(java("look"), Tag::String(java(&inscriber_look(entity))));
        *tag = Some(out);
        return;
    }
    if entity.id == "ae2:charger" {
        let mut out = Compound::new();
        let holds = entity.items.first().and_then(|s| s.as_ref()).map_or(String::new(), |s| s.item.to_string());
        out.insert(java("holds"), Tag::String(java(&holds)));
        *tag = Some(out);
        return;
    }
    if !matches!(entity.id.as_str(), "ae2:drive" | "ae2:chest") {
        return;
    }
    let mut out = Compound::new();
    let looks = cell_looks(&entity.items).into_iter().map(|l| Tag::String(java(&l))).collect();
    out.insert(java("cells"), Tag::List(looks));
    *tag = Some(out);
}

/// A crank's turn adds this much charging (ten turns charge a crystal, as AE2's crank does).
const CRANK_TICKS: i64 = CHARGE_TICKS / 10;

/// The direction a facing names.
fn facing_dir(facing: Option<&str>) -> Pos {
    match facing {
        Some("down") => [0, -1, 0],
        Some("up") => [0, 1, 0],
        Some("south") => [0, 0, 1],
        Some("west") => [-1, 0, 0],
        Some("east") => [1, 0, 0],
        _ => [0, 0, -1],
    }
}

/// What a block entity's moving part shows this frame: it moves from where it was shown towards `target` at
/// `speed` (units a second), as AE2's renderers ease their parts, so a change never jumps.
fn eased(entity: &ModBlockEntity, target: f32, speed: f32) -> f32 {
    thread_local! {
        static SHOWN: RefCell<HashMap<usize, (f32, std::time::Instant)>> = RefCell::new(HashMap::new());
    }
    let key = entity as *const ModBlockEntity as usize;
    SHOWN.with_borrow_mut(|m| {
        let now = std::time::Instant::now();
        let (shown, at) = m.entry(key).or_insert((target, now));
        let step = speed * now.duration_since(*at).as_secs_f32();
        *shown = if (target - *shown).abs() <= step { target } else { *shown + step * (target - *shown).signum() };
        *at = now;
        *shown
    })
}

// Sounds the renderers heard (a lid opening), played on the next frame (drawing can't play them).
thread_local! {
    static SOUNDS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

/// `native_frame`: the sounds the renderers queued, where the player hears them (a renderer only runs for what
/// is near, and it can't say where its block is).
pub fn play_queued_sounds(game: &mut Game<'_>) {
    compass_ask(game);
    // The guide's recipes (made with the game's &mut, which drawing hasn't).
    if super::port_ae2_guide::needs_recipes() {
        super::port_ae2_guide::set_recipes(game.native_recipes());
    }
    super::port_ae2_guide::frame(game);
    // The wireless terminal's screen: opened when the server says, its clicks sent, closed when it shuts.
    let (open, out) = CLIENT.with_borrow_mut(|c| (c.wireless_open, std::mem::take(&mut c.wireless_out)));
    if open == 3 {
        CLIENT.with_borrow_mut(|c| c.wireless_open = 0);
        if game.native_mod_screen() == Some("ae2") {
            game.native_mod_screen_close("ae2");
        }
    } else if open == 1 && game.native_mod_screen_open("ae2") {
        CLIENT.with_borrow_mut(|c| c.wireless_open = 2);
    } else if open == 2 && game.native_mod_screen() != Some("ae2") {
        CLIENT.with_borrow_mut(|c| c.wireless_open = 0);
        let _ = game.native_net_send_to_server(NET_WIRELESS, b"close");
    }
    for m in out {
        let _ = game.native_net_send_to_server(NET_WIRELESS, m.as_bytes());
    }
    pump_ui(game);
    // The level emitter's amount, when typed (the key hooks can't send).
    if CLIENT.with_borrow_mut(|c| std::mem::take(&mut c.level_dirty)) {
        send_level(game);
    }
    let sounds = SOUNDS.with_borrow_mut(std::mem::take);
    let Some(p) = game.native_player() else { return };
    for sound in sounds {
        game.native_play_local_sound(sound, [p.x, p.y + 1.0, p.z], 0.5, 0.95, 0);
    }
}

/// `native_mod_container_viewers`: a sky stone chest shows itself open while anyone has it open (its update tag).
pub fn container_viewers(game: &mut Game<'_>, pos: Pos, viewers: u32, previous: u32) {
    if (viewers > 0) == (previous > 0) || !is_sky_chest(&block_id(game, pos)) {
        return;
    }
    game.native_with_mod_block_entity(pos, |e| set_nbt_i64(&mut e.nbt, "open", i64::from(viewers > 0)));
    SERVER.with_borrow_mut(|s| s.flips.push(pos));
}

/// AE2's SkyChestTESR: the lid (`ae2:hz_<chest>_lid`) swings open about its back edge while the chest is open, with
/// a chest's sounds as it starts to open and as it shuts.
fn sky_chest_submit(
    game: &Game<'_>,
    block: &str,
    state: horizon_world::registry::BlockStateId,
    entity: &ModBlockEntity,
    draw: &mut super::native_block_entity_draw::BlockEntityDraw,
) {
    thread_local! {
        static WAS_OPEN: RefCell<HashMap<usize, bool>> = RefCell::new(HashMap::new());
    }
    let open = matches!(entity.nbt.get(&java("open")), Some(Tag::Long(1)));
    let key = entity as *const ModBlockEntity as usize;
    let changed = WAS_OPEN.with_borrow_mut(|m| m.insert(key, open).is_some_and(|was| was != open));
    if changed {
        SOUNDS.with_borrow_mut(|s| s.push(if open { "minecraft:block.chest.open" } else { "minecraft:block.chest.close" }));
    }
    // Vanilla's lid: eased, a quarter turn at most.
    let lid = eased(entity, if open { 1.0 } else { 0.0 }, 2.0);
    let lid = 1.0 - (1.0 - lid).powi(3);
    let c = [0.5, 0.5, 0.5];
    draw.push();
    match game.native_block_state_property(state, "facing").unwrap_or("north") {
        "south" => draw.rotate_about([0.0, 1.0, 0.0], 180.0, c),
        "east" => draw.rotate_about([0.0, 1.0, 0.0], -90.0, c),
        "west" => draw.rotate_about([0.0, 1.0, 0.0], 90.0, c),
        _ => {}
    }
    // The hinge: the lid's back edge (y 10, z 15 of 16), the front lifting.
    draw.rotate_about([1.0, 0.0, 0.0], 90.0 * lid, [0.5, 10.0 / 16.0, 15.0 / 16.0]);
    let lid_block = if block == "ae2:smooth_sky_stone_chest" { "ae2:hz_smooth_sky_stone_chest_lid" } else { "ae2:hz_sky_stone_chest_lid" };
    game.native_draw_block(draw, lid_block, &[]);
    draw.pop();
}

/// A turn of the crank: it first faces away from a charger beside it (what it sits on), then each turn charges
/// that charger's crystal a tenth, and turns the handle a quarter.
fn crank_turn(game: &mut Game<'_>, pos: Pos) {
    let facing = game.native_block_property(pos, "facing");
    let mut target = add(pos, facing_dir(facing.as_deref()).map(|v| -v));
    if block_id(game, target) != "ae2:charger" {
        let names = ["down", "up", "north", "south", "west", "east"];
        let Some((d, name)) = SIDES.iter().zip(names).find(|(d, _)| block_id(game, add(pos, **d)) == "ae2:charger") else {
            return;
        };
        target = add(pos, *d);
        // Facing away from the charger.
        let away = names[SIDES.iter().position(|s| *s == d.map(|v| -v)).unwrap_or(0)];
        let _ = name;
        game.native_set_block_property_with(pos, "facing", away, 3);
    }
    game.native_with_mod_block_entity(target, |e| {
        let charging = e.items.first().and_then(|s| s.as_ref()).is_some_and(|s| charger_result(&s.item).is_some());
        if charging {
            let p = nbt_i64(&e.nbt, "progress");
            set_nbt_i64(&mut e.nbt, "progress", (p + CRANK_TICKS).min(CHARGE_TICKS));
        }
    });
    game.native_with_mod_block_entity(pos, |e| {
        let t = nbt_i64(&e.nbt, "turns");
        set_nbt_i64(&mut e.nbt, "turns", t + 1);
    });
    SERVER.with_borrow_mut(|s| s.flips.push(pos));
}

/// The crank's handle (`ae2:hz_crank_handle`, AE2's crank_handle model) turning about the crank's axis: a quarter
/// turn for each turn the server counted, eased over a quarter second.
fn crank_submit(
    game: &Game<'_>,
    state: horizon_world::registry::BlockStateId,
    entity: &ModBlockEntity,
    draw: &mut super::native_block_entity_draw::BlockEntityDraw,
) {
    let turns = match entity.nbt.get(&java("turns")) {
        Some(Tag::Long(n)) => *n,
        _ => 0,
    };
    // A quarter turn per turn, eased over a quarter second from wherever the handle is shown.
    let angle = eased(entity, turns as f32 * 90.0, 360.0);
    let c = [0.5, 0.5, 0.5];
    draw.push();
    // The handle model points north (its base at the south side): turn it to the crank's facing, then about it.
    match game.native_block_state_property(state, "facing").unwrap_or("north") {
        "south" => draw.rotate_about([0.0, 1.0, 0.0], 180.0, c),
        "east" => draw.rotate_about([0.0, 1.0, 0.0], -90.0, c),
        "west" => draw.rotate_about([0.0, 1.0, 0.0], 90.0, c),
        "up" => draw.rotate_about([1.0, 0.0, 0.0], 90.0, c),
        "down" => draw.rotate_about([1.0, 0.0, 0.0], -90.0, c),
        _ => {}
    }
    draw.rotate_about([0.0, 0.0, 1.0], angle, c);
    game.native_draw_block(draw, "ae2:hz_crank_handle", &[]);
    draw.pop();
}

/// Ticks a molecular assembler takes for one craft.
const ASSEMBLE_TICKS: u64 = 10;

/// What the inscriber's window shows: its top, middle, bottom and output items, then `1` while it works.
fn inscriber_look(entity: &ModBlockEntity) -> String {
    let mut parts: Vec<String> = (0..4)
        .map(|i| entity.items.get(i).and_then(|s| s.as_ref()).map_or(String::new(), |s| s.item.to_string()))
        .collect();
    parts.push(if entity.data.first().copied().unwrap_or(0) > 0 { "1" } else { "0" }.to_owned());
    parts.join(",")
}

/// AE2's `InscriberTESR`: the two press heads in the window close on the middle and open again once a cycle
/// while it works, the presses under and over them and the item between (the output once there's no input).
fn inscriber_submit(game: &Game<'_>, entity: &ModBlockEntity, draw: &mut super::native_block_entity_draw::BlockEntityDraw) {
    use super::native_block_entity_draw::CubeFace;
    use horizon_assets::item_model::DisplayContext;
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let Some(Tag::String(look)) = entity.nbt.get(&java("look")) else { return };
    let look = look.to_string().unwrap_or_default();
    let parts: Vec<&str> = look.split(',').collect();
    let item = |i: usize| parts.get(i).copied().filter(|s| !s.is_empty());
    let working = parts.get(4) == Some(&"1");
    // How far the heads have come in: 0 open, 1 shut on the middle, over the 100-tick cycle.
    let shut = if working {
        let t = START.get_or_init(std::time::Instant::now).elapsed().as_secs_f32() % 5.0 / 5.0;
        1.0 - (2.0 * t - 1.0).abs()
    } else {
        0.0
    };
    let lift = 2.5 * shut;
    let sprite = "ae2:block/inscriber_inside";
    let face = Some(CubeFace { sprite, uv: None });
    let faces = [face, face, face, face, face, face];
    // The heads (texels): the top one hangs from the frame, the bottom one sits on it.
    game.native_draw_cube(draw, [3.0, 11.0 - lift, 3.0], [13.0, 12.0 - lift, 13.0], faces, 0xFFFFFF);
    game.native_draw_cube(draw, [3.0, 4.0 + lift, 3.0], [13.0, 5.0 + lift, 13.0], faces, 0xFFFFFF);
    let flat = |draw: &mut super::native_block_entity_draw::BlockEntityDraw, id: &str, y: f32, flip: bool| {
        draw.push();
        draw.translate([0.5, y / 16.0, 0.5]);
        draw.rotate_about([1.0, 0.0, 0.0], if flip { -90.0 } else { 90.0 }, [0.0, 0.0, 0.0]);
        draw.scale([0.4, 0.4, 0.4]);
        game.native_draw_item(draw, &ItemStack::new(id, 1), DisplayContext::Fixed, None, 0);
        draw.pop();
    };
    if let Some(top) = item(0) {
        flat(draw, top, 10.9 - lift, true);
    }
    if let Some(bottom) = item(2) {
        flat(draw, bottom, 5.1 + lift, false);
    }
    if let Some(middle) = item(1).or(item(3)) {
        flat(draw, middle, 8.0, false);
    }
}

/// AE2's cell status lights (`CellState.getStateColor`), by state number.
const CELL_LIGHTS: [u32; 5] = [0x000000, 0x00FF00, 0x00AAFF, 0xFFAA00, 0xFF0000];

/// `native_block_entity_submit`: a drive's cells in the holes of its front, each with its status light.
pub fn submit(
    game: &Game<'_>,
    block: &str,
    state: horizon_world::registry::BlockStateId,
    entity: &ModBlockEntity,
    draw: &mut super::native_block_entity_draw::BlockEntityDraw,
) {
    use super::native_block_entity_draw::CubeFace;
    if is_twin(block) {
        twin_submit(game, block, state, entity, draw);
        facade_submit(game, entity, draw);
        return;
    }
    if is_paint(block) {
        paint_submit(game, entity, draw);
        return;
    }
    if block == "ae2:inscriber" {
        inscriber_submit(game, entity, draw);
        return;
    }
    if is_monitor(block) {
        monitor_submit(game, state, entity, draw);
        return;
    }
    if block == "ae2:crafting_monitor" {
        crafting_monitor_submit(game, state, entity, draw);
        return;
    }
    if block == "ae2:sky_stone_tank" {
        tank_submit(game, entity, draw);
        return;
    }
    if ae2_name(block).is_some_and(|n| n.ends_with("_bus") || n.ends_with("_p2p_tunnel") || n.ends_with("_plane")) {
        part_cable_submit(game, state, entity, draw);
        return;
    }
    if block == "ae2:crank" {
        crank_submit(game, state, entity, draw);
        return;
    }
    if matches!(block, "ae2:sky_stone_chest" | "ae2:smooth_sky_stone_chest") {
        sky_chest_submit(game, block, state, entity, draw);
        return;
    }
    if block == "ae2:molecular_assembler" {
        // MolecularAssemblerRenderer.renderStatus: the item made, still, as it lies on the ground in the glass.
        let look = nbt_string(&entity.nbt, "look");
        if !look.is_empty() {
            let drop = if game.pack.assets.registry.default_state(&look).is_some() { 0.2 } else { 0.3 };
            draw.push();
            draw.translate([0.5, 0.5 - drop, 0.5]);
            game.native_draw_item(draw, &ItemStack::new(&look, 1), horizon_assets::item_model::DisplayContext::Ground, None, 0);
            draw.pop();
        }
        assembler_effects(game, state, entity, draw, !look.is_empty());
        return;
    }
    if block == "ae2:charger" {
        // The crystal on the charger's plate, as AE2's charger renderer shows it.
        if let Some(Tag::String(holds)) = entity.nbt.get(&java("holds"))
            && let Ok(id) = holds.to_string()
            && !id.is_empty()
        {
            draw.push();
            draw.translate([0.5, 0.5, 0.5]);
            draw.scale([0.3, 0.3, 0.3]);
            let drawn = game.native_draw_item(draw, &ItemStack::new(&id, 1), horizon_assets::item_model::DisplayContext::Fixed, None, 0);
            draw.pop();
            once(&format!("charger item {id} drawn: {drawn}"));
        }
        return;
    }
    if block != "ae2:drive" {
        return;
    }
    let Some(Tag::List(cells)) = entity.nbt.get(&java("cells")) else {
        once("drive submit: no cells tag on the client's block entity");
        return;
    };
    draw.push();
    let c = [0.5, 0.5, 0.5];
    match game.native_block_state_property(state, "facing").unwrap_or("north") {
        "south" => draw.rotate_about([0.0, 1.0, 0.0], 180.0, c),
        "east" => draw.rotate_about([0.0, 1.0, 0.0], -90.0, c),
        "west" => draw.rotate_about([0.0, 1.0, 0.0], 90.0, c),
        "up" => draw.rotate_about([1.0, 0.0, 0.0], 90.0, c),
        "down" => draw.rotate_about([1.0, 0.0, 0.0], -90.0, c),
        _ => {}
    }
    for (slot, look) in cells.iter().enumerate().take(10) {
        let Tag::String(look) = look else { continue };
        let look = look.to_string().unwrap_or_default();
        let Some((model, state)) = look.split_once(':') else { continue };
        // The front's holes: two columns of five, from the top; x is mirrored seen from the north.
        let (col, row) = (slot % 2, slot / 2);
        let x0 = if col == 0 { 9.0 } else { 2.0 };
        let y1 = 16.0 - (1.0 + 3.0 * row as f32);
        // `native_draw_cube` takes texels, 16 a block, as a model element does. The cell is a plate a fifth of a
        // texel in front of the drive's face: geometry inside the drive (an opaque block) is culled whole.
        let from = [x0, y1 - 2.0, -0.2];
        let to = [x0 + 5.0, y1, 0.0];
        let sprite = format!("ae2:block/drive/cells/{model}");
        let face = |uv| Some(CubeFace { sprite: sprite.as_str(), uv: Some(uv) });
        // Down, up, north, south, west, east, as `Direction::ALL`.
        let drawn = game.native_draw_cube(draw, from, to, [face([0.0, 4.0, 5.0, 6.0]), face([0.0, 0.0, 5.0, 2.0]), face([0.0, 2.0, 5.0, 4.0]), None, None, None], 0xFFFFFF);
        once(&format!("drive cell {sprite} drawn: {drawn}"));
        let light = CELL_LIGHTS[state.parse::<usize>().unwrap_or(0).min(4)];
        if light != 0 {
            let lx = if col == 0 { x0 } else { x0 + 4.0 };
            game.native_draw_cube(
                draw,
                [lx, y1 - 1.5, -0.3],
                [lx + 1.0, y1 - 0.5, -0.2],
                [None, None, face([5.0, 2.0, 6.0, 3.0]), None, None, None],
                light,
            );
        }
    }
    draw.pop();
}

/// A debug line in the game's log, once per distinct text (drawing hooks run every frame).
fn once(text: &str) {
    thread_local! {
        static SEEN: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    }
    if SEEN.with_borrow_mut(|seen| seen.insert(text.to_owned())) {
        log::info!("ae2: {text}");
    }
}

/// `native_place_block`: devices on the beacon template face the player who placed them, as a barrel would (away
/// from where they look); upright ones (the charger) only turn about the vertical.

// ---------------------------------------------------------------------------------------------------------
// M2: crafting terminal, patterns, pattern providers, molecular assemblers, crafting CPUs.

/// The crafting terminal's grid (top-left slot) and result slot, in its screen.
/// crafting_terminal.json: the grid's and the result's slots (left 37 / bottom 154, left 131 / bottom 136 of 241).
const CRAFT_GRID: [f32; 2] = [37.0, 87.0];
const CRAFT_RESULT: [f32; 2] = [131.0, 105.0];
/// The pattern encoding terminal's buttons and their ids.
/// The terminals' crafting status button, in the header beside the title.
const STATUS_BUTTON: [f32; 4] = [158.0, 3.0, 12.0, 12.0];

/// The mode tabs on the right edge (crafting, then processing), 20 square.
const ENC_TABS: [[f32; 2]; 4] = [[173.0, 79.0], [173.0, 99.0], [173.0, 119.0], [173.0, 139.0]];
/// The clear button on each mode's panel, and the encode button.
const ENC_CLEAR: [[f32; 2]; 2] = [[74.0, 86.0], [82.0, 86.0]];
const ENC_SUBSTITUTE_AT: [f32; 2] = [84.0, 86.0];
const ENC_CYCLE_AT: [f32; 2] = [127.0, 97.0];

/// The outputs set in a processing pattern's cells.
fn encoder_outputs(c: &Client) -> usize {
    (0..27).filter_map(|j| c.ghosts.get(enc_output(j))).filter(|(id, _)| !id.is_empty() && id != "minecraft:air").count()
}
const ENC_ENCODE: [f32; 2] = [147.0, 107.0];
/// The blank and encoded pattern slots (AE2's pattern.png).
const ENC_BLANK: [f32; 2] = [147.0, 88.0];
const ENC_ENCODED: [f32; 2] = [147.0, 131.0];
const ENCODE: i32 = 0;
const TOGGLE_MODE: i32 = 1;
/// The IO port's fullness and redstone mode buttons.
const IO_FULLNESS: i32 = 2;
const IO_REDSTONE: i32 = 3;

/// AE2's guis/states.png icons (Icon): x, y of a 16-pixel square.
const ICON_ARROW_RIGHT: [f32; 2] = [32.0, 48.0];
const ICON_ARROW_LEFT: [f32; 2] = [48.0, 48.0];
const ICON_FULLNESS: [[f32; 2]; 3] = [[0.0, 80.0], [16.0, 80.0], [32.0, 80.0]];
/// Ignore, low, high, pulse (RedstoneMode's order in the IO port's setting).
const ICON_REDSTONE: [[f32; 2]; 4] = [[48.0, 0.0], [0.0, 0.0], [16.0, 0.0], [32.0, 0.0]];
const ICON_BUTTON_BACKGROUND: [f32; 2] = [240.0, 240.0];

/// AE2's IconButton: the toolbar button's background and its icon from states.png, 16 pixels.
fn draw_state_button(game: &Game<'_>, batch: &mut HudBatch, at: [f32; 2], icon: [f32; 2]) {
    let [bx, by] = ICON_BUTTON_BACKGROUND;
    game.native_sprite(batch, "ae2:gui/states", [bx, by, 16.0, 16.0], [at[0], at[1], 16.0, 16.0]);
    game.native_sprite(batch, "ae2:gui/states", [icon[0], icon[1], 16.0, 16.0], [at[0], at[1], 16.0, 16.0]);
}
const CLEAR: i32 = 2;
/// The tabs: crafting, processing.
const SET_CRAFTING: i32 = 3;
const SET_PROCESSING: i32 = 4;
/// Menu buttons from here on are clicks on the encoder's ghost cells: the cell shifted by three, and RIGHT.
const GHOST: i32 = 1 << 28;
/// Presses on the Upgrade Toolbelt: its slot shifted by one, and whether the right button.
const TOOLBELT: i32 = 1 << 25;
/// The encoder's half-size buttons (below `STONECUT_PICK`, above what a terminal's click can reach here).
const ENC_SUBSTITUTE: i32 = (1 << 25) - 2;
const ENC_CYCLE: i32 = (1 << 25) - 1;
/// A click on an empty part of a terminal's grid (no item there): only a cursor stack does anything.
const EMPTY_CELL: usize = (1 << 23) - 1;

/// The pattern access terminal's slot clicks: provider times nine plus slot, shifted by two, and SHIFT.
const PROVIDER_SLOT: i32 = 1 << 27;
const PAT_SHOW: i32 = 1;
/// ShowPatternProviders' icons (Icon.PATTERN_TERMINAL_*) and words, by the setting.
const PAT_BUTTON: [([f32; 2], &str); 3] = [
    ([96.0, 80.0], "Show visible Pattern Providers"),
    ([128.0, 80.0], "Show visible Pattern Providers with empty slots"),
    ([112.0, 80.0], "Show all Pattern Providers"),
];

/// Where ghost cell `i` sits in the encoder's screen: the 3x3 grid (CRAFTING_GRID at 18, or PROCESSING_INPUTS at
/// 26 beside the scroll bar), then processing's three outputs (PROCESSING_OUTPUTS at 110).
/// The processing pattern's cells (PatternEncodingTermMenu: 81 inputs, 27 outputs), as they're saved: the first nine
/// inputs, the first three outputs, then the other inputs and the other outputs.
const ENC_CELLS: usize = 108;

fn enc_input(k: usize) -> usize {
    if k < 9 { k } else { k + 3 }
}

fn enc_output(j: usize) -> usize {
    if j < 3 { 9 + j } else { 84 + j - 3 }
}

/// The saved cell the place `i` of the panel shows, the processing panel scrolled by `scroll` rows.
fn enc_visible(i: usize, mode: i32, scroll: usize) -> usize {
    match (mode, i) {
        (1, 0..=8) => enc_input(scroll * 3 + i),
        (1, _) => enc_output(scroll + i - 9),
        _ => i,
    }
}

fn enc_ghosts(entity: &ModBlockEntity) -> Vec<Option<(String, i64)>> {
    ghosts_n(entity, ENC_CELLS)
}

fn ghost_cell(i: usize, crafting: bool) -> [f32; 2] {
    let left = if crafting { 18.0 } else { 26.0 };
    if i < 9 { [left + 18.0 * (i % 3) as f32, 93.0 + 18.0 * (i / 3) as f32] } else { [110.0, 93.0 + 18.0 * (i - 9) as f32] }
}

/// The encoder's ghost cells (AE2's fake slots: they hold a copy, never an item): nine grid cells, then three
/// processing outputs, each an id and an amount.
fn ghosts_of(entity: &ModBlockEntity) -> Vec<Option<(String, i64)>> {
    ghosts_n(entity, 12)
}

/// A block's ghost cells, `n` of them.
fn ghosts_n(entity: &ModBlockEntity, n: usize) -> Vec<Option<(String, i64)>> {
    let saved = match entity.nbt.get(&java("ghosts")) {
        Some(Tag::String(s)) => s.to_string().unwrap_or_default(),
        _ => String::new(),
    };
    let mut cells: Vec<Option<(String, i64)>> = saved
        .split(';')
        .map(|c| {
            let (id, n) = c.split_once(' ')?;
            Some((id.to_owned(), n.parse().ok()?))
        })
        .collect();
    cells.resize(n, None);
    cells.truncate(n);
    cells
}

fn set_ghosts(entity: &mut ModBlockEntity, cells: &[Option<(String, i64)>]) {
    let text: Vec<String> = cells.iter().map(|c| c.as_ref().map_or(String::new(), |(id, n)| format!("{id} {n}"))).collect();
    entity.nbt.insert(java("ghosts"), Tag::String(java(&text.join(";"))));
}

/// A click on a ghost cell: an item on the cursor puts a copy there (one in a crafting grid, the stack's
/// amount in processing; right-click adds one), an empty cursor clears it (right-click takes one away).
fn ghost_click(entity: &mut ModBlockEntity, cell: usize, right: bool, carried: Option<&ItemStack>) {
    if cell >= ENC_CELLS {
        return;
    }
    let mode = nbt_i64(&entity.nbt, "mode");
    // Crafting, smithing and stonecutting take one of each; only processing keeps amounts.
    let crafting = mode != 1;
    let cells_in_mode = match mode {
        0 => 9,
        2 => 3,
        3 => 1,
        _ => ENC_CELLS,
    };
    if cell >= cells_in_mode && entity.id == "ae2:pattern_encoding_terminal" || crafting && cell >= 9 {
        return;
    }
    if entity.id != "ae2:pattern_encoding_terminal" && cell >= 12 {
        return;
    }
    let mut cells = if entity.id == "ae2:pattern_encoding_terminal" { enc_ghosts(entity) } else { ghosts_of(entity) };
    if right
        && let Some(fluid) = carried.and_then(|c| bucket_fluid(&c.item))
        && (mode == 1 && entity.id == "ae2:pattern_encoding_terminal" || entity.id == "ae2:level_emitter")
    {
        cells[cell] = Some((format!("~{fluid}"), 1000));
        set_ghosts(entity, &cells);
        return;
    }
    cells[cell] = match (carried, cells[cell].take()) {
        (Some(stack), Some((id, n))) if right && *stack.item == *id && !crafting => Some((id, n + 1)),
        (Some(stack), _) if crafting || right => Some((stack.item.to_string(), 1)),
        (Some(stack), _) => Some((stack.item.to_string(), i64::from(stack.count))),
        (None, Some((id, n))) if right && n > 1 => Some((id, n - 1)),
        (None, _) => None,
    };
    set_ghosts(entity, &cells);
}

/// What the encoder's crafting grid makes (the recipe's result and count), as AE2 shows beside it.
fn craft_preview(game: &Game<'_>, cells: &[Option<(String, i64)>]) -> Option<(String, i64)> {
    let grid: Vec<Option<ItemStack>> = cells.iter().take(9).map(|c| c.as_ref().map(|(id, _)| ItemStack::new(id, 1))).collect();
    if grid.iter().all(Option::is_none) {
        return None;
    }
    game.res.recipes().craft(&grid, 3).map(|r| (r.item.to_string(), i64::from(r.count)))
}
/// The custom data key of an encoded pattern.
const PATTERN_KEY: &str = "ae2:pattern";

fn draw_button(game: &Game<'_>, batch: &mut HudBatch, rect: [f32; 4], label: &str) {
    draw_button_at(game, batch, rect, label, false);
}

/// A button, lit while the mouse is over it (`origin`: the screen's top left).
fn draw_button_hover(game: &Game<'_>, batch: &mut HudBatch, rect: [f32; 4], label: &str, origin: [f32; 2]) {
    let [mx, my] = CLIENT.with_borrow(|c| c.mouse);
    let (mx, my) = (mx + origin[0], my + origin[1]);
    let hover = mx >= rect[0] && mx < rect[0] + rect[2] && my >= rect[1] && my < rect[1] + rect[3];
    draw_button_at(game, batch, rect, label, hover);
}

/// A vanilla button (AE2's widgets are): "§7" before the label greys it out.
fn draw_button_at(game: &Game<'_>, batch: &mut HudBatch, rect: [f32; 4], label: &str, hover: bool) {
    let (label, enabled) = label.strip_prefix("§7").map_or((label, true), |l| (l, false));
    let sprite = match (enabled, hover) {
        (false, _) => "gui/sprites/widget/button_disabled",
        (true, true) => "gui/sprites/widget/button_highlighted",
        (true, false) => "gui/sprites/widget/button",
    };
    let [x, y, w, h] = rect;
    horizon_client::ui::nine_slice(batch, &game.gui.atlas, sprite, x, y, w, h, u32::MAX);
    let tw = game.native_text_width(label);
    game.native_text(batch, label, [x + (w - tw) / 2.0, y + ((h - 8.0) / 2.0).floor()], if enabled { 0xFFFFFF } else { 0xA0A0A0 }, true);
}

/// AE2's Scrollbar (DEFAULT style): the creative tab's handle, greyed out with nothing to scroll.
fn draw_scrollbar(game: &Game<'_>, batch: &mut HudBatch, at: [f32; 2], height: f32, top: usize, most: usize) {
    let (sprite, offset) = if most == 0 {
        ("gui/sprites/container/creative_inventory/scroller_disabled", 0.0)
    } else {
        ("gui/sprites/container/creative_inventory/scroller", ((height - 15.0) * top as f32 / most as f32).floor())
    };
    game.native_sprite(batch, sprite, [0.0, 0.0, 12.0, 15.0], [at[0], at[1] + offset, 12.0, 15.0]);
}

/// The row a scroll bar's handle at the pointer stands for.
fn scrollbar_row(mouse_y: f32, top: f32, height: f32, most: usize) -> usize {
    if most == 0 {
        return 0;
    }
    let travel = (height - 15.0).max(1.0);
    (((mouse_y - top - 7.5) / travel).clamp(0.0, 1.0) * most as f32).round() as usize
}

/// `native_screen_mouse_button_released_pre`: a scroll bar's handle let go. A press a dialog or the toolbelt took has
/// its release taken too: let go outside the window, it would throw the stack on the cursor.
pub fn scrollbar_release(canceled: &mut bool) {
    CLIENT.with_borrow_mut(|c| {
        c.scroll_drag = 0;
        if std::mem::take(&mut c.press_taken) {
            *canceled = true;
        }
    });
}

/// `native_screen_mouse_button_pressed_pre` took the press.
pub fn press_taken() {
    CLIENT.with_borrow_mut(|c| c.press_taken = true);
}

/// A dialog or the crafting status over a screen: those are menus of their own in AE2, with nothing behind them.
fn overlay_shown() -> bool {
    CLIENT.with_borrow(|c| c.show_jobs || c.settings_open || c.dialog.is_some())
}

/// The crafting terminal: its result is the grid's recipe; taking it uses one of each ingredient, and the grid
/// refills from the network when it can (AE2's crafting terminal does).
fn crafting_terminal_tick(game: &mut Game<'_>, grid: Option<usize>, entity: &mut ModBlockEntity) {
    if entity.items.len() < 11 {
        return;
    }
    let shown = nbt_i64(&entity.nbt, "shown") != 0;
    if shown && entity.items[CT_RESULT].is_none() {
        // The player took the result: use the ingredients.
        for slot in CT_GRID {
            let Some(stack) = entity.items[slot].as_mut() else { continue };
            let refill = grid.map(|g| {
                let key = key_of(game, stack);
                with_storage(game, g, None, |st| st.extract(&key, 1, Actionable::Modulate))
            });
            if refill != Some(1) {
                stack.count -= 1;
                if stack.count == 0 {
                    entity.items[slot] = None;
                }
            }
        }
    }
    let grid_items: Vec<Option<ItemStack>> = entity.items[CT_GRID].to_vec();
    let result = game.res.recipes().craft(&grid_items, 3);
    if entity.items[CT_RESULT].as_ref().map(|s| (&s.item, s.count)) != result.as_ref().map(|s| (&s.item, s.count)) {
        entity.items[CT_RESULT] = result.clone();
    }
    set_nbt_i64(&mut entity.nbt, "shown", i64::from(entity.items[CT_RESULT].is_some()));
}

/// The crafting terminal's grid and result slots (slot 0 is its insert slot).
const CT_GRID: std::ops::Range<usize> = 1..10;
const CT_RESULT: usize = 10;

/// A pattern's inputs and outputs, as saved on the pattern item: "count*key" strings.
fn pattern_entries(list: &[(AeKey, i64)]) -> Tag {
    Tag::List(list.iter().map(|(k, n)| Tag::String(java(&format!("{n}*{}", key_to_string(k))))).collect())
}

fn read_entries(tag: Option<&Tag>) -> Vec<(AeKey, i64)> {
    let Some(Tag::List(list)) = tag else { return Vec::new() };
    list.iter()
        .filter_map(|t| match t {
            Tag::String(js) => {
                let s = js.to_string().ok()?;
                let (n, key) = s.split_once('*')?;
                Some((key_from_string(key)?, n.parse().ok()?))
            }
            _ => None,
        })
        .collect()
}

/// The pattern an encoded pattern item holds.
fn decode_pattern(game: &Game<'_>, stack: &ItemStack) -> Option<Pattern> {
    if !matches!(&*stack.item, "ae2:crafting_pattern" | "ae2:processing_pattern" | "ae2:smithing_table_pattern" | "ae2:stonecutting_pattern") {
        return None;
    }
    let Some(Tag::Compound(data)) = stack.extra.as_ref()?.other.iter().find(|(k, _)| k == CELL_DATA).map(|(_, t)| t) else {
        return None;
    };
    let Some(Tag::Compound(p)) = data.get(&java(PATTERN_KEY)) else { return None };
    let inputs = read_entries(p.get(&java("in")));
    let outputs = read_entries(p.get(&java("out")));
    if inputs.is_empty() || outputs.is_empty() {
        return None;
    }
    let mut one = stack.clone();
    one.count = 1;
    // Substitutions: `subs` lists, by input, what else its slot takes.
    let subs: Vec<String> = match p.get(&java("subs")) {
        Some(Tag::List(list)) => list.iter().map(|t| if let Tag::String(s) = t { s.to_string().unwrap_or_default() } else { String::new() }).collect(),
        _ => Vec::new(),
    };
    let input_of = |(i, (k, n)): (usize, (AeKey, i64))| {
        let mut input = PatternInput::of(k.clone(), n);
        for alt in subs.get(i).map_or("", String::as_str).split('|').filter(|a| !a.is_empty()) {
            let what = key_of(game, &ItemStack::new(alt, 1));
            if what != k {
                input.possible.push(super::port_ae2_core::GenericStack { what, amount: n });
                input.substitute = true;
            }
        }
        input
    };
    Some(Pattern {
        definition: key_of(game, &one),
        inputs: inputs.into_iter().enumerate().map(input_of).collect(),
        outputs: outputs.into_iter().map(|(what, amount)| super::port_ae2_core::GenericStack { what, amount }).collect(),
    })
}

/// The pattern encoding terminal's buttons: switch crafting/processing, or encode the grid onto a blank pattern.
fn encoder_button(game: &mut Game<'_>, entity: &mut ModBlockEntity, id: i32) {
    if entity.items.len() < 2 {
        return;
    }
    if id == TOGGLE_MODE {
        let mode = nbt_i64(&entity.nbt, "mode");
        set_nbt_i64(&mut entity.nbt, "mode", 1 - mode);
        return;
    }
    if id == CLEAR {
        set_ghosts(entity, &[]);
        return;
    }
    if id == SET_CRAFTING || id == SET_PROCESSING || id == SET_SMITHING || id == SET_STONECUTTING {
        let mode = match id {
            SET_PROCESSING => 1,
            SET_SMITHING => 2,
            SET_STONECUTTING => 3,
            _ => 0,
        };
        set_nbt_i64(&mut entity.nbt, "mode", mode);
        return;
    }
    if id != ENCODE {
        return;
    }
    let blank = entity.items[0].as_ref().is_some_and(|s| &*s.item == "ae2:blank_pattern");
    let rewrite = entity.items[1].as_ref().is_some_and(|s| matches!(&*s.item, "ae2:crafting_pattern" | "ae2:processing_pattern" | "ae2:smithing_table_pattern" | "ae2:stonecutting_pattern"));
    if !(rewrite || blank && entity.items[1].is_none()) {
        return;
    }
    let mode = nbt_i64(&entity.nbt, "mode");
    if mode >= 2 {
        encode_special(game, entity, mode as i32);
        return;
    }
    let cells = enc_ghosts(entity);
    let grid: Vec<Option<ItemStack>> =
        cells.iter().take(9).map(|c| c.as_ref().map(|(id, n)| ItemStack::new(id, (*n).clamp(1, 64) as u32))).collect();
    let set_outputs: Vec<(String, i64)> = (0..27).filter_map(|j| cells[enc_output(j)].clone()).collect();
    let processing = nbt_i64(&entity.nbt, "mode") == 1;
    // A processing pattern's inputs keep their amounts past a stack, in all 81 cells.
    let all_inputs: Vec<(String, i64)> = (0..81).filter_map(|k| cells[enc_input(k)].clone()).collect();
    // Inputs: one of each slot's item for a crafting pattern, the whole stacks for a processing one.
    let mut inputs: Vec<(AeKey, i64)> = Vec::new();
    let entries: Vec<(String, i64)> = if processing { all_inputs.clone() } else { grid.iter().flatten().map(|s| (s.item.to_string(), 1)).collect() };
    for (id, n) in entries {
        let key = if id.starts_with('~') { ghost_key(game, &id) } else { key_of(game, &ItemStack::new(&id, 1)) };
        match inputs.iter_mut().find(|(k, _)| *k == key) {
            Some((_, total)) => *total += n,
            None => inputs.push((key, n)),
        }
    }
    // Substitutions on: each input may be anything its slot's ingredient takes (AECraftingPattern's possible inputs).
    let mut alternatives: Vec<String> = Vec::new();
    if !processing && nbt_i64(&entity.nbt, "substitute") != 0 {
        let found = game.res.recipes().find(&grid, 3).map(|(name, _)| name.to_owned());
        if let Some(placement) = found.and_then(|name| game.res.recipes().placement(&name)) {
            for (key, _) in &inputs {
                let probe = stack_of(key, 1);
                let list = placement.ingredients.iter().find(|i| i.test(&probe)).map_or_else(Vec::new, |i| i.1.clone());
                alternatives.push(list.join("|"));
            }
        }
    }
    // Processing: the outputs set in its three cells; none set, what a furnace makes of the first input.
    if processing && !set_outputs.is_empty() {
        let outputs: Vec<(AeKey, i64)> = set_outputs.iter().map(|(id, n)| (if id.starts_with('~') { ghost_key(game, id) } else { key_of(game, &ItemStack::new(id, 1)) }, *n)).collect();
        write_pattern(game, entity, true, &inputs, &outputs);
        return;
    }
    let output = if processing {
        grid.iter().flatten().next().and_then(|first| {
            let cooked = game.res.recipes().cook("minecraft:smelting", first)?.result.clone();
            let count = cooked.count * first.count;
            Some((cooked, count))
        })
    } else {
        game.res.recipes().craft(&grid, 3).map(|r| {
            let n = r.count;
            (r, n)
        })
    };
    let Some((out, count)) = output else { return };
    let mut out_one = out.clone();
    out_one.count = 1;
    let outputs = vec![(key_of(game, &out_one), i64::from(count))];
    write_pattern(game, entity, processing, &inputs, &outputs);
    if !alternatives.is_empty()
        && let Some(pattern) = entity.items[1].as_mut()
        && let Some((_, Tag::Compound(data))) = pattern.extra.as_mut().and_then(|e| e.other.iter_mut().find(|(k, _)| k == CELL_DATA))
        && let Some(Tag::Compound(p)) = data.get_mut(&java(PATTERN_KEY))
    {
        p.insert(java("subs"), Tag::List(alternatives.iter().map(|a| Tag::String(java(a))).collect()));
    }
}

/// The encoded pattern into the encoder's output slot, a blank used up.
fn write_pattern(_game: &Game<'_>, entity: &mut ModBlockEntity, processing: bool, inputs: &[(AeKey, i64)], outputs: &[(AeKey, i64)]) {
    let mut pattern = ItemStack::new(if processing { "ae2:processing_pattern" } else { "ae2:crafting_pattern" }, 1);
    let mut p = Compound::new();
    p.insert(java("in"), pattern_entries(inputs));
    p.insert(java("out"), pattern_entries(outputs));
    let mut data = Compound::new();
    data.insert(java(PATTERN_KEY), Tag::Compound(p));
    pattern.extra.get_or_insert_with(Default::default).other.push((CELL_DATA.to_owned(), Tag::Compound(data)));
    let rewrite = entity.items[1].is_some();
    entity.items[1] = Some(pattern);
    if rewrite {
        return;
    }
    if let Some(b) = entity.items[0].as_mut() {
        b.count -= 1;
        if b.count == 0 {
            entity.items[0] = None;
        }
    }
}

/// The side of the block at `to` that faces `from` (a neighbour).
fn face_toward(to: Pos, from: Pos) -> horizon_core::geometry::Direction {
    use horizon_core::geometry::Direction;
    match [from[0] - to[0], from[1] - to[1], from[2] - to[2]] {
        [0, 1, 0] => Direction::Up,
        [0, -1, 0] => Direction::Down,
        [0, 0, -1] => Direction::North,
        [0, 0, 1] => Direction::South,
        [-1, 0, 0] => Direction::West,
        _ => Direction::East,
    }
}

/// A molecular assembler's ticks a craft, by its acceleration cards.
const ASSEMBLER_TICKS: [u64; 6] = [10, 8, 6, 4, 2, 1];

/// A CPU's view of its network for `execution.rs`: storage, the providers, power.
struct CpuNet<'a, 'g> {
    game: &'a mut Game<'g>,
    grid: usize,
    cpu: Pos,
    patterns: Vec<(Pos, Arc<Pattern>)>,
    tick: u64,
}

impl CpuNet<'_, '_> {
    /// The providers that hold `pattern`, highest priority first (CraftingService's provider order).
    fn providers_of(&self, pattern: &Pattern) -> Vec<Pos> {
        let mut out: Vec<Pos> = self.patterns.iter().filter(|(_, p)| **p == *pattern).map(|(pos, _)| *pos).collect();
        out.sort_by_key(|p| -provider_nbt(self.game, *p, "priority"));
        out
    }
}

impl CpuNetwork for CpuNet<'_, '_> {
    fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        with_storage(self.game, self.grid, None, |st| st.extract(what, amount, mode))
    }
    fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        with_storage(self.game, self.grid, None, |st| st.insert(what, amount, mode))
    }
    fn provider_count(&self, pattern: &Pattern) -> usize {
        self.providers_of(pattern).len()
    }
    /// PatternProviderLogic.pushPattern's refusals: a crafting lock, or blocking mode with a pattern input
    /// still in the machine.
    fn provider_busy(&self, pattern: &Pattern, provider: usize) -> bool {
        let Some(&at) = self.providers_of(pattern).get(provider) else { return true };
        if provider_nbt(self.game, at, "lock_reason") != 0 {
            return true;
        }
        if pattern.definition.id() != "ae2:processing_pattern" {
            // MolecularAssemblerBlockEntity.acceptsPlans: one craft at a time.
            let tick = self.tick;
            return !SIDES.iter().map(|d| add(at, *d)).any(|q| {
                block_id(&*self.game, q) == "ae2:molecular_assembler" && SERVER.with_borrow(|s| s.assembler_jobs.get(&q).is_none_or(|j| j.1 <= tick))
            });
        }
        if provider_nbt(self.game, at, "blocking") == 0 {
            return false;
        }
        let inputs: Vec<AeKey> = self
            .patterns
            .iter()
            .filter(|(p, _)| *p == at)
            .flat_map(|(_, pat)| pat.inputs.iter().filter_map(|i| i.possible.first().map(|g| g.what.clone())))
            .collect();
        SIDES.iter().map(|d| add(at, *d)).any(|q| {
            let b = block_id(&*self.game, q);
            if b.starts_with("ae2:") {
                return false;
            }
            self.game.world.block_entities.get(&(q[0], q[1], q[2])).is_some_and(|be| {
                be.slots().iter().flatten().any(|st| {
                    let mut one = st.clone();
                    one.count = 1;
                    inputs.contains(&key_of(self.game, &one))
                })
            })
        })
    }
    /// A crafting pattern goes to a molecular assembler beside the provider; a processing pattern's inputs go
    /// into the machine beside it (a furnace under or over it, as a hopper would put them).
    fn push_pattern(&mut self, pattern: &Pattern, provider: usize, inputs: &[super::port_ae2_core::KeyCounter]) -> bool {
        let Some(&at) = self.providers_of(pattern).get(provider) else { return false };
        let outputs: Vec<(AeKey, i64)> = pattern.outputs.iter().map(|o| (o.what.clone(), o.amount)).collect();
        if pattern.definition.id() != "ae2:processing_pattern" {
            let tick_now = self.tick;
            let assembler = SIDES.iter().map(|d| add(at, *d)).find(|q| {
                block_id(&*self.game, *q) == "ae2:molecular_assembler" && SERVER.with_borrow(|s| s.assembler_jobs.get(q).is_none_or(|j| j.1 <= tick_now))
            });
            let Some(assembler) = assembler else {
                return false;
            };
            let (tick, cpu) = (self.tick, self.cpu);
            let item = outputs.first().map_or(String::new(), |(k, _)| k.id().to_string());
            // Acceleration cards in the assembler: AE2's 0 to 5, down to a craft a tick.
            let speed = self.game.native_with_mod_block_entity(assembler, |e| cards(e, 1, "ae2:speed_card")).unwrap_or(0);
            let ticks = ASSEMBLER_TICKS[speed.min(5)];
            let now = self.tick;
            SERVER.with_borrow_mut(|s| {
                let e = s.assembler_cycles.entry(assembler).or_insert((speed, now, 0.0));
                if e.0 != speed {
                    *e = (speed, now, 0.0);
                } else {
                    let cycle = (now - e.1) as f64;
                    // Back to back only: a gap is the job's, not the assembler's.
                    if cycle >= ticks as f64 && cycle < ticks as f64 * 2.0 {
                        e.2 = if e.2 == 0.0 { cycle } else { e.2 * 0.95 + cycle * 0.05 };
                    }
                    e.1 = now;
                }
            });
            let grid: Vec<(String, i64)> = inputs.iter().flat_map(|list| list.iter().map(|(k, n)| (k.id().to_string(), n))).collect();
            SERVER.with_borrow_mut(|s| {
                s.assembler_jobs.insert(assembler, (tick, tick + ticks, item, grid));
                s.assembling.push((tick + ticks, cpu, outputs));
            });
            provider_pushed(self.game, at, pattern);
            return true;
        }
        // PatternProviderLogic.pushPattern: the first side that takes all of it, through the face it touches (a
        // furnace takes what cooks from above, fuel from the side). One that won't take it all takes none, and the
        // ingredients stay with the CPU until something does.
        let machines: Vec<Pos> = SIDES
            .iter()
            .map(|d| add(at, *d))
            .filter(|q| {
                let b = block_id(&*self.game, *q);
                let tanks = self.game.native_fluid_tanks(&self.game.dim.id().to_owned(), *q, face_toward(*q, at)).is_some();
                !is_network_block(&b) && (tanks || self.game.world.block_entities.contains_key(&(q[0], q[1], q[2])))
            })
            .collect();
        let mut pushed_to = None;
        for machine in machines {
            let spot = (machine[0], machine[1], machine[2]);
            let before = self.game.world.block_entities.get(&spot).cloned();
            let face = face_toward(machine, at);
            let mut took_all = true;
            'items: for list in inputs {
                for (key, amount) in list.iter() {
                    let refused = if key.key_type() == super::port_ae2_core::KeyType::Fluids {
                        self.game.native_fluid_insert(machine, face, key.id(), amount.min(i64::from(i32::MAX)) as i32) > 0
                    } else {
                        self.game.native_insert_item(machine, face, stack_of(key, amount as u32)).is_some()
                    };
                    if refused {
                        took_all = false;
                        break 'items;
                    }
                }
            }
            if took_all {
                pushed_to = Some(machine);
                break;
            }
            if let Some(be) = before {
                self.game.world.block_entities.insert(spot, be);
            }
        }
        let Some(target) = pushed_to else { return false };
        let cpu = self.cpu;
        SERVER.with_borrow_mut(|s| s.awaiting.push((target, cpu, outputs)));
        provider_pushed(self.game, at, pattern);
        true
    }
    fn extract_power(&mut self, amount: f64, mode: Actionable) -> f64 {
        let grid = self.grid;
        SERVER.with_borrow_mut(|s| {
            let Some(g) = s.grids.get(grid) else { return 0.0 };
            if !g.online {
                return 0.0;
            }
            let Some(root) = g.nodes.iter().min().copied() else { return 0.0 };
            let e = s.energy.entry(root).or_insert(0.0);
            let got = amount.min(*e);
            if mode == Actionable::Modulate && e.is_finite() {
                *e -= got;
            }
            got
        })
    }
}

/// Runs each CPU's tick, hands back what assemblers made and machines finished, and tells requesters.
fn run_cpus(game: &mut Game<'_>, tick: u64) {
    let monitor = TICK_MONITOR.load(std::sync::atomic::Ordering::Relaxed).then(std::time::Instant::now);
    run_cpus_now(game, tick);
    if let Some(started) = monitor {
        thread_local! {
            static SPENT: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
        }
        let total = SPENT.get() + started.elapsed().as_secs_f64();
        if tick % 100 == 0 {
            log::info!("AE2 crafting CPUs: {:.3} ms a tick over the last 100 ticks", total * 10.0);
            SPENT.set(0.0);
        } else {
            SPENT.set(total);
        }
    }
}

fn run_cpus_now(game: &mut Game<'_>, tick: u64) {
    let jobs: Vec<(Pos, usize, Vec<(Pos, Arc<Pattern>)>)> = SERVER.with_borrow(|s| {
        s.grids
            .iter()
            .enumerate()
            .flat_map(|(i, g)| g.cpus.iter().map(move |(key, _, _)| (*key, i, g.patterns.clone())))
            .filter(|(key, _, _)| s.cpus.get(key).is_some_and(|c| c.has_job() || c.active))
            .collect()
    });
    use super::port_ae2_grid::CPU_WATCH;
    // CraftingServiceStorage: what entered each network since the last tick, for the CPUs that wait for it.
    let mut arrived = CPU_WATCH.with_borrow_mut(|w| {
        w.quiet = true;
        std::mem::take(&mut w.log)
    });
    let mut watching: Vec<AeKey> = Vec::new();
    for (key, grid, patterns) in jobs {
        let Some(mut cpu) = SERVER.with_borrow_mut(|s| s.cpus.remove(&key)) else { continue };
        let mut net = CpuNet { game, grid, cpu: key, patterns, tick };
        cpu.tick(&mut net);
        for (on, what, amount) in &mut arrived {
            let take = cpu.waiting_for(what).min(*amount);
            if *on != grid || take <= 0 {
                continue;
            }
            let got = net.extract(what, take, Actionable::Modulate);
            if got > 0 {
                *amount -= got;
                let taken = cpu.insert(what, got, Actionable::Modulate, &mut net);
                if taken < got {
                    net.insert(what, got - taken, Actionable::Modulate);
                }
            }
        }
        // Assemblers that are done, and machines with output, hand it to this CPU (what it doesn't wait for
        // goes to storage).
        let ready: Vec<Vec<(AeKey, i64)>> = SERVER.with_borrow_mut(|s| {
            let (done, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut s.assembling).into_iter().partition(|(t, c, _)| *c == key && *t <= tick);
            s.assembling = wait;
            done.into_iter().map(|(_, _, out)| out).collect()
        });
        for outputs in ready {
            for (what, amount) in outputs {
                let taken = cpu.insert(&what, amount, Actionable::Modulate, &mut net);
                if taken < amount {
                    net.insert(&what, amount - taken, Actionable::Modulate);
                }
            }
        }
        let waits: Vec<(usize, Pos, Vec<(AeKey, i64)>)> = SERVER.with_borrow(|s| {
            s.awaiting.iter().enumerate().filter(|(_, (_, c, _))| *c == key).map(|(i, (t, _, o))| (i, *t, o.clone())).collect()
        });
        let mut remaining: Vec<(usize, Vec<(AeKey, i64)>)> = Vec::new();
        for (i, target, mut owed) in waits {
            let found: Vec<(usize, ItemStack)> = net
                .game
                .world
                .block_entities
                .get(&(target[0], target[1], target[2]))
                .map(|be| be.slots().iter().enumerate().filter_map(|(n, s)| s.clone().map(|s| (n, s))).collect())
                .unwrap_or_default();
            let mut took_any = false;
            for (slot, stack) in found {
                let key = key_of(net.game, &{
                    let mut one = stack.clone();
                    one.count = 1;
                    one
                });
                let Some(entry) = owed.iter_mut().find(|(k, n)| *k == key && *n > 0) else { continue };
                let take = (i64::from(stack.count)).min(entry.1);
                entry.1 -= take;
                if let Some(be) = net.game.world.block_entities.get_mut(&(target[0], target[1], target[2]))
                    && let Some(s) = be.slots_mut().get_mut(slot)
                    && let Some(st) = s.as_mut()
                {
                    st.count -= take as u32;
                    if st.count == 0 {
                        *s = None;
                    }
                }
                took_any = true;
                SERVER.with_borrow_mut(|s| {
                    let e = s.machine_times.entry(target).or_insert((tick, 0.0));
                    let each = (tick - e.0) as f64 / take.max(1) as f64;
                    // The first output and long idle gaps say nothing of its speed.
                    if tick > e.0 && (e.1 == 0.0 || each < e.1 * 4.0) {
                        e.1 = if e.1 == 0.0 { each } else { e.1 * 0.8 + each * 0.2 };
                    }
                    e.0 = tick;
                });
                for d in SIDES {
                    provider_returned(net.game, add(target, d), &key, take);
                }
                let taken = cpu.insert(&key, take, Actionable::Modulate, &mut net);
                if taken < take {
                    net.insert(&key, take - taken, Actionable::Modulate);
                }
            }
            // What it owes in fluids: out of its tanks, to the millibucket.
            let dim = net.game.dim.id().to_owned();
            for entry in owed.iter_mut().filter(|e| e.0.key_type() == super::port_ae2_core::KeyType::Fluids && e.1 > 0) {
                let what = entry.0.clone();
                for d in SIDES {
                    let face = face_toward(target, add(target, d));
                    let Some(tanks) = net.game.native_fluid_tanks(&dim, target, face) else { continue };
                    let has: i64 = tanks.iter().filter(|t| t.can_drain && t.fluid.as_deref() == Some(what.id())).map(|t| i64::from(t.amount)).sum();
                    if has <= 0 {
                        continue;
                    }
                    let Some((fluid, got)) = net.game.native_fluid_extract(&dim, target, face, has.min(entry.1) as i32) else { continue };
                    if fluid != what.id() {
                        net.game.native_fluid_insert(target, face, &fluid, got);
                        continue;
                    }
                    let got = i64::from(got);
                    entry.1 -= got;
                    took_any = true;
                    let taken = cpu.insert(&what, got, Actionable::Modulate, &mut net);
                    if taken < got {
                        net.insert(&what, got - taken, Actionable::Modulate);
                    }
                    break;
                }
            }
            if took_any {
                net.game.container_changed((target[0], target[1], target[2]));
            }
            remaining.push((i, owed));
        }
        SERVER.with_borrow_mut(|s| {
            for (i, owed) in remaining {
                if let Some(w) = s.awaiting.get_mut(i) {
                    w.2 = owed;
                }
            }
            s.awaiting.retain(|(_, _, owed)| owed.iter().any(|(_, n)| *n > 0));
        });
        let events = cpu.take_events();
        drop(net);
        for event in events {
            if let CpuEvent::Finished { .. } | CpuEvent::Cancelled { .. } = event {
                let who = SERVER.with_borrow_mut(|s| s.requesters.remove(&key));
                let what = SERVER.with_borrow_mut(|s| s.request_items.remove(&key));
                if let (true, Some(uuid), Some((id, amount))) = (matches!(event, CpuEvent::Finished { .. }), who.filter(|u| !SERVER.with_borrow(|s| s.no_notify.contains(u))), what) {
                    let _ = game.native_net_send_to_player(uuid, NET_UI, format!("jobdone {id} {amount}").as_bytes());
                }
            }
        }
        watching.extend(cpu.waiting_keys());
        SERVER.with_borrow_mut(|s| s.cpus.insert(key, cpu));
    }
    CPU_WATCH.with_borrow_mut(|w| {
        w.quiet = false;
        w.keys = watching;
    });
}

/// A craft asked from a terminal: plan it, then start it on the grid's CPU, telling the player what happens.
fn request_craft(game: &mut Game<'_>, grid: usize, uuid: u128, key: &AeKey, amount: i64) {
    if let Err(e) = submit_craft(game, grid, uuid, key, amount, None) {
        game.native_action_bar(uuid, horizon_client::text::Text::legacy(&format!("§c{e}")));
    }
}

/// CraftingService.submitJob: the CPU a job goes to. `chosen` is the one the plan's button picked; with none
/// (Automatic), findSuitableCraftingCPU: of those online, free, big enough and not kept for the other kind of
/// request, one kept for this kind first, then the most co-processors, then the least storage that fits. A uuid
/// of 0 is a machine's request.
fn pick_cpu(game: &Game<'_>, grid: usize, bytes: i64, player: bool, chosen: Option<usize>) -> Result<Pos, String> {
    let cpus = SERVER.with_borrow(|s| s.grids.get(grid).map_or(Vec::new(), |g| g.cpus.clone()));
    let busy_cpu = |key: &Pos| SERVER.with_borrow(|s| s.cpus.get(key).is_some_and(CraftingCpu::has_job));
    let active = |key: &Pos| SERVER.with_borrow(|s| s.cpus.get(key).is_some_and(|c| c.active));
    if let Some(i) = chosen {
        let Some(&(key, size, _)) = cpus.get(i) else { return Err("The selected CPU is offline.".to_owned()) };
        if !active(&key) {
            return Err("The selected CPU is offline.".to_owned());
        }
        if busy_cpu(&key) {
            return Err("The selected CPU is already working on another job.".to_owned());
        }
        if size < bytes {
            return Err("The selected CPU does not have enough storage.".to_owned());
        }
        return Ok(key);
    }
    if cpus.is_empty() {
        return Err("There are no crafting CPUs on the network.".to_owned());
    }
    let (mut offline, mut busy, mut small, mut excluded) = (0, 0, 0, 0);
    let mut valid = Vec::new();
    for &(key, size, accel) in &cpus {
        let mode = provider_nbt(game, key, "cpu_mode");
        if !active(&key) {
            offline += 1;
        } else if busy_cpu(&key) {
            busy += 1;
        } else if size < bytes {
            small += 1;
        } else if (mode == 1 && !player) || (mode == 2 && player) {
            excluded += 1;
        } else {
            valid.push((mode == 0, std::cmp::Reverse(accel), size, key));
        }
    }
    valid.sort();
    if let Some(first) = valid.first() {
        return Ok(first.3);
    }
    let mut why = Vec::new();
    for (n, word) in [(offline, "offline"), (busy, "busy"), (small, "too small"), (excluded, "excluded")] {
        if n > 0 {
            why.push(format!("{n} {word}"));
        }
    }
    Err(format!("None of the crafting CPUs are suitable for this job. ({})", why.join(", ")))
}

fn submit_craft(game: &mut Game<'_>, grid: usize, uuid: u128, key: &AeKey, amount: i64, chosen: Option<usize>) -> Result<(), String> {
    let patterns = SERVER.with_borrow(|s| s.grids.get(grid).map_or(Vec::new(), |g| g.patterns.clone()));
    let mut book = PatternBook::new();
    for (_, p) in &patterns {
        book.add((**p).clone(), 0);
    }
    for key in emitable_keys(game, grid) {
        book.add_emitable(key);
    }
    let storage = with_storage(game, grid, None, |st| st.contents());
    let plan = calculate(&book, &storage, key, amount, CalculationStrategy::ReportMissingItems).map_err(|e| format!("Can't plan that: {e}"))?;
    if plan.simulation {
        let missing: Vec<String> = plan.missing_items.iter().filter(|(_, n)| *n > 0).map(|(k, n)| format!("{n} {}", k.id())).collect();
        return Err(format!("Missing: {}", missing.join(", ")));
    }
    let cpu_key = pick_cpu(game, grid, plan.bytes, uuid != 0, chosen)?;
    let Some(mut cpu) = SERVER.with_borrow_mut(|s| s.cpus.remove(&cpu_key)) else {
        return Err("The crafting CPU isn't ready yet".to_owned());
    };
    let tick = SERVER.with_borrow(|s| s.tick);
    let result = {
        let mut net = CpuNet { game, grid, cpu: cpu_key, patterns, tick };
        cpu.submit(&plan, &mut net, false, None)
    };
    SERVER.with_borrow_mut(|s| s.cpus.insert(cpu_key, cpu));
    result.map_err(|e| format!("Can't start: {e:?}"))?;
    SERVER.with_borrow_mut(|s| {
        s.requesters.insert(cpu_key, uuid);
        s.request_items.insert(cpu_key, (wire_id(key), amount));
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------
// The inscriber, budding quartz and the growth accelerator.

/// Ticks an inscribe takes, and AE a tick it draws from its network.
const INSCRIBE_TICKS: i32 = 100;
const INSCRIBE_AE_PER_TICK: f64 = 10.0;

/// An inscriber recipe: its three ingredients (top and bottom optional), whether the presses are used up
/// (`press`; `inscribe` keeps them), and its result.
struct InscriberRecipe {
    top: Option<HashSet<String>>,
    middle: HashSet<String>,
    bottom: Option<HashSet<String>>,
    press: bool,
    result: ItemStack,
}

fn inscriber_recipes(game: &Game<'_>) -> Vec<InscriberRecipe> {
    game.native_recipes_of_type("ae2:inscriber")
        .iter()
        .filter_map(|r| {
            let json = &*r.json;
            let ing = |slot: &str| json.get("ingredients")?.get(slot).and_then(|v| game.native_recipe_ingredient(v)).map(|i| i.0);
            Some(InscriberRecipe {
                top: ing("top"),
                middle: ing("middle")?,
                bottom: ing("bottom"),
                press: json.get("mode").and_then(|m| m.as_str()) == Some("press"),
                result: game.native_recipe_result(json.get("result")?)?,
            })
        })
        .collect()
}

fn fits(want: &Option<HashSet<String>>, have: &Option<ItemStack>) -> bool {
    match (want, have) {
        (None, None) => true,
        (Some(set), Some(stack)) => set.contains(&*stack.item),
        _ => false,
    }
}

/// The inscriber: the recipe its slots make runs for `INSCRIBE_TICKS` on network power, then the middle (and,
/// pressing, the top and bottom) is used up and the result joins the output.
fn inscriber_tick(game: &mut Game<'_>, pos: Pos, on_grid: bool, entity: &mut ModBlockEntity) {
    if entity.items.len() < 4 {
        return;
    }
    entity.data.resize(3, 0);
    entity.data[2] = nbt_i64(&entity.nbt, "auto_export") as i32;
    // InscriberBlockEntity.tickingRequest: Auto-Export pushes the result into what's beside it.
    if nbt_i64(&entity.nbt, "auto_export") != 0
        && let Some(result) = entity.items[3].take()
    {
        let mut left = Some(result);
        for d in SIDES {
            let Some(stack) = left.take() else { break };
            let target = add(pos, d);
            let face = face_toward(target, pos);
            left = game.native_insert_item(target, face, stack);
        }
        entity.items[3] = left;
    }
    // Each tick's progress towards 100 by Acceleration Cards, and 10 AE a point of it.
    let speed = [2, 3, 5, 10, 50][cards(entity, 4, "ae2:speed_card").min(4)];
    entity.data[1] = INSCRIBE_TICKS / speed;
    let all = inscriber_recipes(game);
    let recipe = all.into_iter().find(|r| {
        entity.items[1].as_ref().is_some_and(|m| r.middle.contains(&*m.item))
            && fits(&r.top, &entity.items[0])
            && fits(&r.bottom, &entity.items[2])
    });
    let room = |r: &InscriberRecipe, out: &Option<ItemStack>| match out {
        None => true,
        Some(o) => o.item == r.result.item && o.count + r.result.count <= max_stack(game, &o.item),
    };
    let Some(recipe) = recipe.filter(|r| room(r, &entity.items[3])) else {
        entity.data[0] = 0;
        return;
    };
    if !on_grid || !take_energy(pos, INSCRIBE_AE_PER_TICK * f64::from(speed)) {
        return;
    }
    entity.data[0] += 1;
    if entity.data[0] < entity.data[1] {
        return;
    }
    entity.data[0] = 0;
    let used: &[usize] = if recipe.press { &[0, 1, 2] } else { &[1] };
    for &slot in used {
        if let Some(s) = entity.items[slot].as_mut() {
            s.count -= 1;
            if s.count == 0 {
                entity.items[slot] = None;
            }
        }
    }
    match entity.items[3].as_mut() {
        Some(o) => o.count += recipe.result.count,
        None => entity.items[3] = Some(recipe.result.clone()),
    }
}

/// The quartz bud stages, smallest first, and what a cluster is.
const BUDS: [&str; 4] = ["ae2:small_quartz_bud", "ae2:medium_quartz_bud", "ae2:large_quartz_bud", "ae2:quartz_cluster"];
/// The chance a budding block grows a bud a tick (about every 40 seconds a side), and what a growth
/// accelerator next to it multiplies it by while powered.
const BUD_CHANCE: f64 = 1.0 / 800.0;
const ACCELERATED: f64 = 10.0;
/// What a flawed budding block becomes when a growth wears it down, and how likely that is a growth.
const DECAY: [(&str, &str); 3] = [
    ("ae2:flawed_budding_quartz", "ae2:chipped_budding_quartz"),
    ("ae2:chipped_budding_quartz", "ae2:damaged_budding_quartz"),
    ("ae2:damaged_budding_quartz", "ae2:quartz_block"),
];
const DECAY_CHANCE: f64 = 0.08;

fn chance(game: &mut Game<'_>, p: f64) -> bool {
    use horizon_core::random::RandomSource;
    let dim = game.dim.id().to_owned();
    game.native_level_random(&dim).is_some_and(|r| (r.next_float() as f64) < p)
}

/// A budding quartz block's tick: now and then a bud grows on a free side, or a bud there grows a stage.
fn budding_tick(game: &mut Game<'_>, pos: Pos, block: &str) {
    let accelerated = SIDES.iter().any(|d| {
        let q = add(pos, *d);
        block_id(game, q) == "ae2:growth_accelerator" && game.native_block_property(q, "powered").as_deref() == Some("true")
    });
    let p = if accelerated { BUD_CHANCE * ACCELERATED } else { BUD_CHANCE };
    if !chance(game, p) {
        return;
    }
    let side = {
        use horizon_core::random::RandomSource;
        let dim = game.dim.id().to_owned();
        game.native_level_random(&dim).and_then(|r| r.next_int_bound(6).ok()).unwrap_or(0) as usize
    };
    let target = add(pos, SIDES[side]);
    let facing = ["down", "up", "north", "south", "west", "east"][side];
    let here = block_id(game, target);
    let next = if here == "minecraft:air" || here == "minecraft:water" {
        Some(BUDS[0])
    } else {
        BUDS.iter().position(|b| *b == here).filter(|i| *i + 1 < BUDS.len()).map(|i| BUDS[i + 1])
    };
    let Some(next) = next else { return };
    let dim = game.dim.id().to_owned();
    if let Some(state) = game.pack.assets.registry.default_state(next) {
        game.native_set_block(&dim, target, state, 3);
        game.native_set_block_property_with(target, "facing", facing, 3);
    }
    // A flawed budding block may wear down a stage as it grows (flawless ones never do).
    if let Some((_, worn)) = DECAY.iter().find(|(from, _)| *from == block)
        && chance(game, DECAY_CHANCE)
        && let Some(state) = game.pack.assets.registry.default_state(worn)
    {
        game.native_set_block(&dim, pos, state, 3);
    }
}

// ---------------------------------------------------------------------------------------------------------
// M3: buses.

/// A container a storage bus faces, as storage: each slot's key, count and stack size; what its config allows.
/// A storage bus' view of a container through the face it touches: its slots (key, amount, stack size), which
/// the face lets in and out, and the bus' filter.
struct External {
    slots: Vec<Option<(AeKey, i64, i64)>>,
    access: Vec<(bool, bool)>,
    filter: BusFilter,
    changed: bool,
}

impl super::port_ae2_grid::MeStorage for External {
    fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        if what.key_type() != super::port_ae2_core::KeyType::Items || self.filter.access == 1 || !self.filter.allows(what) {
            return 0;
        }
        let mut left = amount;
        // Onto equal stacks first, then into empty slots (as a hopper would), where the face lets it in.
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let Some((k, n, max)) = slot
                && self.access[i].0
                && k == what
                && *n < *max
            {
                let put = left.min(*max - *n);
                if mode == Actionable::Modulate {
                    *n += put;
                    self.changed = true;
                }
                left -= put;
            }
        }
        let room = MAX_STACKS.with_borrow(|m| m.get(what.id()).copied().unwrap_or(64)).max(1);
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if left <= 0 {
                break;
            }
            if slot.is_none() && self.access[i].0 {
                let put = left.min(room);
                if mode == Actionable::Modulate {
                    *slot = Some((what.clone(), put, room));
                    self.changed = true;
                }
                left -= put;
            }
        }
        amount - left
    }
    fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        if self.filter.access == 2 || !(self.filter.no_extract_filter || self.filter.allows(what)) {
            return 0;
        }
        let mut got = 0;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if got >= amount {
                break;
            }
            if let Some((k, n, _)) = slot
                && self.access[i].1
                && k == what
            {
                let take = (amount - got).min(*n);
                got += take;
                if mode == Actionable::Modulate {
                    *n -= take;
                    self.changed = true;
                    if *n == 0 {
                        *slot = None;
                    }
                }
            }
        }
        got
    }
    fn available_stacks(&self, out: &mut super::port_ae2_core::KeyCounter) {
        if self.filter.access == 2 {
            return;
        }
        for (i, slot) in self.slots.iter().enumerate() {
            if let Some((k, n, _)) = slot
                && (self.access[i].1 || self.filter.report_all)
                && (self.filter.no_extract_filter || self.filter.allows(k))
            {
                out.add(k, *n);
            }
        }
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// `native_tooltip`: a storage cell's bytes and types in use, as AE2's cell tooltip shows them.
pub fn tooltip(stack: &ItemStack, lines: &mut Vec<(horizon_client::text::Text, u32)>) {
    tooltip_inner(stack, lines);
    // AEBasePoweredItem.appendHoverText: the stored energy, first under the name.
    if let Some((most, _)) = item_battery(&stack.item) {
        lines.insert(1.min(lines.len()), (energy_line(portable_power(stack), most), u32::MAX));
    }
}

fn tooltip_inner(stack: &ItemStack, lines: &mut Vec<(horizon_client::text::Text, u32)>) {
    // The guide key opens the page of what the pointer is over (AE2's "Press W" hint).
    super::port_ae2_guide::hovered(&stack.item);
    if super::port_ae2_guide::has_page(&stack.item) {
        lines.push((horizon_client::text::Text::legacy("§8Press W to open the AE2 guide"), u32::MAX));
    }
    if matches!(&*stack.item, "ae2:crafting_pattern" | "ae2:processing_pattern" | "ae2:smithing_table_pattern" | "ae2:stonecutting_pattern") {
        pattern_tooltip(stack, lines);
        return;
    }
    let line = |s: String| (horizon_client::text::Text::legacy(&s), u32::MAX);
    // AE2's upgrade cards (`Upgrades.addTooltip`): what takes them, and how many.
    if let Some(card) = ae2_name(&stack.item).filter(|c| c.ends_with("_card")) {
        let mut supported: Vec<(&str, u32)> = Vec::new();
        for (c, label, max, _) in UPGRADES {
            if *c == card && !supported.iter().any(|(l, _)| l == label) {
                supported.push((label, *max));
            }
        }
        supported.sort_by_key(|e| e.1);
        if !supported.is_empty() {
            let at = 1.min(lines.len());
            lines.insert(at, line("§7Supported by:".to_owned()));
            for (i, (what, n)) in supported.iter().enumerate() {
                lines.insert(at + 1 + i, line(format!("§8  {what} ({n})")));
            }
        }
        return;
    }
    if &*stack.item == "ae2:facade" {
        if let Some(block) = facade_block(stack)
            && let Some(first) = lines.first_mut()
        {
            first.0 = horizon_client::text::Text::legacy(&format!("{} - {}", first.0.plain(), item_words(&block)));
        }
        return;
    }
    if &*stack.item == "ae2:color_applicator" {
        let color = applicator_color(stack).map_or_else(String::new, |c| if c == "fluix" { "Fluix".to_owned() } else { item_words(c) });
        if let Some(first) = lines.first_mut() {
            first.0 = horizon_client::text::Text::legacy(&format!("{} - {color}", first.0.plain()));
        }
        return;
    }
    if let Some(Tag::Compound(d)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t)
        && let Some(Tag::IntArray(s)) = d.get(&java("ae2:spatial_size"))
    {
        lines.insert(1.min(lines.len()), line(format!("§7Stored Size: {}x{}x{}", s.first().unwrap_or(&0), s.get(1).unwrap_or(&0), s.get(2).unwrap_or(&0))));
        return;
    }
    // SpatialStorageCellItem: unformatted, and what it can take.
    if let Some(max) = ae2_name(&stack.item).and_then(|n| n.strip_prefix("spatial_storage_cell_")) {
        lines.insert(1.min(lines.len()), line("§7§oUnformatted".to_owned()));
        lines.insert(2.min(lines.len()), line(format!("§7Capacity: {max}x{max}x{max}")));
        return;
    }
    // SkyStoneTankBlock.appendHoverText, AnnihilationPlanePartItem.
    // IntrinsicEnchantItem: fluix tools always have this at the least.
    let intrinsic = match ae2_name(&stack.item) {
        Some("fluix_sword") => Some("Looting I"),
        Some("fluix_pickaxe" | "fluix_axe" | "fluix_shovel" | "fluix_spade" | "fluix_hoe") => Some("Fortune I"),
        _ => None,
    };
    if let Some(what) = intrinsic {
        lines.insert(1.min(lines.len()), line(format!("§7Always has at least {what}")));
        return;
    }
    if &*stack.item == "ae2:sky_stone_tank" {
        lines.insert(1.min(lines.len()), line("§7Can Store up to 16 Buckets".to_owned()));
        return;
    }
    if &*stack.item == "ae2:annihilation_plane" {
        let enchanted = stack.extra.as_ref().is_some_and(|e| !e.enchantments.is_empty());
        lines.insert(lines.len().min(1 + usize::from(enchanted)), line(if enchanted { "§7Enchants increase energy use" } else { "§7Can be enchanted" }.to_owned()));
        return;
    }
    // Wireless terminals: whether they're linked to an access point (AE2's Linked / Unlinked).
    if is_wireless_terminal(&stack.item) {
        let text = match link_of(stack) {
            Some(p) => format!("§7Linked: {}, {}, {}", p[0], p[1], p[2]),
            None => "§7Unlinked".to_owned(),
        };
        lines.insert(1.min(lines.len()), line(text));
        return;
    }
    // AE2's portable cells: their battery (AEBasePoweredItem's "Stored Energy") and the cell's use.
    if portable_type(&stack.item).is_some() {
        let power = portable_power(stack);
        let pct = (power * 100.0 / PORTABLE_BATTERY).round();
        lines.insert(1.min(lines.len()), line(format!("§7Stored Energy: {} AE / {} AE ({pct}%)", readable_number::format_i64(power as i64, 6), readable_number::format_i64(PORTABLE_BATTERY as i64, 6))));
    }
    let Some(cell) = cell_type(&stack.item).or_else(|| portable_type(&stack.item)) else { return };
    let inv = BasicCellInventory::new(cell, &[], cell_fuzzy(stack), cell_upgrades(stack), &read_cell(stack));
    let (used, total) = (inv.used_bytes(), inv.total_bytes());
    let pct = if total > 0 { used * 100 / total } else { 0 };
    let mut at = 1.min(lines.len());
    let mut add = |lines: &mut Vec<(horizon_client::text::Text, u32)>, text: String| {
        lines.insert(at.min(lines.len()), (horizon_client::text::Text::legacy(&text), u32::MAX));
        at += 1;
    };
    add(lines, format!("§7{used} of {total} Bytes Used ({pct}%)"));
    add(lines, format!("§7{} of {} Types", inv.stored_item_types(), inv.total_item_types()));
    // BasicCellHandler: a cell with a partition says how it filters.
    let installed = cell_cards(stack);
    if !cell_config(stack).is_empty() {
        let list = if installed.iter().any(|c| c.as_str() == "ae2:inverter_card") { "Excluded" } else { "Included" };
        let kind = if installed.iter().any(|c| c.as_str() == "ae2:fuzzy_card") { "Fuzzy" } else { "Precise" };
        add(lines, format!("§7Partitioned - {list} {kind}"));
    }
    // StorageCellClientTooltipComponent: the first five it holds, then its cards (words here: tooltips take no icons).
    let mut held = super::port_ae2_core::KeyCounter::new();
    inv.available_stacks(&mut held);
    let mut stored: Vec<(AeKey, i64)> = held.iter().filter(|(_, n)| *n > 0).map(|(k, n)| (k.clone(), n)).collect();
    stored.sort_by_key(|e| std::cmp::Reverse(e.1));
    for (key, n) in stored.iter().take(5) {
        add(lines, format!("§8  {} {}", key.format_amount(*n, super::port_ae2_core::AmountFormat::Slot), item_words(key.id())));
    }
    if stored.len() > 5 {
        add(lines, "§8  ...".to_owned());
    }
    if !installed.is_empty() {
        add(lines, format!("§7Upgrades: {}", installed.iter().map(|c| item_words(c)).collect::<Vec<_>>().join(", ")));
    }
}

/// An item id as words: `minecraft:oak_planks` -> "Oak Planks".
pub fn item_words(id: &str) -> String {
    let path = id.split_once(':').map_or(id, |(_, p)| p);
    path.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// InitUpgrades: card, how AE2 names what takes it, how many, and the machines it stands for.
const UPGRADES: &[(&str, &str, u32, &[&str])] = &[
    ("crafting_card", "ME Interface", 1, &["interface", "cable_interface"]),
    ("fuzzy_card", "ME Interface", 1, &["interface", "cable_interface"]),
    ("speed_card", "ME IO Port", 3, &["io_port"]),
    ("redstone_card", "ME IO Port", 1, &["io_port"]),
    ("fuzzy_card", "ME Level Emitter", 1, &["level_emitter"]),
    ("crafting_card", "ME Level Emitter", 1, &["level_emitter"]),
    ("fuzzy_card", "ME Import/Export Bus", 1, &["import_bus", "export_bus"]),
    ("redstone_card", "ME Import/Export Bus", 1, &["import_bus", "export_bus"]),
    ("capacity_card", "ME Import/Export Bus", 5, &["import_bus", "export_bus"]),
    ("speed_card", "ME Import/Export Bus", 4, &["import_bus", "export_bus"]),
    ("inverter_card", "ME Import/Export Bus", 1, &["import_bus"]),
    ("crafting_card", "ME Import/Export Bus", 1, &["export_bus"]),
    ("fuzzy_card", "ME Storage Cells", 1, &[]),
    ("inverter_card", "ME Storage Cells", 1, &[]),
    ("equal_distribution_card", "ME Storage Cells", 1, &[]),
    ("void_card", "ME Storage Cells", 1, &[]),
    ("fuzzy_card", "Portable Cells", 1, &[]),
    ("inverter_card", "Portable Cells", 1, &[]),
    ("equal_distribution_card", "Portable Cells", 1, &[]),
    ("void_card", "Portable Cells", 1, &[]),
    ("energy_card", "Portable Cells", 2, &[]),
    ("energy_card", "Wireless Terminals", 2, &[]),
    ("fuzzy_card", "View Cell", 1, &[]),
    ("inverter_card", "View Cell", 1, &[]),
    ("fuzzy_card", "ME Storage Bus", 1, &["storage_bus"]),
    ("inverter_card", "ME Storage Bus", 1, &["storage_bus"]),
    ("capacity_card", "ME Storage Bus", 5, &["storage_bus"]),
    ("void_card", "ME Storage Bus", 1, &["storage_bus"]),
    ("fuzzy_card", "ME Formation Plane", 1, &["formation_plane"]),
    ("inverter_card", "ME Formation Plane", 1, &["formation_plane"]),
    ("capacity_card", "ME Formation Plane", 5, &["formation_plane"]),
    ("energy_card", "Color Applicator", 2, &[]),
    ("equal_distribution_card", "Color Applicator", 1, &[]),
    ("void_card", "Color Applicator", 1, &[]),
    ("energy_card", "Matter Cannon", 2, &[]),
    ("fuzzy_card", "Matter Cannon", 1, &[]),
    ("inverter_card", "Matter Cannon", 1, &[]),
    ("void_card", "Matter Cannon", 1, &[]),
    ("speed_card", "Matter Cannon", 4, &[]),
    ("speed_card", "Molecular Assembler", 5, &["molecular_assembler"]),
    ("speed_card", "Inscriber", 4, &["inscriber"]),
    ("energy_card", "Vibration Chamber", 3, &["vibration_chamber"]),
    ("speed_card", "Vibration Chamber", 3, &["vibration_chamber"]),
];

/// AE2's encoded pattern tooltip: the mode, what it makes and what it takes.
fn pattern_tooltip(stack: &ItemStack, lines: &mut Vec<(horizon_client::text::Text, u32)>) {
    let Some(Tag::Compound(data)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        lines.insert(1.min(lines.len()), (horizon_client::text::Text::legacy("§cInvalid pattern"), u32::MAX));
        return;
    };
    let Some(Tag::Compound(p)) = data.get(&java(PATTERN_KEY)) else { return };
    let inputs = read_entries(p.get(&java("in")));
    let outputs = read_entries(p.get(&java("out")));
    let crafting = &*stack.item == "ae2:crafting_pattern";
    let mut extra = Vec::new();
    extra.push(format!("§7{}", if crafting { "Crafting pattern" } else { "Processing pattern" }));
    if crafting {
        extra.push(format!("§7Using Substitutions: {}", if p.get(&java("subs")).is_some() { "Yes" } else { "No" }));
    }
    for (i, (k, n)) in outputs.iter().enumerate() {
        let head = if i == 0 { if crafting { "Crafts: " } else { "Produces: " } } else { "and " };
        extra.push(format!("§9{head}§f{} x {}", key_amount(&wire_id(k), *n, super::port_ae2_core::AmountFormat::Slot), item_words(k.id())));
    }
    for (i, (k, n)) in inputs.iter().enumerate() {
        let head = if i == 0 { "With: " } else { "and " };
        extra.push(format!("§9{head}§f{} x {}", key_amount(&wire_id(k), *n, super::port_ae2_core::AmountFormat::Slot), item_words(k.id())));
    }
    for (i, l) in extra.into_iter().enumerate() {
        lines.insert((1 + i).min(lines.len()), (horizon_client::text::Text::legacy(&l), u32::MAX));
    }
}

/// `native_net_from_client`: a recipe JEI's "+" sent for the terminal the player has open. The pattern encoding
/// terminal takes it as ghosts (its mode follows the recipe); the crafting terminal's grid goes back to the
/// network and fills with the recipe's items from it, as AE2's transfer does.
pub fn from_client(game: &mut Game<'_>, player: u128, channel: &str, data: &[u8]) {
    if channel == NET_UI {
        ui_from_client(game, player, &String::from_utf8_lossy(data));
        return;
    }
    if channel == NET_WIRELESS {
        let text = String::from_utf8_lossy(data).into_owned();
        let w: Vec<&str> = text.split(' ').collect();
        let using = SERVER.with_borrow(|s| s.wireless.contains(&player));
        match w.as_slice() {
            ["close"] => SERVER.with_borrow_mut(|s| {
                s.wireless.remove(&player);
                s.viewers.remove(&player);
                s.chest_terms.remove(&player);
                s.wcraft_ops.push((player, "close".to_owned()));
            }),
            ["grid", ..] if using => SERVER.with_borrow_mut(|s| s.wcraft_ops.push((player, w[1..].join(" ")))),
            ["insert", slot] if using => {
                if let Ok(slot) = slot.parse::<i32>() {
                    SERVER.with_borrow_mut(|s| s.wireless_inserts.push((player, slot)));
                }
            }
            ["take", i, right, shift] if using => {
                let Some(grid) = SERVER.with_borrow(|s| s.viewers.get(&player).and_then(|p| s.grid_of.get(p)).copied()) else { return };
                let Some((key, shown)) = i.parse::<usize>().ok().and_then(|i| SERVER.with_borrow(|s| s.sent.get(&player).and_then(|v| v.keys.get(i).cloned()))) else { return };
                if shown == 0 {
                    request_craft(game, grid, player, &key, if *shift == "1" { 64 } else if *right == "1" { 16 } else { 1 });
                    return;
                }
                let most = i64::from(max_stack(game, key.id()));
                let have = with_viewer_storage(game, player, |st| st.extract(&key, most, Actionable::Simulate)).unwrap_or(0);
                let want = if *right == "1" { (have + 1) / 2 } else { have };
                let got = with_viewer_storage(game, player, |st| st.extract(&key, want, Actionable::Modulate)).unwrap_or(0);
                if got > 0 && let Some(left) = game.native_give_stack(player, stack_of(&key, got as u32)) {
                    let back = key_of(game, &left);
                    with_viewer_storage(game, player, |st| st.insert(&back, i64::from(left.count), Actionable::Modulate));
                }
            }
            _ => {}
        }
        return;
    }
    if channel == NET_LEVEL {
        let Some(pos) = SERVER.with_borrow(|s| s.viewers.get(&player).copied()) else { return };
        let level = String::from_utf8_lossy(data).trim().parse::<i64>().unwrap_or(0);
        if matches!(block_id(game, pos).as_str(), "ae2:level_emitter" | "ae2:energy_level_emitter") {
            game.native_with_mod_block_entity(pos, |e| set_nbt_i64(&mut e.nbt, "level", level));
        }
        return;
    }
    if channel == NET_DEBUG {
        let position = game.native_server_players().into_iter().find(|p| p.uuid == player).map(|p| p.position);
        if let Some(position) = position {
            let text = String::from_utf8_lossy(data).into_owned();
            if let Some(rest) = text.strip_prefix("ae2") .filter(|r| r.is_empty() || r.starts_with(' ')) {
                let level = game.native_server_players().into_iter().find(|p| p.uuid == player).map_or(0, |p| p.permission);
                let args: Vec<&str> = rest.split_whitespace().collect();
                ae2_run(game, Some(player), level, position, &args);
                return;
            }
            debug_command(game, player, position, &text);
        }
        return;
    }
    if channel != NET_TRANSFER {
        return;
    }
    let Some(pos) = SERVER.with_borrow(|s| s.viewers.get(&player).copied()) else { return };
    let text = String::from_utf8_lossy(data);
    let mut crafting = true;
    let mut width = 3usize;
    let mut slots: Vec<(usize, Vec<String>, i64)> = Vec::new();
    let mut results: Vec<(String, i64)> = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split(' ').collect();
        match parts.as_slice() {
            ["mode", m] => crafting = *m == "crafting",
            ["size", w, _] => width = w.parse::<usize>().unwrap_or(3).clamp(1, 3),
            ["s", i, ids, n] => {
                if let (Ok(i), Ok(n)) = (i.parse::<usize>(), n.parse::<i64>()) {
                    slots.push((i, ids.split(',').map(str::to_owned).collect(), n.max(1)));
                }
            }
            ["r", id, n] => results.push(((*id).to_owned(), n.parse().unwrap_or(1))),
            _ => {}
        }
    }
    // A crafting recipe keeps its shape in the 3x3 grid; a machine's inputs fill it in order.
    let cell = |i: usize| if crafting { (i / width) * 3 + i % width } else { i };
    if SERVER.with_borrow(|s| s.wireless.contains(&player)) {
        if crafting {
            let cells: Vec<String> = slots.iter().map(|(i, ids, _)| format!("{}={}", cell(*i), ids.join(","))).collect();
            SERVER.with_borrow_mut(|s| s.wcraft_ops.push((player, format!("fill {}", cells.join(" ")))));
        }
        return;
    }
    match block_id(game, pos).as_str() {
        "ae2:pattern_encoding_terminal" => {
            let mut cells: Vec<Option<(String, i64)>> = vec![None; ENC_CELLS];
            for (i, ids, n) in &slots {
                if let (Some(c), Some(id)) = (Some(cell(*i)).filter(|c| *c < 9), ids.first()) {
                    cells[c] = Some((id.clone(), if crafting { 1 } else { *n }));
                }
            }
            if !crafting {
                for (j, (id, n)) in results.iter().take(27).enumerate() {
                    cells[enc_output(j)] = Some((id.clone(), *n));
                }
            }
            game.native_with_mod_block_entity(pos, |e| {
                set_nbt_i64(&mut e.nbt, "mode", if crafting { 0 } else { 1 });
                set_ghosts(e, &cells);
            });
        }
        "ae2:crafting_terminal" if crafting => {
            let Some(grid) = SERVER.with_borrow(|s| s.grid_of.get(&pos).copied()) else { return };
            // The grid's items back to the network (what doesn't fit, to the player).
            let old: Vec<ItemStack> = game
                .native_with_mod_block_entity(pos, |e| e.items[CT_GRID].iter_mut().filter_map(Option::take).collect())
                .unwrap_or_default();
            for stack in old {
                let key = key_of(game, &stack);
                let stored = with_storage(game, grid, None, |st| st.insert(&key, i64::from(stack.count), Actionable::Modulate));
                if stored < i64::from(stack.count) {
                    let mut left = stack.clone();
                    left.count -= stored as u32;
                    let _ = game.native_give_stack(player, left);
                }
            }
            // One of each ingredient from the network, the first alternative it has.
            SERVER.with_borrow_mut(|s| s.terminal_fills.retain(|f| f.0 != player));
            let mut fill: Vec<(usize, ItemStack)> = Vec::new();
            for (i, ids, _) in &slots {
                let c = cell(*i);
                if c >= 9 {
                    continue;
                }
                let mut found = false;
                for id in ids {
                    let key = key_of(game, &ItemStack::new(id, 1));
                    if with_storage(game, grid, None, |st| st.extract(&key, 1, Actionable::Modulate)) > 0 {
                        fill.push((c, stack_of(&key, 1)));
                        found = true;
                        break;
                    }
                }
                // What the network hasn't comes out of the player's inventory, once their state is to hand.
                if !found {
                    SERVER.with_borrow_mut(|s| s.terminal_fills.push((player, pos, c, ids.clone())));
                }
            }
            game.native_with_mod_block_entity(pos, |e| {
                for (c, stack) in fill {
                    if let Some(slot) = e.items.get_mut(CT_GRID.start + c) {
                        *slot = Some(stack);
                    }
                }
            });
        }
        _ => {}
    }
}

/// AE2's guide (W over an item in JEI's panel): the item's page.
pub fn open_guide(game: &mut Game<'_>, item: &str) {
    super::port_ae2_guide::open_item(game, item);
}

fn is_terminal_menu(menu: Option<&str>) -> bool {
    matches!(menu, Some("ae2:terminal" | "ae2:crafting_terminal" | "ae2:pattern_encoding_terminal" | "ae2:pattern_access_terminal"))
}

/// The level emitter's amount, as typed, to the server.
pub const NET_LEVEL: &str = "ae2:level";

fn send_level(game: &mut Game<'_>) {
    let text = CLIENT.with_borrow(|c| c.level_text.clone());
    // (An expression goes as its value; one not yet complete doesn't go.)
    // A fluid's is typed in buckets, and goes in mB.
    let unit = CLIENT.with_borrow(|c| if c.ghosts.first().is_some_and(|(id, _)| id.starts_with('~')) { 1000.0 } else { 1.0 });
    let Some(value) = (if text.trim().is_empty() { Some(0) } else { math_value(&text).and_then(|v| scaled_amount(v, unit)) }) else { return };
    let _ = game.native_net_send_to_server(NET_LEVEL, value.to_string().as_bytes());
}

/// `native_screen_key_pressed_pre`: while the search box has the keys, they are its (the inventory key types).
pub fn screen_key(menu: Option<&str>, key: &str, canceled: &mut bool) {
    if dialog_key(key) || item_screen_key(key) {
        *canceled = true;
        return;
    }
    if matches!(menu, Some("ae2:level_emitter" | "ae2:energy_level_emitter")) && CLIENT.with_borrow(|c| c.level_focus) {
        CLIENT.with_borrow_mut(|c| {
            if key.ends_with("backspace") {
                c.level_text.pop();
                c.level_dirty = true;
            } else if key.ends_with("escape") || key.ends_with("enter") {
                c.level_focus = false;
            }
        });
        *canceled = true;
        return;
    }
    if !is_terminal_menu(menu) || !CLIENT.with_borrow(|c| c.search_focus) {
        return;
    }
    CLIENT.with_borrow_mut(|c| {
        if key.ends_with("backspace") {
            c.search.pop();
            c.view.take();
        } else if key.ends_with("escape") || key.ends_with("enter") {
            c.search_focus = false;
        }
        c.scroll = 0;
    });
    // Escape leaves the box, not the screen; everything else is text.
    *canceled = true;
}

/// `native_screen_char_typed_pre`: a character into the search box.
pub fn screen_char(menu: Option<&str>, ch: char, canceled: &mut bool) {
    if dialog_char(ch) || item_screen_char(ch) {
        *canceled = true;
        return;
    }
    if matches!(menu, Some("ae2:level_emitter" | "ae2:energy_level_emitter")) && CLIENT.with_borrow(|c| c.level_focus) {
        CLIENT.with_borrow_mut(|c| {
            if (ch.is_ascii_digit() || "+-*/%^(). ".contains(ch)) && c.level_text.len() < 32 {
                c.level_text.push(ch);
                c.level_dirty = true;
            }
        });
        *canceled = true;
        return;
    }
    if !is_terminal_menu(menu) || !CLIENT.with_borrow(|c| c.search_focus) {
        return;
    }
    CLIENT.with_borrow_mut(|c| {
        if c.search.chars().count() < 40 {
            c.search.push(ch);
        }
        c.view.take();
        c.scroll = 0;
    });
    *canceled = true;
}

// ---------------------------------------------------------------------------------------------------------
// `/ae2debug` (operators): fill the nearest network with many item types, and time the storage paths, to see
// that millions of items and thousands of types cost the server and the terminals little.

/// `native_register_commands`.
pub fn register_commands(commands: &mut Vec<super::native_server::NativeCommand>) {
    commands.push(super::native_server::NativeCommand::op("ae2debug fill <types:integer> <amount:integer>", 2));
    commands.push(super::native_server::NativeCommand::op("ae2debug bench", 2));
    for syntax in ["ae2", "ae2 list", "ae2 reloadconfig", "ae2 chunklogger", "ae2 spatial info", "ae2 spatial tpback", "ae2 channelmode", "ae2 channelmode infinite", "ae2 channelmode default", "ae2 channelmode x2", "ae2 channelmode x3", "ae2 channelmode x4", "ae2 tickmonitor true", "ae2 tickmonitor false", "ae2 compass", "ae2 testmeteorites", "ae2 testmeteorites force", "ae2 setuptestworld"] {
        commands.push(super::native_server::NativeCommand::op(syntax, 4));
    }
    commands.push(super::native_server::NativeCommand::op("ae2debug synth <cells:integer> <types:integer>", 2));
}

/// `native_perform_command` (a LAN guest's `/ae2debug`).
pub fn perform_command(game: &mut Game<'_>, player: &super::native_server::NativeServerPlayer, command: &str, ran: &mut bool) {
    match command.split_whitespace().next() {
        Some("ae2debug") => {
            *ran = true;
            debug_command(game, player.uuid, player.position, command);
        }
        Some("ae2") => {
            *ran = true;
            let args: Vec<&str> = command.split_whitespace().skip(1).collect();
            ae2_run(game, Some(player.uuid), player.permission, player.position, &args);
        }
        _ => {}
    }
}

/// The owner's `/ae2debug`, typed on their client (single player's own commands stay on the client): to the
/// server, which runs it for them.
pub const NET_DEBUG: &str = "ae2:debug";

pub fn client_command(game: &mut Game<'_>, command: &str, ran: &mut bool) {
    if let Some(rest) = command.strip_prefix("ae2guide") {
        *ran = true;
        let msg = super::port_ae2_guide::command(game, rest);
        game.native_chat(&msg);
        return;
    }
    if command.split_whitespace().next() != Some("ae2debug") {
        return;
    }
    *ran = true;
    if let Err(e) = game.native_net_send_to_server(NET_DEBUG, command.as_bytes()) {
        game.native_chat(&format!("ae2debug: {e}"));
    }
}

fn debug_command(game: &mut Game<'_>, uuid: u128, position: [f64; 3], command: &str) {
    let mut words = command.split_whitespace();
    words.next();
    let say = |game: &mut Game<'_>, text: String| {
        game.native_tell(uuid, horizon_client::text::Text::legacy(&text));
    };
    let player_position = position;
    let arg = |w: Option<&str>| w.and_then(|v| v.parse::<i64>().ok());
    match words.next() {
        // A crafting pattern (oak logs into planks), for the test pipeline's crafting dialogs.
        Some("pattern") => {
            let log = AeKey::item("minecraft:oak_log", "", 0, 0);
            let planks = AeKey::item("minecraft:oak_planks", "", 0, 0);
            let mut pattern = ItemStack::new("ae2:crafting_pattern", 1);
            let mut p = Compound::new();
            p.insert(java("in"), pattern_entries(&[(log, 1)]));
            p.insert(java("out"), pattern_entries(&[(planks, 4)]));
            set_custom(&mut pattern, PATTERN_KEY, Tag::Compound(p));
            let _ = game.native_give_stack(uuid, pattern);
        }
        // `clearpatterns x y z`: a provider's patterns gone.
        Some("clearpatterns") => {
            let (Some(x), Some(y), Some(z)) = (arg(words.next()), arg(words.next()), arg(words.next())) else { return };
            game.native_with_mod_block_entity([x as i32, y as i32, z as i32], |e| {
                for slot in &mut e.items {
                    if slot.as_ref().is_some_and(|s| s.item.ends_with("_pattern")) {
                        *slot = None;
                    }
                }
            });
        }
        // `setpattern x y z crafting|processing in n out n`: a pattern into a provider.
        Some("setpattern") => {
            let (Some(x), Some(y), Some(z)) = (arg(words.next()), arg(words.next()), arg(words.next())) else { return };
            let processing = words.next() == Some("processing");
            let (Some(input), Some(a), Some(output), Some(b)) = (words.next().map(str::to_owned), arg(words.next()), words.next().map(str::to_owned), arg(words.next())) else { return };
            let mut pattern = ItemStack::new(if processing { "ae2:processing_pattern" } else { "ae2:crafting_pattern" }, 1);
            let mut p = Compound::new();
            // Several ingredients as `a+b`, `n` of each.
            let inputs: Vec<(AeKey, i64)> = input.split('+').map(|id| (AeKey::item(id, "", 0, 0), a)).collect();
            p.insert(java("in"), pattern_entries(&inputs));
            p.insert(java("out"), pattern_entries(&[(AeKey::item(&output, "", 0, 0), b)]));
            set_custom(&mut pattern, PATTERN_KEY, Tag::Compound(p));
            let done = game.native_with_mod_block_entity([x as i32, y as i32, z as i32], |e| {
                if let Some(slot) = e.items.iter_mut().find(|s| s.is_none()) {
                    *slot = Some(pattern.clone());
                }
            });
            say(game, format!("§7setpattern {}", done.is_some()));
        }
        // `speed x y z n`: acceleration cards in an assembler.
        Some("speed") => {
            let (Some(x), Some(y), Some(z), Some(n)) = (arg(words.next()), arg(words.next()), arg(words.next()), arg(words.next())) else { return };
            game.native_with_mod_block_entity([x as i32, y as i32, z as i32], |e| {
                for (i, slot) in e.items.iter_mut().enumerate().skip(1) {
                    *slot = ((i as i64) <= n).then(|| ItemStack::new("ae2:speed_card", 1));
                }
            });
        }
        // `craft id n`: an auto-crafting job on the nearest network; `eta`: its forecast now.
        Some("craft") => {
            let (Some(id), Some(n)) = (words.next().map(str::to_owned), arg(words.next())) else { return };
            let Some(grid) = nearest_grid(player_position) else { return };
            let key = key_of(game, &ItemStack::new(&id, 1));
            request_craft(game, grid, uuid, &key, n);
        }
        Some("eta") => {
            let Some(grid) = nearest_grid(player_position) else { return };
            let text = crafting_status(game, grid);
            let state: Vec<String> = SERVER.with_borrow(|s| s.cpus.values().map(|c| c.debug_state()).filter(|t| !t.is_empty()).collect());
            say(game, format!("ETA {} {}", text.replace('\n', " "), state.join(" ")));
        }
        // `nbt x y z`: a device's settings as saved (priority, mode, level, power, the ghosts), for the tests.
        Some("nbt") => {
            let p: Vec<i32> = words.by_ref().take(3).filter_map(|w| w.parse().ok()).collect();
            let &[x, y, z] = p.as_slice() else { return };
            let text = game
                .native_with_mod_block_entity([x, y, z], |e| {
                    let mut t: Vec<String> =
                        ["priority", "mode", "level", "power", "stonecut", "freq"].iter().map(|k| format!("{k}={}", nbt_i64(&e.nbt, k))).collect();
                    t.push(format!("ghosts={}", ghosts_of(e).iter().flatten().map(|(id, n)| format!("{id}x{n}")).collect::<Vec<_>>().join(",")));
                    t.push(format!("items={}", e.items.iter().flatten().map(|s| format!("{}x{}", s.item, s.count)).collect::<Vec<_>>().join(",")));
                    t.push(format!("config={}", e.items.first().cloned().flatten().map(|s| cell_config(&s).len()).unwrap_or(0)));
                    t.join(" ")
                })
                .unwrap_or_else(|| "no block entity".to_owned());
            say(game, format!("nbt {text}"));
        }
        // `meteor`: AE2's meteorite starts around the player (worked out, placed, or not there).
        Some("meteor") => {
            let at = [player_position[0].floor() as i32, player_position[1].floor() as i32, player_position[2].floor() as i32];
            let dim = game.dim.id().to_owned();
            for line in super::port_ae2_meteor::report(game, &dim, at) {
                say(game, line);
            }
        }
        // `put <item> <n>`: that many into the nearest network.
        Some("put") => {
            let (Some(id), Some(n)) = (words.next().map(str::to_owned), arg(words.next())) else { return };
            let Some(grid) = nearest_grid(player_position) else { return };
            let key = key_of(game, &ItemStack::new(&id, 1));
            let stored = with_storage(game, grid, None, |st| st.insert(&key, n, Actionable::Modulate));
            say(game, format!("§7Put {stored} {id}"));
        }
        Some("fill") => {
            let (Some(types), Some(amount)) = (arg(words.next()), arg(words.next())) else { return };
            let Some(grid) = nearest_grid(player_position) else {
                say(game, format!("§cNo AE2 network (you are at {:?})", player_position));
                return;
            };
            let ids: Vec<String> = game.res.items().order.iter().filter(|i| &***i != "minecraft:air").map(|i| i.to_string()).collect();
            let t = std::time::Instant::now();
            let mut stored = 0i64;
            let mut kinds = 0;
            with_storage(game, grid, None, |st| {
                for k in 0..types.max(0) as usize {
                    // Real items first, then the same items again with other data (distinct keys, as AE2's).
                    let id = &ids[k % ids.len()];
                    let round = k / ids.len();
                    let key = AeKey::item(id, &if round > 0 { format!("n{round}") } else { String::new() }, 0, 0);
                    let n = st.insert(&key, amount, Actionable::Modulate);
                    if n > 0 {
                        kinds += 1;
                    }
                    stored += n;
                }
            });
            say(game, format!("§aStored {stored} items of {kinds} types in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0));
        }
        Some("bench") => {
            let Some(grid) = nearest_grid(player_position) else {
                say(game, format!("§cNo AE2 network (you are at {:?})", player_position));
                return;
            };
            let t = std::time::Instant::now();
            let items = terminal_items(game, grid);
            let list = t.elapsed();
            let total: i64 = items.iter().map(|(_, n)| *n).sum();
            let t = std::time::Instant::now();
            for _ in 0..10 {
                with_storage(game, grid, None, |_| ());
            }
            let mount = t.elapsed() / 10;
            let cells = SERVER.with_borrow(|s| s.cells.len());
            say(game, format!(
                "§a{} types, {} items, {} cells: terminal list {:.2} ms, storage open/close {:.3} ms",
                items.len(),
                total,
                cells,
                list.as_secs_f64() * 1000.0,
                mount.as_secs_f64() * 1000.0
            ));
        }
        Some("synth") => {
            // A network of `cells` 256k cells held in memory only, `types` kinds spread over them, filled to the
            // brim: how long AE2's paths take at that size, with no world in the way.
            let (Some(cells), Some(types)) = (arg(words.next()), arg(words.next())) else { return };
            let cell = cell_type("ae2:item_storage_cell_256k").expect("256k cell");
            let mut st: NetworkStorage<Box<dyn super::port_ae2_grid::MeStorage>> = NetworkStorage::new();
            for c in 0..cells.max(1) as u64 {
                st.mount(c, 0, Box::new(BasicCellInventory::new(cell, &[], FuzzyMode::IgnoreAll, CellUpgrades::default(), &CellContents::default())));
            }
            let t = std::time::Instant::now();
            let mut stored = 0i64;
            // Each type an even share of what the cells hold (a 256k cell: 2 097 152 items less its types' cost).
            let each = (cells.max(1) * 2_000_000 / types.max(1)).max(1);
            for k in 0..types.max(1) {
                let key = AeKey::item("minecraft:diamond_sword", &format!("n{k}"), 0, 0);
                stored += st.insert(&key, each, Actionable::Modulate);
            }
            let fill = t.elapsed();
            let t = std::time::Instant::now();
            let contents = st.contents();
            let scan = t.elapsed();
            let t = std::time::Instant::now();
            let key = AeKey::item("minecraft:diamond_sword", &format!("n{}", types / 2), 0, 0);
            for _ in 0..1000 {
                let got = st.extract(&key, 64, Actionable::Modulate);
                st.insert(&key, got, Actionable::Modulate);
            }
            let move_1000 = t.elapsed();
            say(game, format!(
                "§a{} cells, {} types, {} items: fill {:.1} ms, full scan {:.2} ms, 1000 take+put {:.2} ms",
                cells,
                contents.size(),
                stored,
                fill.as_secs_f64() * 1000.0,
                scan.as_secs_f64() * 1000.0,
                move_1000.as_secs_f64() * 1000.0
            ));
        }
        _ => say(game, "§7ae2debug fill <types> <amount> | bench | synth <cells> <types>".into()),
    }
}

/// The grid with a block nearest `at`.
fn nearest_grid(at: [f64; 3]) -> Option<usize> {
    SERVER.with_borrow(|s| {
        s.grid_of
            .iter()
            .map(|(p, g)| {
                let d = (p[0] as f64 + 0.5 - at[0]).powi(2) + (p[1] as f64 + 0.5 - at[1]).powi(2) + (p[2] as f64 + 0.5 - at[2]).powi(2);
                (d, *g)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, g)| g)
    })
}

/// `native_block_signal`: a level emitter giving its signal (from the server's own list: no world read here).
pub fn block_signal(pos: [i32; 3], signal: &mut i32) {
    SERVER.with_borrow(|s| {
        if s.lit_emitters.contains(&pos) {
            *signal = 15;
        } else if let Some(level) = s.p2p_signal.get(&pos) {
            *signal = *level;
        } else if let Some(level) = s.light_signals.get(&pos) {
            *signal = *level;
        }
    });
}

/// `native_use_item_on`: AE2's quartz wrenches turn an AE2 block a quarter turn about the clicked face's axis
/// (`IOrientationStrategy`), its block entity and contents kept.
pub fn use_item_on(game: &mut Game<'_>, stack: &ItemStack, pos: Pos, face: Pos, claimed: &mut bool) {
    let _ = stack;
    let block = block_id(game, pos);
    if tool_click(game, pos, &block) {
        *claimed = true;
        return;
    }
    if !matches!(&*stack.item, "ae2:certus_quartz_wrench" | "ae2:nether_quartz_wrench") {
        return;
    }
    let block = block_id(game, pos);
    if !block.starts_with("ae2:") {
        return;
    }
    let Some(facing) = game.native_block_property(pos, "facing") else { return };
    *claimed = true;
    let names = ["down", "up", "north", "south", "west", "east"];
    let f = facing_dir(Some(facing.as_str()));
    // f × axis: a quarter turn; along the axis it doesn't move, so it faces the clicked side instead.
    let turned = [f[1] * face[2] - f[2] * face[1], f[2] * face[0] - f[0] * face[2], f[0] * face[1] - f[1] * face[0]];
    let to = if turned == [0, 0, 0] { face } else { turned };
    let Some(name) = names.iter().find(|n| facing_dir(Some(n)) == to) else { return };
    if game.native_set_block_property_with(pos, "facing", name, 3) {
        SERVER.with_borrow_mut(|s| s.flips.push(pos));
    }
}

/// Whether the acting player sneaks (Horizon swaps a guest's sneak in as Left Shift).
fn sneaking(game: &Game<'_>) -> bool {
    game.keys.contains(&winit::keyboard::KeyCode::ShiftLeft)
        || game.keys.contains(&winit::keyboard::KeyCode::ShiftRight)
        || game.player.as_ref().is_some_and(|p| p.pose == horizon_client::pose::Pose::Crouching)
}

/// AE2's handheld tools on a block; true when the tool took the click.
fn tool_use(game: &mut Game<'_>, tool: &str, pos: Pos, block: &str) -> bool {
    let Some(player) = game.native_acting_player() else { return false };
    let tell = |game: &mut Game<'_>, t: &str| {
        game.native_tell(player.uuid, horizon_client::text::Text::legacy(t));
    };
    match tool {
        // The network tool: the grid the block is in (AE2's network status screen, as chat).
        "ae2:network_tool" => {
            let Some(g) = SERVER.with_borrow(|s| s.grid_of.get(&pos).copied()) else {
                if block.starts_with("ae2:") {
                    tell(game, "§7Not part of a network");
                    return true;
                }
                return false;
            };
            open_network_status(game, player.uuid, g);
            true
        }
        // The memory card: sneak to copy a machine's settings, use to paste them on one of the same kind.
        "ae2:memory_card" => {
            if !block.starts_with("ae2:") {
                return false;
            }
            let keys = ["ghosts", "mode", "level"];
            if sneaking(game) {
                let copied = game
                    .native_with_mod_block_entity(pos, |e| {
                        let mut c = Compound::new();
                        for k in keys {
                            if let Some(v) = e.nbt.get(&java(k)) {
                                c.insert(java(k), v.clone());
                            }
                        }
                        c
                    })
                    .unwrap_or_default();
                if let Some(card) = game.native_carried_stack(game.selected as i32) {
                    let extra = card.extra.get_or_insert_with(Default::default);
                    extra.other.retain(|(k, _)| k != CELL_DATA);
                    let mut data = Compound::new();
                    data.insert(java("ae2:memory_block"), Tag::String(java(block)));
                    data.insert(java("ae2:memory"), Tag::Compound(copied));
                    extra.other.push((CELL_DATA.to_owned(), Tag::Compound(data)));
                    extra.other.sort_by(|a, b| a.0.cmp(&b.0));
                }
                tell(game, &format!("§7Settings copied from {}", item_words(block)));
            } else {
                let held = game.held().cloned();
                let saved = held.as_ref().and_then(|c| c.extra.as_ref()).and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA).map(|(_, t)| t.clone()));
                let Some(Tag::Compound(data)) = saved else {
                    tell(game, "§7Blank memory card: sneak and use it on a machine to copy its settings");
                    return true;
                };
                let from = match data.get(&java("ae2:memory_block")) {
                    Some(Tag::String(j)) => j.to_string().unwrap_or_default(),
                    _ => String::new(),
                };
                if from != block {
                    tell(game, &format!("§cThese settings are for {}", item_words(&from)));
                    return true;
                }
                if let Some(Tag::Compound(mem)) = data.get(&java("ae2:memory")) {
            // (P2P tunnels take the frequency instead: see `p2p_card`.)
                    let mem = mem.clone();
                    game.native_with_mod_block_entity(pos, |e| {
                        for (k, v) in mem.iter() {
                            e.nbt.insert(k.clone(), v.clone());
                        }
                    });
                }
                tell(game, &format!("§7Settings pasted on {}", item_words(block)));
            }
            true
        }
        // The entropy manipulator: heat (smelts the block, melts ice and snow, boils water) or, sneaking, cool
        // (water to ice, lava to obsidian or cobblestone, grass and mycelium to dirt).
        "ae2:entropy_manipulator" => {
            let cool = sneaking(game);
            // Water or lava between the player and the block clicked is what the manipulator points at.
            let eye = [player.position[0], player.position[1] + 1.62, player.position[2]];
            let target = [pos[0] as f64 + 0.5, pos[1] as f64 + 0.5, pos[2] as f64 + 0.5];
            let d = [target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let steps = (len * 10.0) as i32;
            for i in 1..steps {
                let t = f64::from(i) / f64::from(steps);
                let at = [(eye[0] + d[0] * t).floor() as i32, (eye[1] + d[1] * t).floor() as i32, (eye[2] + d[2] * t).floor() as i32];
                if at == pos {
                    break;
                }
                let here = block_id(game, at);
                if matches!(here.as_str(), "minecraft:water" | "minecraft:lava")
                    && let Some((out, drops)) = entropy_recipe(game, &here, cool)
                {
                    let dim = game.dim.id().to_owned();
                    if let Some(state) = game.pack.assets.registry.default_state(&out) {
                        game.native_set_block(&dim, at, state, 3);
                    }
                    for d in drops {
                        let _ = game.native_give_stack(player.uuid, ItemStack::new(&d, 1));
                    }
                    return true;
                }
            }
            if let Some((out, drops)) = entropy_recipe(game, block, cool) {
                let dim = game.dim.id().to_owned();
                if let Some(state) = game.pack.assets.registry.default_state(&out) {
                    game.native_set_block(&dim, pos, state, 3);
                }
                for d in drops {
                    let _ = game.native_give_stack(player.uuid, ItemStack::new(&d, 1));
                }
                return true;
            }
            let to: Option<String> = if cool {
                match block {
                    "minecraft:water" => Some("minecraft:ice".into()),
                    "minecraft:lava" => Some(if game.native_block_property(pos, "level").is_some_and(|l| l != "0") { "minecraft:cobblestone" } else { "minecraft:obsidian" }.into()),
                    "minecraft:grass_block" | "minecraft:mycelium" | "minecraft:podzol" => Some("minecraft:dirt".into()),
                    "minecraft:stone" => Some("minecraft:cobblestone".into()),
                    _ => None,
                }
            } else {
                match block {
                    "minecraft:ice" | "minecraft:packed_ice" | "minecraft:snow_block" => Some("minecraft:water".into()),
                    "minecraft:water" => Some("minecraft:air".into()),
                    "minecraft:obsidian" => Some("minecraft:lava".into()),
                    _ => game.res.recipes().cook("minecraft:smelting", &ItemStack::new(block, 1)).map(|r| r.result.item.to_string()).filter(|r| game.pack.assets.registry.default_state(r).is_some()),
                }
            };
            let Some(to) = to else { return false };
            let dim = game.dim.id().to_owned();
            if let Some(state) = game.pack.assets.registry.default_state(&to) {
                game.native_set_block(&dim, pos, state, 3);
            }
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------------------
// Fluids in the terminal (AE2's fluid keys): shown as their bucket, stored and taken a bucket at a time.

/// A key as the terminal's list names it: fluids marked with `~`.
fn wire_id(key: &AeKey) -> String {
    if key.key_type() == super::port_ae2_core::KeyType::Fluids { format!("~{}", key.id()) } else { key.id().to_owned() }
}

/// What a list entry is drawn as: a fluid as its bucket.
fn icon_of(id: &str) -> String {
    match id.strip_prefix('~') {
        Some(fluid) => fluid_bucket(fluid),
        None => id.to_owned(),
    }
}

fn fluid_bucket(fluid: &str) -> String {
    match fluid {
        "minecraft:water" => "minecraft:water_bucket".to_owned(),
        "minecraft:lava" => "minecraft:lava_bucket".to_owned(),
        f => format!("{f}_bucket"),
    }
}

/// The fluid a filled bucket holds (water, lava, a mod's fluid's bucket).
fn bucket_fluid(item: &str) -> Option<String> {
    match item {
        "minecraft:water_bucket" => Some("minecraft:water".to_owned()),
        "minecraft:lava_bucket" => Some("minecraft:lava".to_owned()),
        i if !i.starts_with("minecraft:") && i.ends_with("_bucket") => Some(i.trim_end_matches("_bucket").to_owned()),
        _ => None,
    }
}

/// A terminal click with a bucket on the cursor: a filled one stores its 1000 mB, an empty one takes a bucket of
/// the fluid clicked. True when it was a bucket click.
fn terminal_bucket(game: &mut Game<'_>, grid: usize, uuid: u128, index: usize, carried: &mut Option<ItemStack>) -> bool {
    let Some(stack) = carried.clone() else { return false };
    let swap = |carried: &mut Option<ItemStack>, game: &mut Game<'_>, to: &str| {
        if stack.count > 1 {
            if let Some(c) = carried.as_mut() {
                c.count -= 1;
            }
            let _ = game.native_give_stack(uuid, ItemStack::new(to, 1));
        } else {
            *carried = Some(ItemStack::new(to, 1));
        }
    };
    if let Some(fluid) = bucket_fluid(&stack.item) {
        let key = AeKey::fluid(&fluid, "");
        let fits = with_storage(game, grid, None, |st| st.insert(&key, 1000, Actionable::Simulate)) == 1000;
        if fits {
            with_storage(game, grid, None, |st| st.insert(&key, 1000, Actionable::Modulate));
            swap(carried, game, "minecraft:bucket");
        }
        return true;
    }
    if &*stack.item != "minecraft:bucket" {
        return false;
    }
    let entry = SERVER.with_borrow(|s| s.sent.get(&uuid).and_then(|v| v.keys.get(index).cloned()).filter(|(_, n)| *n > 0));
    let Some((key, _)) = entry.filter(|(k, _)| k.key_type() == super::port_ae2_core::KeyType::Fluids) else { return false };
    let have = with_storage(game, grid, None, |st| st.extract(&key, 1000, Actionable::Simulate));
    if have == 1000 {
        with_storage(game, grid, None, |st| st.extract(&key, 1000, Actionable::Modulate));
        swap(carried, game, &fluid_bucket(key.id()));
    }
    true
}

// ---------------------------------------------------------------------------------------------------------
// P2P tunnels (AE2's `P2PTunnelPart`): an input and its outputs share a frequency, set with a memory card;
// a tunnel is attuned to another kind by using an item of that kind on it.

/// The tunnel kind an item attunes to (AE2's `ae2:p2p_attunements/*` tags).
fn attunement(game: &Game<'_>, item: &str) -> Option<&'static str> {
    const KINDS: [&str; 6] = ["me_p2p_tunnel", "item_p2p_tunnel", "fluid_p2p_tunnel", "redstone_p2p_tunnel", "light_p2p_tunnel", "fe_p2p_tunnel"];
    let items = game.res.items();
    KINDS.into_iter().find(|k| items.item_in_tag(&format!("ae2:p2p_attunements/{k}"), item))
}

/// A tunnel turned into another kind, its facing, frequency and side kept.
fn convert_tunnel(game: &mut Game<'_>, pos: Pos, to: &str, freq: i64, output: bool) {
    let facing = game.native_block_property(pos, "facing");
    let dim = game.dim.id().to_owned();
    let Some(state) = game.pack.assets.registry.default_state(to) else { return };
    game.native_set_block(&dim, pos, state, 3);
    if let Some(f) = facing {
        game.native_set_block_property_with(pos, "facing", &f, 3);
    }
    SERVER.with_borrow_mut(|s| {
        s.p2p_pending.insert(pos, (freq, output));
    });
}

/// A click on a tunnel: attunement, or a memory card (sneak: this becomes an input and the card takes its
/// frequency; else this becomes an output of the card's frequency, and its kind). True when it took the click.
fn p2p_click(game: &mut Game<'_>, pos: Pos, block: &str, item: &str) -> bool {
    let Some(player) = game.native_acting_player() else { return false };
    let say = |game: &mut Game<'_>, t: &str| {
        game.native_action_bar(player.uuid, horizon_client::text::Text::legacy(t));
    };
    let (freq, output) = game.native_with_mod_block_entity(pos, |e| (nbt_i64(&e.nbt, "freq"), nbt_i64(&e.nbt, "output") != 0)).unwrap_or((0, false));
    if item == "ae2:memory_card" {
        if sneaking(game) {
            let freq = if freq != 0 {
                freq
            } else {
                (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as i64) & 0x7FFF_FFFF_FFFF) | 1
            };
            game.native_with_mod_block_entity(pos, |e| {
                set_nbt_i64(&mut e.nbt, "freq", freq);
                set_nbt_i64(&mut e.nbt, "output", 0);
            });
            if let Some(card) = game.native_carried_stack(game.selected as i32) {
                let extra = card.extra.get_or_insert_with(Default::default);
                extra.other.retain(|(k, _)| k != CELL_DATA);
                let mut data = Compound::new();
                data.insert(java("ae2:p2p_freq"), Tag::Long(freq));
                data.insert(java("ae2:p2p_kind"), Tag::String(java(block)));
                extra.other.push((CELL_DATA.to_owned(), Tag::Compound(data)));
                extra.other.sort_by(|a, b| a.0.cmp(&b.0));
            }
            say(game, &format!("§7P2P input, frequency {:04X}", freq & 0xFFFF));
            return true;
        }
        let card = game.held().and_then(|c| c.extra.as_ref()).and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA).map(|(_, t)| t.clone()));
        let Some(Tag::Compound(data)) = card else { return false };
        let Some(Tag::Long(f)) = data.get(&java("ae2:p2p_freq")).cloned() else { return false };
        let kind = match data.get(&java("ae2:p2p_kind")) {
            Some(Tag::String(j)) => j.to_string().unwrap_or_default(),
            _ => block.to_owned(),
        };
        if kind != block {
            convert_tunnel(game, pos, &kind, f, true);
        } else {
            game.native_with_mod_block_entity(pos, |e| {
                set_nbt_i64(&mut e.nbt, "freq", f);
                set_nbt_i64(&mut e.nbt, "output", 1);
            });
        }
        say(game, &format!("§7P2P output, frequency {:04X}", f & 0xFFFF));
        return true;
    }
    match attunement(game, item) {
        Some(kind) if format!("ae2:{kind}") != block => {
            convert_tunnel(game, pos, &format!("ae2:{kind}"), freq, output);
            say(game, &format!("§7Attuned: {}", item_words(kind)));
            true
        }
        _ => false,
    }
}

/// A tunnel input's work each tick: items out of its slot to the outputs' faced containers, a redstone signal
/// to the outputs.
fn p2p_input_tick(game: &mut Game<'_>, pos: Pos, kind: &str, freq: i64, tick: u64, entity: &mut ModBlockEntity) {
    let outputs: Vec<Pos> = SERVER.with_borrow(|s| {
        s.p2p.iter().filter(|(_, (f, out, k, seen))| *f == freq && *out && k == kind && tick.saturating_sub(*seen) <= 40).map(|(p, _)| *p).collect()
    });
    match kind {
        "ae2:item_p2p_tunnel" => {
            let Some(mut stack) = entity.items.first_mut().and_then(Option::take) else { return };
            for out in &outputs {
                let facing = game.native_block_property(*out, "facing");
                let target = add(*out, facing_dir(facing.as_deref()));
                match put_in_container(game, target, stack) {
                    Some(left) => stack = left,
                    None => return,
                }
            }
            if let Some(slot) = entity.items.first_mut() {
                *slot = Some(stack);
            }
        }
        "ae2:redstone_p2p_tunnel" => {
            let dim = game.dim.id().to_owned();
            let level = game.native_neighbor_signal(&dim, pos);
            SERVER.with_borrow_mut(|s| {
                for out in &outputs {
                    if s.p2p_signal.get(out).copied().unwrap_or(0) != level {
                        s.p2p_signal.insert(*out, level);
                        s.signal_updates.push(*out);
                    }
                }
            });
        }
        _ => {}
    }
}

/// Puts a stack into a vanilla container's slots (onto like stacks, then empty slots); what's left.
fn put_in_container(game: &mut Game<'_>, at: Pos, mut stack: ItemStack) -> Option<ItemStack> {
    let most = max_stack(game, &stack.item);
    let Some(be) = game.world.block_entities.get_mut(&(at[0], at[1], at[2])) else { return Some(stack) };
    let mut moved = false;
    for pass in 0..2 {
        for slot in be.slots_mut().iter_mut() {
            if stack.count == 0 {
                break;
            }
            match slot {
                Some(s) if pass == 0 && s.item == stack.item && s.extra == stack.extra && s.count < most => {
                    let n = (most - s.count).min(stack.count);
                    s.count += n;
                    stack.count -= n;
                    moved = true;
                }
                None if pass == 1 => {
                    let n = most.min(stack.count);
                    let mut put = stack.clone();
                    put.count = n;
                    *slot = Some(put);
                    stack.count -= n;
                    moved = true;
                }
                _ => {}
            }
        }
    }
    if moved {
        game.container_changed((at[0], at[1], at[2]));
    }
    (stack.count > 0).then_some(stack)
}

// ---------------------------------------------------------------------------------------------------------
// Planes (AE2's `AnnihilationPlanePart`, `FormationPlanePart`).

/// Every block's name (an item placeable as a block shares its name).
fn block_names(game: &Game<'_>) -> Arc<HashSet<String>> {
    thread_local! {
        static NAMES: RefCell<Option<Arc<HashSet<String>>>> = const { RefCell::new(None) };
    }
    if let Some(n) = NAMES.with_borrow(|n| n.clone()) {
        return n;
    }
    let reg = &game.pack.assets.registry;
    let names: HashSet<String> = (0..reg.len() as u32)
        .filter_map(|i| reg.state(horizon_world::registry::BlockStateId(i)).map(|s| s.name.clone()))
        .filter(|n| !n.ends_with(":air"))
        .collect();
    let names = Arc::new(names);
    NAMES.with_borrow_mut(|n| *n = Some(names.clone()));
    names
}

/// The formation plane as a storage: one block item at a time, into open air.
struct Formation {
    open: bool,
    placed: Option<AeKey>,
    blocks: Arc<HashSet<String>>,
    /// The plane's filter: AE2's config with its fuzzy and inverter cards.
    filter: BusFilter,
    /// Settings.PLACE_BLOCK off, and what it has thrown out so far (AEConfig.formationPlaneEntityLimit: not while
    /// 128 items lie there). What isn't a block is thrown out either way.
    drop_items: bool,
    can_throw: bool,
    thrown: Vec<(AeKey, i64)>,
}

impl super::port_ae2_grid::MeStorage for Formation {
    fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        if !self.open || amount < 1 || what.key_type() != super::port_ae2_core::KeyType::Items || !self.filter.allows(what) {
            return 0;
        }
        if self.drop_items || !self.blocks.contains(what.id()) {
            if !self.can_throw || !self.thrown.is_empty() {
                return 0;
            }
            let most = MAX_STACKS.with_borrow(|m| m.get(what.id()).copied().unwrap_or(64)).max(1);
            let n = amount.min(most);
            if mode == Actionable::Modulate {
                self.thrown.push((what.clone(), n));
            }
            return n;
        }
        if self.placed.is_some() {
            return 0;
        }
        if mode == Actionable::Modulate {
            self.placed = Some(what.clone());
        }
        1
    }
    fn extract(&mut self, _: &AeKey, _: i64, _: Actionable) -> i64 {
        0
    }
    fn available_stacks(&self, _: &mut super::port_ae2_core::KeyCounter) {}
    fn is_preferred_for(&self, _: &AeKey) -> bool {
        false
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The annihilation plane: items lying in front of it, and the block there (its drops), into the network when
/// it has room for all of them.
fn annihilate(game: &mut Game<'_>, grid: usize, pos: Pos) {
    let facing = game.native_block_property(pos, "facing");
    let t = add(pos, facing_dir(facing.as_deref()));
    let dim = game.dim.id().to_owned();
    let lo = [t[0] as f64, t[1] as f64, t[2] as f64];
    for item in game.native_item_entities(&dim, lo, [lo[0] + 1.0, lo[1] + 1.0, lo[2] + 1.0]) {
        let key = key_of(game, &item.stack);
        let stored = with_storage(game, grid, None, |st| st.insert(&key, i64::from(item.stack.count), Actionable::Modulate));
        if stored > 0 {
            let mut left = item.stack.clone();
            left.count -= stored as u32;
            game.native_set_item_entity(&dim, item.id, left);
        }
    }
    let name = block_id(game, t);
    // FluidPickupStrategy.tryPickup: a source block, a bucket's worth, when the network has room for it.
    if matches!(name.as_str(), "minecraft:water" | "minecraft:lava") {
        if game.native_block_property(t, "level").as_deref() == Some("0") {
            let key = AeKey::fluid(&name, "");
            if with_storage(game, grid, None, |st| st.insert(&key, 1000, Actionable::Simulate)) == 1000
                && let Some(air) = game.pack.assets.registry.default_state("minecraft:air")
            {
                with_storage(game, grid, None, |st| st.insert(&key, 1000, Actionable::Modulate));
                game.native_set_block(&dim, t, air, 3);
            }
        }
        return;
    }
    // ItemPickupStrategy.canHandleBlock: not air, not unbreakable, not what the tag lists (AE2's is empty).
    let unbreakable = game.res.facts().get(game.world.block(t[0], t[1], t[2])).hardness < 0.0;
    if unbreakable || matches!(name.as_str(), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air") {
        return;
    }
    let state = game.world.block(t[0], t[1], t[2]);
    let enchants: Vec<(String, i32)> = game
        .native_with_mod_block_entity(pos, |e| match e.nbt.get(&java("enchantments")) {
            Some(Tag::Compound(c)) => c.iter().filter_map(|(k, v)| Some((k.to_string().ok()?, match v { Tag::Int(n) => *n, _ => 1 }))).collect(),
            _ => Vec::new(),
        })
        .unwrap_or_default();
    let tool = (!enchants.is_empty()).then(|| {
        let mut tool = ItemStack::new("minecraft:diamond_pickaxe", 1);
        for (id, level) in &enchants {
            tool.enchant(id, *level as u32);
        }
        tool
    });
    let mut random = horizon_core::random::LegacyRandom::new(SERVER.with_borrow(|s| s.tick) as i64 ^ ((t[0] as i64) << 20) ^ t[2] as i64);
    let drops = {
        let items = game.res.items();
        match items.block_loot_table(&game.pack.assets.registry, state) {
            Some(table) => items.loot_table(&table, false, tool.as_ref(), &mut random),
            None => Vec::new(),
        }
    };
    // ItemPickupStrategy.calculateEnergyUsage: 1 + hardness + the items, times 8 a level of the other
    // enchantments and 0.85 a level of efficiency; unbreaking makes it free but one time in (level + 1).
    let level = |id: &str| enchants.iter().find(|(e, _)| e == id).map_or(0, |(_, l)| *l);
    let mut power = 1.0 + f64::from(game.res.facts().get(state).hardness) + drops.iter().map(|d| f64::from(d.count)).sum::<f64>();
    let mut use_power = true;
    if !enchants.is_empty() {
        let efficiency = level("minecraft:efficiency");
        let unbreaking = level("minecraft:unbreaking");
        if unbreaking > 0 {
            use horizon_core::random::RandomSource;
            use_power = random.next_int_bound(unbreaking + 1).unwrap_or(0) == 0;
        }
        let others: i32 = enchants.iter().map(|(_, l)| *l).sum::<i32>() - efficiency - unbreaking;
        power *= f64::from(if others > 0 { 8 * others } else { 1 }) * 0.85f64.powi(efficiency);
    }
    let keys: Vec<(AeKey, i64)> = drops.iter().map(|d| (key_of(game, d), i64::from(d.count))).collect();
    let fits = with_storage(game, grid, None, |st| keys.iter().all(|(k, n)| st.insert(k, *n, Actionable::Simulate) == *n));
    if !fits || (use_power && !take_energy(pos, power)) {
        return;
    }
    with_storage(game, grid, None, |st| {
        for (k, n) in &keys {
            st.insert(k, *n, Actionable::Modulate);
        }
    });
    // Removing a container spills what it held, as breaking it does.
    let held: Vec<ItemStack> = game.world.block_entities.get(&(t[0], t[1], t[2])).map_or(Vec::new(), |be| be.slots().iter().flatten().cloned().collect());
    if let Some(air) = game.pack.assets.registry.default_state("minecraft:air") {
        game.native_set_block(&dim, t, air, 3);
    }
    for stack in held {
        game.spawn_item(stack, [f64::from(t[0]) + 0.5, f64::from(t[1]) + 0.5, f64::from(t[2]) + 0.5], [0.0, 0.1, 0.0], 10);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Spatial IO (AE2's `SpatialIOPortBlockEntity`): the pylons' bounding box, contracted by one, swapped with what
// the spatial cell holds. Blocks with block entities stay where they are.

/// The volume the grid's pylons define, inclusive: None without a usable pylon frame.
fn spatial_region(game: &Game<'_>, grid: usize) -> Option<(Pos, Pos)> {
    let pylons: Vec<Pos> = SERVER.with_borrow(|s| s.grids.get(grid).map(|g| g.nodes.iter().copied().collect::<Vec<Pos>>()).unwrap_or_default())
        .into_iter()
        .filter(|p| block_id(game, *p) == "ae2:spatial_pylon")
        .collect();
    if pylons.len() < 2 {
        return None;
    }
    let mut lo = pylons[0];
    let mut hi = pylons[0];
    for p in &pylons {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    let inner = ([lo[0] + 1, lo[1] + 1, lo[2] + 1], [hi[0] - 1, hi[1] - 1, hi[2] - 1]);
    (0..3).all(|i| inner.1[i] >= inner.0[i]).then_some(inner)
}

fn state_string(game: &Game<'_>, at: Pos) -> Option<String> {
    let id = game.world.block(at[0], at[1], at[2]);
    let st = game.pack.assets.registry.state(id)?;
    let props: Vec<String> = st.properties.iter().map(|(k, v)| format!("{k}={v}")).collect();
    Some(format!("{}|{}", st.name, props.join(",")))
}

fn place_state(game: &mut Game<'_>, dim: &str, at: Pos, s: &str) {
    let (name, props) = s.split_once('|').unwrap_or((s, ""));
    let reg = &game.pack.assets.registry;
    let Some(mut st) = reg.default_state(name).and_then(|id| reg.state(id)).cloned() else { return };
    for kv in props.split(',').filter(|p| !p.is_empty()) {
        if let Some((k, v)) = kv.split_once('=')
            && let Some(slot) = st.properties.get_mut(k)
        {
            *slot = v.to_owned();
        }
    }
    let Some(id) = reg.id(&st) else { return };
    game.native_set_block(dim, at, id, 2);
}

/// A spatial cell's limit on each side.
fn spatial_limit(item: &str) -> Option<i32> {
    match item {
        "ae2:spatial_storage_cell_2" => Some(2),
        "ae2:spatial_storage_cell_16" => Some(16),
        "ae2:spatial_storage_cell_128" => Some(128),
        _ => None,
    }
}

/// The redstone pulse: the cell in the input swaps with the volume, then goes to the output.
/// SpatialPylonService.updateStatus: what a transition costs in AE, and how well the pylons cover the volume (0 to 1).
fn spatial_power(game: &Game<'_>, grid: usize, lo: Pos, hi: Pos) -> (f64, f64) {
    let pylons = SERVER.with_borrow(|s| s.grids.get(grid).map_or(Vec::new(), |g| g.nodes.iter().copied().collect::<Vec<_>>()))
        .into_iter()
        .filter(|p| block_id(game, *p) == "ae2:spatial_pylon")
        .count() as f64;
    let (x, y, z) = (f64::from(hi[0] - lo[0] + 1), f64::from(hi[1] - lo[1] + 1), f64::from(hi[2] - lo[2] + 1));
    let wanted = ((x * z + x * y + y * z) * 3.0 / 8.0).floor().max(6.0);
    let efficiency = (pylons / wanted).clamp(0.0, 1.0);
    ((x * y * z * 1250.0).powf(1.0 + 0.35 * (1.0 - efficiency)).floor(), efficiency)
}

fn spatial_swap(game: &mut Game<'_>, grid: usize, pos: Pos, entity: &mut ModBlockEntity) {
    let Some(mut cell) = entity.items.first().cloned().flatten() else { return };
    let Some(limit) = spatial_limit(&cell.item) else { return };
    if entity.items.get(1).is_some_and(Option::is_some) {
        return;
    }
    let Some((lo, hi)) = spatial_region(game, grid) else { return };
    let size = [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1];
    if size.iter().any(|s| *s > limit) {
        return;
    }
    // SpatialIOPortBlockEntity: it runs only with all the power it needs at hand.
    let (required, _) = spatial_power(game, grid, lo, hi);
    if !take_energy(pos, required) {
        return;
    }
    let data = match cell.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t.clone()) {
        Some(Tag::Compound(c)) => c,
        _ => Compound::new(),
    };
    // A used cell keeps its size (AE2: it can't be reformatted).
    if let Some(Tag::IntArray(s)) = data.get(&java("ae2:spatial_size"))
        && s.as_slice() != size.as_slice()
    {
        return;
    }
    let stored: Vec<String> = match data.get(&java("ae2:spatial_blocks")) {
        Some(Tag::List(l)) => l.iter().filter_map(|t| if let Tag::String(j) = t { j.to_string().ok() } else { None }).collect(),
        _ => Vec::new(),
    };
    let dim = game.dim.id().to_owned();
    let fixed = |game: &Game<'_>, at: Pos| game.world.block_entities.contains_key(&(at[0], at[1], at[2]));
    // What the volume holds now (leaving blocks with block entities), then the volume cleared.
    let mut captured = Vec::new();
    let air = game.pack.assets.registry.default_state("minecraft:air");
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                let at = [x, y, z];
                if fixed(game, at) {
                    continue;
                }
                let name = block_id(game, at);
                if name != "minecraft:air"
                    && let Some(s) = state_string(game, at)
                {
                    captured.push(format!("{},{},{}|{s}", x - lo[0], y - lo[1], z - lo[2]));
                }
                if let Some(air) = air {
                    game.native_set_block(&dim, at, air, 2);
                }
            }
        }
    }
    // The cell's blocks into the volume (one that would land on a kept block stays in the cell).
    for entry in stored {
        let Some((rel, state)) = entry.split_once('|') else { continue };
        let r: Vec<i32> = rel.split(',').filter_map(|v| v.parse().ok()).collect();
        if r.len() != 3 {
            continue;
        }
        let at = [lo[0] + r[0], lo[1] + r[1], lo[2] + r[2]];
        if fixed(game, at) {
            captured.push(entry.clone());
            continue;
        }
        place_state(game, &dim, at, state);
    }
    let mut data = data;
    data.insert(java("ae2:spatial_size"), Tag::IntArray(size.to_vec()));
    data.insert(java("ae2:spatial_blocks"), Tag::List(captured.iter().map(|s| Tag::String(java(s))).collect()));
    let extra = cell.extra.get_or_insert_with(Default::default);
    extra.other.retain(|(k, _)| k != CELL_DATA);
    extra.other.push((CELL_DATA.to_owned(), Tag::Compound(data)));
    extra.other.sort_by(|a, b| a.0.cmp(&b.0));
    entity.items[0] = None;
    entity.items[1] = Some(cell);
}

// ---------------------------------------------------------------------------------------------------------
// The entropy manipulator's AE2 recipes (`ae2:entropy`): (block it becomes, items it drops).

fn entropy_recipe(game: &Game<'_>, block: &str, cool: bool) -> Option<(String, Vec<String>)> {
    let mode = if cool { "cool" } else { "heat" };
    for r in game.native_recipes_of_type("ae2:entropy") {
        let j = &*r.json;
        if j.get("mode").and_then(|m| m.as_str()) != Some(mode) {
            continue;
        }
        let input = j.get("input");
        let id = input.and_then(|i| i.get("block").or_else(|| i.get("fluid"))).and_then(|b| b.get("id")).and_then(|v| v.as_str());
        if id != Some(block) {
            continue;
        }
        let out = j.get("output");
        let to = out.and_then(|o| o.get("block")).and_then(|b| b.get("id")).and_then(|v| v.as_str()).unwrap_or("minecraft:air").to_owned();
        let drops = out
            .and_then(|o| o.get("drops"))
            .and_then(|d| d.as_array())
            .map(|d| d.iter().filter_map(|x| x.get("item").and_then(|i| i.as_str()).map(str::to_owned)).collect())
            .unwrap_or_default();
        return Some((to, drops));
    }
    None
}

// ---------------------------------------------------------------------------------------------------------
// The colour applicator: paints the cable used on with its colour (sneak-use in the air picks the next colour).

const APPLICATOR_COLORS: [&str; 17] = [
    "fluix", "white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray", "cyan", "purple",
    "blue", "brown", "green", "red", "black",
];

/// ColorApplicatorItem.getColorFromItem: a paint item's AE colour (a snowball is fluix, "transparent").
fn paint_color(id: &str) -> Option<&'static str> {
    if id == "minecraft:snowball" {
        return Some("fluix");
    }
    let name = id.strip_prefix("ae2:").and_then(|n| n.strip_suffix("_paint_ball")).map(|n| n.trim_end_matches("_lumen"))
        .or_else(|| id.strip_prefix("minecraft:").and_then(|n| n.strip_suffix("_dye")))?;
    APPLICATOR_COLORS.iter().skip(1).find(|c| **c == name).copied()
}

/// Every item that has a colour: the paints above and anything in a `c:dyes/<colour>` tag.
fn paint_items(game: &Game<'_>) -> HashSet<String> {
    let items = game.res.items();
    let mut out: HashSet<String> = HashSet::from(["minecraft:snowball".to_owned()]);
    for c in APPLICATOR_COLORS.iter().skip(1) {
        out.insert(format!("minecraft:{c}_dye"));
        out.insert(format!("ae2:{c}_paint_ball"));
        out.insert(format!("ae2:{c}_lumen_paint_ball"));
        out.extend(items.item_tags.get(&format!("c:dyes/{c}")).into_iter().flatten().cloned());
    }
    out
}

/// The paint the applicator uses (ColorApplicatorItem.getColor): the one it was set to, while it still holds
/// some, else the first it holds.
fn applicator_paint(stack: &ItemStack) -> Option<String> {
    let contents = read_cell(stack);
    let held: Vec<String> = contents.keys.iter().zip(&contents.amounts).filter(|(_, n)| **n > 0).map(|(k, _)| k.id().to_string()).collect();
    let chosen = match stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) {
        Some(Tag::Compound(d)) => match d.get(&java("ae2:color_item")) {
            Some(Tag::String(j)) => j.to_string().ok(),
            _ => None,
        },
        _ => None,
    };
    chosen.filter(|c| held.contains(c)).or_else(|| held.into_iter().next())
}

fn applicator_color(stack: &ItemStack) -> Option<&'static str> {
    applicator_paint(stack).and_then(|p| paint_color(&p))
}

/// ColorApplicatorItem.cycleColors: the next paint it holds, by colour.
fn cycle_applicator(stack: &mut ItemStack) {
    let contents = read_cell(stack);
    let mut held: Vec<String> = contents.keys.iter().zip(&contents.amounts).filter(|(_, n)| **n > 0).map(|(k, _)| k.id().to_string()).collect();
    if held.is_empty() {
        return;
    }
    let order = |id: &String| paint_color(id).and_then(|c| APPLICATOR_COLORS.iter().position(|x| *x == c)).map_or(usize::MAX, |i| (i + 16) % 17);
    held.sort_by_key(order);
    let now = applicator_paint(stack);
    let at = now.and_then(|n| held.iter().position(|h| *h == n)).map_or(0, |i| (i + 1) % held.len());
    set_custom(stack, "ae2:color_item", Tag::String(java(&held[at])));
}

/// ColorApplicatorItem.consumeItem: one of the paint and 100 AE.
fn applicator_consume(game: &mut Game<'_>) {
    let creative = game.native_acting_player().is_some_and(|p| p.game_mode == 1);
    let Some(held) = game.native_carried_stack(game.selected as i32) else { return };
    let Some(paint) = applicator_paint(held) else { return };
    let mut contents = read_cell(held);
    if let Some(i) = contents.keys.iter().position(|k| k.id() == paint.as_str()) {
        contents.amounts[i] -= 1;
        contents.item_count -= 1;
        if contents.amounts[i] <= 0 {
            contents.keys.remove(i);
            contents.amounts.remove(i);
        }
    }
    write_cell(held, &contents);
    use_item_power(held, 100.0, creative);
}

/// BlockRecolorer's groups: the uncoloured block (None: always coloured) and the coloured ones' suffix.
const RECOLOR_GROUPS: [(Option<&str>, &str); 9] = [
    (Some("minecraft:glass"), "_stained_glass"),
    (Some("minecraft:glass_pane"), "_stained_glass_pane"),
    (Some("minecraft:white_wool"), "_wool"),
    (Some("minecraft:white_banner"), "_banner"),
    (Some("minecraft:white_wall_banner"), "_wall_banner"),
    (Some("minecraft:white_carpet"), "_carpet"),
    (Some("minecraft:terracotta"), "_terracotta"),
    (None, "_glazed_terracotta"),
    (None, "_concrete"),
];

/// BlockRecolorer.recolor: the block in `color` (fluix: the group's uncoloured one), or None.
fn recolor_block(block: &str, color: &str) -> Option<String> {
    for (plain, suffix) in RECOLOR_GROUPS {
        let path = block.strip_prefix("minecraft:")?;
        let colored = APPLICATOR_COLORS.iter().skip(1).any(|c| path.strip_prefix(c).and_then(|r| r.strip_prefix('_')).is_some_and(|r| format!("_{r}") == suffix));
        if plain == Some(block) || colored {
            if color == "fluix" {
                return plain.map(str::to_owned).filter(|p| p != block);
            }
            let to = format!("minecraft:{color}{suffix}");
            return (to != block).then_some(to);
        }
    }
    None
}

/// ColorApplicatorItem.useOn: recolour a cable or a vanilla block with what the applicator holds (more than 100 AE
/// needed). Sneaking with nothing to paint cycles the paint.
fn applicator_use(game: &mut Game<'_>, pos: Pos, block: &str) -> bool {
    let Some(held) = game.held().cloned() else { return false };
    let painted = match applicator_color(&held) {
        Some("fluix") if held_power(game) > 100.0 && SERVER.with_borrow(|s| s.last_click.values().any(|(p, _)| *p == pos)) && {
            let face = SERVER.with_borrow(|s| s.last_click.values().find(|(p, _)| *p == pos).map(|(_, f)| *f)).unwrap_or([0, 1, 0]);
            clean_splotches(game, add(pos, face), [-face[0], -face[1], -face[2]])
        } => true,
        Some(color) if held_power(game) > 100.0 => {
            if cable(block).is_some() {
                paint_cable(game, pos, block)
            } else if block == "ae2:crafting_monitor" {
                // CraftingMonitorBlockEntity.recolourBlock.
                let changed = game.native_with_mod_block_entity(pos, |e| {
                    let new = nbt_string(&e.nbt, "paint") != color;
                    e.nbt.insert(java("paint"), Tag::String(java(color)));
                    new
                });
                game.native_block_entity_sync(pos);
                changed.unwrap_or(false)
            } else if let Some(to) = recolor_block(block, color) {
                let dim = game.dim.id().to_owned();
                let old: Vec<(String, String)> = {
                    let id = game.world.block(pos[0], pos[1], pos[2]);
                    game.pack.assets.registry.state(id).map(|s| s.properties.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()).unwrap_or_default()
                };
                match game.pack.assets.registry.default_state(&to) {
                    Some(state) => {
                        game.native_set_block(&dim, pos, state, 3);
                        for (k, v) in old {
                            game.native_set_block_property_with(pos, &k, &v, 3);
                        }
                        true
                    }
                    None => false,
                }
            } else {
                false
            }
        }
        _ => false,
    };
    if painted {
        applicator_consume(game);
        return true;
    }
    if sneaking(game) {
        if let Some(h) = game.native_carried_stack(game.selected as i32) {
            cycle_applicator(h);
        }
        return true;
    }
    false
}

/// Paints a cable the applicator's colour: true when it changed.
fn paint_cable(game: &mut Game<'_>, pos: Pos, block: &str) -> bool {
    let Some(held) = game.held().cloned() else { return false };
    let Some(color) = applicator_color(&held) else { return false };
    let name = ae2_name(block).unwrap_or_default();
    let Some(old) = APPLICATOR_COLORS.iter().find(|c| name.starts_with(&format!("{c}_"))) else { return false };
    let to = format!("ae2:{color}_{}", &name[old.len() + 1..]);
    if to == format!("ae2:{name}") {
        return false;
    }
    // Under facades: the cable the twin holds takes the colour.
    if is_twin(&game.native_block(pos)) {
        let cable = to.trim_start_matches("ae2:").to_owned();
        game.native_with_mod_block_entity(pos, |e| e.nbt.insert(java("cable"), Tag::String(java(&cable))));
        game.native_block_entity_sync(pos);
        cable_sides(game, pos);
        for d in SIDES {
            cable_sides(game, add(pos, d));
        }
        return true;
    }
    let dim = game.dim.id().to_owned();
    match game.pack.assets.registry.default_state(&to) {
        Some(state) => {
            game.native_set_block(&dim, pos, state, 3);
            true
        }
        None => false,
    }
}

/// The held powered item's charge (a creative player's counts as endless).
fn held_power(game: &mut Game<'_>) -> f64 {
    if game.native_acting_player().is_some_and(|p| p.game_mode == 1) {
        return f64::INFINITY;
    }
    game.held().map_or(0.0, portable_power)
}

/// Takes `ae` from the held powered item (none from a creative player).
fn spend_held_power(game: &mut Game<'_>, ae: f64) {
    let creative = game.native_acting_player().is_some_and(|p| p.game_mode == 1);
    if let Some(held) = game.native_carried_stack(game.selected as i32) {
        use_item_power(held, ae, creative);
    }
}

/// `native_use_item` for the applicator (sneak: the next colour) and the cutting knife (an iron ingot into a
/// name press). True when it took the use.
pub fn tool_use_air(game: &mut Game<'_>, stack: &ItemStack) -> bool {
    let Some(player) = game.native_acting_player() else { return false };
    match &*stack.item {
        "ae2:matter_cannon" => fire_cannon(game, player.uuid, player.game_mode == 1),
        // GuideItem.use: the guide opens at its start page (on the render thread's next frame).
        "ae2:guide" => {
            super::port_ae2_guide::command(game, "open index.md");
            true
        }
        // Fluids can't be clicked: the manipulator finds the water or lava it points at itself (AE2 ray-traces fluids).
        "ae2:entropy_manipulator" => {
            if held_power(game) <= 1600.0 {
                return false;
            }
            let (yaw, pitch) = (f64::from(player.yaw).to_radians(), f64::from(player.pitch).to_radians());
            let dir = [-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()];
            let eye = [player.position[0], player.position[1] + 1.62, player.position[2]];
            for i in 1..=50 {
                let t = f64::from(i) * 0.1;
                let at = [(eye[0] + dir[0] * t).floor() as i32, (eye[1] + dir[1] * t).floor() as i32, (eye[2] + dir[2] * t).floor() as i32];
                let block = block_id(game, at);
                if matches!(block.as_str(), "minecraft:air" | "minecraft:cave_air") {
                    continue;
                }
                if !matches!(block.as_str(), "minecraft:water" | "minecraft:lava") {
                    return false;
                }
                let cool = sneaking(game);
                let Some((out, drops)) = entropy_recipe(game, &block, cool) else { return false };
                let dim = game.dim.id().to_owned();
                if let Some(state) = game.pack.assets.registry.default_state(&out) {
                    game.native_set_block(&dim, at, state, 3);
                }
                for d in drops {
                    let _ = game.native_give_stack(player.uuid, ItemStack::new(&d, 1));
                }
                spend_held_power(game, 1600.0);
                return true;
            }
            false
        }
        "ae2:color_applicator" if sneaking(game) => {
            if let Some(h) = game.native_carried_stack(game.selected as i32) {
                cycle_applicator(h);
            }
            true
        }
        _ => false,
    }
}

/// AE2's handheld tools used on a block (P2P attunement and memory cards, the colour applicator, the network tool,
/// the entropy manipulator, the wrenches). From the block's right click, and from the item's use when the player
/// sneaks (a sneaking click skips the block's own use). True when a tool took the click.
fn tool_click(game: &mut Game<'_>, pos: Pos, block: &str) -> bool {
    let block = block.to_owned();
    let held = game.held().map(|s| s.item.to_string());
    if block.ends_with("_p2p_tunnel") && let Some(item) = held.as_deref() && p2p_click(game, pos, &block, item) {
        return true;
    }
    if held.as_deref() == Some("ae2:color_applicator") && applicator_use(game, pos, &block) {
        return true;
    }
    // EntropyManipulatorItem: only with more than 1 600 AE, which each use takes.
    if held.as_deref() == Some("ae2:entropy_manipulator") && held_power(game) <= 1600.0 {
        return false;
    }
    if matches!(held.as_deref(), Some("ae2:network_tool" | "ae2:memory_card" | "ae2:entropy_manipulator"))
        && tool_use(game, held.as_deref().unwrap_or(""), pos, &block)
    {
        if held.as_deref() == Some("ae2:entropy_manipulator") {
            spend_held_power(game, 1600.0);
        }
        return true;
    }
    if matches!(held.as_deref(), Some("ae2:certus_quartz_wrench" | "ae2:nether_quartz_wrench")) && block.starts_with("ae2:") {
        if game.native_block_property(pos, "facing").is_some() {
            return wrench_turn(game, pos, &block);
        }
        if let Some(facing) = game.native_block_property(pos, "facing") {
            let names = ["north", "east", "south", "west", "up", "down"];
            let at = names.iter().position(|n| *n == facing).unwrap_or(0);
            for step in 1..names.len() {
                let next = names[(at + step) % names.len()];
                if game.native_set_block_property_with(pos, "facing", next, 3) {
                    SERVER.with_borrow_mut(|s| s.flips.push(pos));
                    break;
                }
            }
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------------------
// What AE2 says about a block in Jade (AE2's `appeng.integration.modules.jade` providers, `waila.ae2.*` texts),
// for the Jade AE2 Native mod: the server's view of the block, as legacy-coloured lines.

pub fn describe(game: &mut Game<'_>, pos: Pos) -> Vec<String> {
    let block = block_id(game, pos);
    let name = ae2_name(&block).unwrap_or("").to_owned();
    let mut out = Vec::new();
    let grid = SERVER.with_borrow(|s| s.grid_of.get(&pos).copied());
    let (online, conflict, too_many, active, channels, root) = SERVER.with_borrow(|s| match grid.and_then(|g| s.grids.get(g)) {
        Some(g) => (g.online, g.conflict, g.too_many, g.active.contains(&pos), g.cable_channels.get(&pos).copied().unwrap_or(0), g.nodes.iter().min().copied()),
        None => (false, false, false, false, 0, None),
    });
    let part = is_part(&block) || cable(&block).is_some();
    // CrystalResonanceGeneratorProvider.
    if name == "crystal_resonance_generator" && game.native_with_mod_block_entity(pos, |e| nbt_i64(&e.nbt, "suppressed")).unwrap_or(0) != 0 {
        out.push("§cSuppressed".to_owned());
    }
    // ChargerDataProvider: what it holds, and a powered item's charge.
    if name == "charger"
        && let Some(st) = game.native_with_mod_block_entity(pos, |e| e.items.first().cloned().flatten()).flatten()
    {
        out.push(format!("Contains: §f{}", item_words(&st.item)));
        if let Some((most, _)) = item_battery(&st.item) {
            out.push(format!("{}% charged", (portable_power(&st) * 100.0 / most).floor() as i64));
        }
    }
    // CraftingMonitorDataProvider: what its CPU makes.
    if name == "crafting_monitor" {
        let job = SERVER.with_borrow(|s| {
            let g = s.grids.get(grid?)?;
            let key = g.cpu_of.get(&pos)?;
            Some(s.cpus.get(key)?.job()?.final_output.what.id().to_owned())
        });
        if let Some(id) = job {
            out.push(format!("Crafting: {}", item_words(&id)));
        }
    }
    // Parts (PartTooltips): ChannelDataProvider (smart cables), StorageMonitorDataProvider, AnnihilationPlaneDataProvider,
    // GridNodeStateProvider, P2PStateDataProvider, PatternProviderDataProvider.
    if part {
        if let Some((_, dense)) = cable(&block)
            && name.contains("_smart_")
        {
            if conflict {
                out.push("§cError: Controller Conflict".to_owned());
            } else if too_many {
                out.push("§cError: Too Many Channels".to_owned());
            } else {
                // IUsedChannelProvider.getMaxChannelsInfo: -1 (just the count) in infinite mode.
                let used = if online { channels } else { 0 };
                match super::port_ae2_grid::channel_factor() {
                    0 => out.push(format!("{used} Channels")),
                    f => out.push(format!("{used} of {} Channels", if dense { 32 } else { 8 } * f)),
                }
            }
        }
        if matches!(name.as_str(), "storage_monitor" | "conversion_monitor") {
            let (item, locked) = game
                .native_with_mod_block_entity(pos, |e| (nbt_string(&e.nbt, "item"), nbt_i64(&e.nbt, "locked") != 0))
                .unwrap_or_default();
            if let Some(key) = key_from_string(&item) {
                out.push(format!("Showing: {}", item_words(&wire_id(&key))));
            }
            out.push(if locked { "Locked" } else { "Unlocked" }.to_owned());
        }
        if name == "annihilation_plane" {
            let enchants = game.native_with_mod_block_entity(pos, |e| match e.nbt.get(&java("enchantments")) {
                Some(Tag::Compound(c)) => c.iter().filter_map(|(k, v)| Some((k.to_string().ok()?, match v { Tag::Int(n) => *n, _ => 1 }))).collect::<Vec<_>>(),
                _ => Vec::new(),
            });
            let enchants = enchants.unwrap_or_default();
            if !enchants.is_empty() {
                out.push("Enchanted with:".to_owned());
                for (id, level) in enchants {
                    out.push(format!("§7{}", enchantment_words(game, &id, level)));
                }
            }
        }
    }
    // GridNodeStateDataProvider (grid-connected blocks) and GridNodeStateProvider (parts): grey.
    let grid_block = part || (is_network_block(&block) && !matches!(name.as_str(), "cell_workbench" | "condenser"));
    if grid_block {
        let powered = online || conflict;
        let needs_channel = node_kind(&block).requires_channel;
        let state = if grid.is_none() || !powered {
            "Device Offline"
        } else if needs_channel && !active {
            "Device Missing Channel"
        } else {
            "Device Online"
        };
        out.push(format!("§7{state}"));
    }
    // P2PStateDataProvider (powered tunnels): the link, the frequency in its colours, an ME tunnel's channels.
    if name.ends_with("_p2p_tunnel") && online {
        let (freq, output, custom) = game
            .native_with_mod_block_entity(pos, |e| (nbt_i64(&e.nbt, "freq"), nbt_i64(&e.nbt, "output") != 0, nbt_string(&e.nbt, "name")))
            .unwrap_or_default();
        let outputs = SERVER.with_borrow(|s| s.p2p.iter().filter(|(p, (f, o, k, _))| **p != pos && freq != 0 && *f == freq && *o && *k == block).count());
        let has_input = SERVER.with_borrow(|s| s.p2p.iter().any(|(p, (f, o, k, _))| *p != pos && freq != 0 && *f == freq && !*o && *k == block));
        if output && has_input {
            out.push("Linked (Output Side)".to_owned());
        } else if !output && outputs > 0 {
            out.push(if outputs <= 1 { "Linked (Input Side)".to_owned() } else { format!("Linked (Input Side) - {outputs} Outputs") });
        } else {
            out.push("Unlinked".to_owned());
        }
        let hex = p2p_colored_hex(freq as u16);
        if custom.is_empty() {
            out.push(format!("Frequency: {hex}"));
        } else {
            out.push(format!("Frequency: §f{custom} ({hex}§r)"));
        }
        // MEP2PTunnelPart.getExternalFacingNode: a linked tunnel's outer node.
        if name == "me_p2p_tunnel" && ((output && has_input) || (!output && outputs > 0)) {
            out.push(format!("Carried Channels: {channels}"));
        }
    }
    // PatternProviderDataProvider: why it's locked.
    if name == "pattern_provider" {
        let (reason, until) = game
            .native_with_mod_block_entity(pos, |e| (nbt_i64(&e.nbt, "lock_reason"), nbt_string(&e.nbt, "lock_until")))
            .unwrap_or_default();
        match reason {
            1 => out.push("§cWaiting for redstone pulse to unlock".to_owned()),
            2 => out.push("§cLocked by redstone signal".to_owned()),
            3 => out.push("§cLocked by lack of redstone signal".to_owned()),
            4 => {
                let (id, amount) = until.rsplit_once(' ').unwrap_or((&until, "0"));
                out.push(format!("§cWaiting for {} ({amount}) to unlock", item_words(id)));
            }
            _ => {}
        }
    }
    // PowerStorageDataProvider: a block's own store (energy cells their share of the network's).
    let most = match name.as_str() {
        "energy_cell" => 200_000.0,
        "dense_energy_cell" => 1_600_000.0,
        "creative_energy_cell" => 9.22337203685477E14,
        "controller" => 8_000.0,
        "charger" | "inscriber" => 1_600.0,
        "chest" => 500.0,
        _ => 0.0,
    };
    if most > 0.0 {
        let current = match name.as_str() {
            "creative_energy_cell" => most,
            "energy_cell" | "dense_energy_cell" | "controller" => SERVER.with_borrow(|s| {
                let Some(g) = grid.and_then(|g| s.grids.get(g)) else { return 0.0 };
                let total = root.and_then(|r| s.energy.get(&r).copied()).unwrap_or(0.0);
                total * most / grid_capacity(g, &s.devices).max(1.0)
            }),
            _ if online => most,
            _ => 0.0,
        };
        out.push(format!("Stored: {} / {}", format_power(current.min(most)), format_power(most)));
    }
    // AE2's spatial anchor: what it keeps loaded. Horizon can't force chunks to stay loaded, so it says so.
    if name == "spatial_anchor" {
        out.push(format!("§7Used power: §f{} AE/t", if active { "80" } else { "0" }));
        out.push("§7Loaded chunks: §f0 §8(Horizon has no chunk forcing)".to_owned());
    }
    out
}

/// Platform.formatPower: "200 kAE", "#.##" after dividing by a thousand while above it.
fn format_power(p: f64) -> String {
    const PREFIXES: [&str; 10] = ["k", "M", "G", "T", "P", "T", "P", "E", "Z", "Y"];
    let mut p = p;
    let mut level = "";
    let mut i = 0;
    while p > 1000.0 && i < PREFIXES.len() {
        p /= 1000.0;
        level = PREFIXES[i];
        i += 1;
    }
    let t = format!("{p:.2}");
    format!("{} {level}AE", t.trim_end_matches('0').trim_end_matches('.'))
}

/// P2PHelper.toColoredHexString, bold: each hex digit of the frequency in its AE colour's medium variant.
fn p2p_colored_hex(freq: u16) -> String {
    const MEDIUM: [u32; 16] = [
        14408667, 16428612, 12069560, 8563943, 16318282, 12320593, 16299479, 10526880, 13487565, 5352134, 10769358, 5327615,
        12031615, 6349614, 16711740, 5658198,
    ];
    let mut out = String::new();
    for i in 0..4 {
        // P2PHelper.getFrequencyNibble: the high nibble first.
        let nibble = ((freq >> (4 * (3 - i))) & 0xF) as usize;
        out.push_str(&format!("§#{:06X}§l{:X}", MEDIUM[nibble], nibble));
    }
    out
}

/// An enchantment and level as vanilla names it ("Efficiency III").
fn enchantment_words(game: &Game<'_>, id: &str, level: i32) -> String {
    let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
    let name = game.translate(&format!("enchantment.{ns}.{path}"), path);
    const ROMAN: [&str; 10] = ["I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X"];
    if level == 1 && matches!(path, "silk_touch" | "infinity" | "mending" | "aqua_affinity" | "channeling" | "flame" | "multishot") {
        return name;
    }
    format!("{name} {}", ROMAN.get((level - 1).max(0) as usize).copied().unwrap_or("?"))
}

/// Blocks with an info screen: P2P tunnels, planes, energy cells (AE2's words for them, from `describe`).
fn is_info_block(name: &str) -> bool {
    name.ends_with("_p2p_tunnel")
        || matches!(name, "annihilation_plane" | "energy_cell" | "dense_energy_cell" | "creative_energy_cell" | "energy_acceptor" | "spatial_anchor")
}

// ---------------------------------------------------------------------------------------------------------
// More of AE2's screens: the number dialogs (craft amount, priority, stock and processing amounts), the crafting
// plan (confirm and error), the terminal settings, and the item screens over the world (portable cells, the
// network tool's toolbox and network status, the cutting knife).

/// Client ↔ server: the dialogs' requests and the plans they get back, and the item screens.
pub const NET_UI: &str = "ae2:ui";

#[derive(Clone, PartialEq)]
enum DialogKind {
    /// AE2's CraftAmountScreen: how many of the terminal's item (its index in what was sent, and id) to craft.
    Craft(usize, String),
    /// AE2's CraftConfirmScreen over the plan for that many.
    Confirm(usize, String, i64),
    /// AE2's CraftErrorScreen.
    Error(usize, String, i64),
    /// AE2's PriorityScreen for the open block.
    Priority,
    /// AE2's SetStockAmountScreen for an interface's config cell.
    Stock(usize, String),
    /// AE2's SetProcessingPatternAmountScreen for the encoder's cell.
    Amount(usize, String),
}

#[derive(Clone)]
struct Dialog {
    kind: DialogKind,
    text: String,
    /// An error's missing items (`m`), with amounts.
    plan: Vec<(char, String, i64)>,
    /// CraftingPlanSummary's entries: item, available, missing, to craft.
    entries: Vec<(String, i64, i64, i64)>,
    bytes: i64,
    /// CraftingCPUCycler: the CPUs that could take the job (index, bytes, co-processors), and which the button says
    /// (-1 Automatic, or one of those by its place in the list).
    cpus: Vec<(usize, i64, i64)>,
    selected: i32,
    /// The plan can run (CraftingPlanSummary.isSimulation: not), the job has been asked for, and Shift was down on
    /// the amount screen's button (start as soon as the plan is ready), when the CPU list was last asked for.
    can_start: bool,
    starting: bool,
    auto_start: bool,
    asked_cpus: Option<std::time::Instant>,
    waiting: bool,
    error: String,
    scroll: usize,
}

impl Dialog {
    fn new(kind: DialogKind, text: String) -> Self {
        Dialog {
            kind,
            text,
            plan: Vec::new(),
            entries: Vec::new(),
            bytes: 0,
            cpus: Vec::new(),
            selected: -1,
            can_start: false,
            starting: false,
            auto_start: false,
            asked_cpus: None,
            waiting: false,
            error: String::new(),
            scroll: 0,
        }
    }

    fn size(&self) -> [f32; 2] {
        match self.kind {
            DialogKind::Confirm(..) => [238.0, 206.0],
            DialogKind::Error(..) => [219.0, 87.0],
            DialogKind::Priority => [176.0, 125.0],
            _ => [176.0, 107.0],
        }
    }

    /// What the number stands for in its key's amounts: a fluid's is typed in buckets, and counts in mB.
    fn unit(&self) -> f64 {
        match &self.kind {
            DialogKind::Craft(_, id) | DialogKind::Stock(_, id) | DialogKind::Amount(_, id) if id.starts_with('~') => 1000.0,
            _ => 1.0,
        }
    }

    /// The text's value in that key's amounts; None when it isn't a whole number of them.
    fn scaled(&self) -> Option<i64> {
        scaled_amount(math_value(&self.text)?, self.unit())
    }

    fn value(&self) -> i64 {
        self.scaled().unwrap_or(0)
    }

    /// NumberEntryWidget.validate: what's wrong with the text as typed.
    fn problem(&self) -> Option<String> {
        let least = if self.kind == DialogKind::Priority { i64::MIN / 2 } else { 1 };
        let fluid = self.unit() > 1.0;
        match math_value(&self.text) {
            None => Some("Please enter a number or a mathematical expression e.g. : 3*4".to_owned()),
            Some(_) if self.scaled().is_none() => Some(if fluid { "The smallest amount is 0.001 B (1 mB)".to_owned() } else { "Must be whole number".to_owned() }),
            Some(_) if self.value() < least => Some(if fluid { "Please enter at least 0.001 B (1 mB)".to_owned() } else { format!("Please enter a number greater than or equal to {least}") }),
            Some(_) => None,
        }
    }
}

fn dialog_origin(game: &Game<'_>, d: &Dialog) -> [f32; 2] {
    let (w, h) = game.gui_size();
    let [dw, dh] = d.size();
    [((w - dw) / 2.0).floor(), ((h - dh) / 2.0).floor()]
}

/// AE2's NumberEntryWidget: +1 +10 +100 +1000 over the box, the same less under it (x, width, step).
const STEPS: [(f32, f32, i64); 4] = [(20.0, 22.0, 1), (48.0, 28.0, 10), (82.0, 32.0, 100), (120.0, 38.0, 1000)];
const NEXT_BUTTON: [f32; 4] = [120.0, 51.0, 38.0, 20.0];
const BACK_BUTTON: [f32; 4] = [154.0, 0.0, 22.0, 14.0];
// (Position.resolve: `bottom` is the widget's top, that far up from the window's foot.)
const CONFIRM_CANCEL: [f32; 4] = [6.0, 181.0, 50.0, 20.0];
const CONFIRM_START: [f32; 4] = [162.0, 181.0, 50.0, 20.0];
const CONFIRM_CPU: [f32; 4] = [19.0, 138.0, 180.0, 20.0];
const CONFIRM_SCROLL: [f32; 4] = [218.0, 19.0, 12.0, 114.0];
const ERROR_BUTTONS: [[f32; 4]; 3] = [[5.0, 62.0, 50.0, 20.0], [84.0, 62.0, 50.0, 20.0], [164.0, 62.0, 50.0, 20.0]];

fn open_dialog(kind: DialogKind, text: String) {
    CLIENT.with_borrow_mut(|c| c.dialog = Some(Dialog::new(kind, text)));
}

pub fn dialog_open() -> bool {
    CLIENT.with_borrow(|c| c.dialog.is_some())
}

/// The dialog's number confirmed (its button or Enter): the plan asked for, or the amount saved.
fn dialog_confirm(c: &mut Client) {
    let Some(d) = c.dialog.as_mut() else { return };
    if !matches!(d.kind, DialogKind::Confirm(..) | DialogKind::Error(..)) && d.problem().is_some() {
        return;
    }
    let v = d.value();
    match d.kind.clone() {
        DialogKind::Craft(index, id) => {
            if v < 1 {
                return;
            }
            c.ui_out.push(format!("plan {index} {v}"));
            if settings(c)[3]
                && let Some((id, _)) = c.items.get(index).cloned()
                && !c.pinned.contains(&id)
            {
                // PinnedKeys: the first row's nine, the oldest giving way.
                if c.pinned.len() >= TERM_COLS {
                    c.pinned.remove(0);
                }
                c.pinned.push(id);
                c.view.take();
            }
            let mut next = Dialog::new(DialogKind::Confirm(index, id, v), v.to_string());
            next.waiting = true;
            next.auto_start = c.shift;
            c.dialog = Some(next);
        }
        DialogKind::Stock(cell, _) => {
            c.ui_out.push(format!("set stock {cell} {}", v.max(1)));
            c.dialog = None;
        }
        DialogKind::Amount(cell, _) => {
            c.ui_out.push(format!("set amount {cell} {}", v.max(1)));
            c.dialog = None;
        }
        DialogKind::Priority => c.dialog = None,
        DialogKind::Confirm(index, _, amount) if d.can_start && !d.waiting && !d.starting && !d.cpus.is_empty() => {
            d.starting = true;
            let cpu = d.cpus.get(d.selected.max(0) as usize).filter(|_| d.selected >= 0).map_or("auto".to_owned(), |k| k.0.to_string());
            c.ui_out.push(format!("craft {index} {amount} {cpu}"));
        }
        _ => {}
    }
}

/// CraftingStatusScreen's window: its corner (the CPU list hangs off its left side).
fn status_origin(game: &Game<'_>) -> [f32; 2] {
    let (w, h) = game.gui_size();
    [((w - 238.0) / 2.0).floor(), ((h - 184.0) / 2.0).floor()]
}

// screens/crafting_cpu.json and crafting_status.json.
const STATUS_CANCEL: [f32; 4] = [163.0, 159.0, 50.0, 20.0];
const STATUS_BACK: [f32; 4] = [213.0, -4.0, 25.0, 22.0];
const STATUS_SCROLL: [f32; 4] = [218.0, 19.0, 12.0, 137.0];
/// The left toolbar's one button (VerticalButtonBar: two pixels in from its place, then its own margin).
const STATUS_MODE: [f32; 4] = [-20.0, 8.0, 16.0, 16.0];
const STATUS_ROWS: usize = 6;
const STATUS_COLS: usize = 3;
const CPU_LIST: [f32; 4] = [-94.0, 4.0, 94.0, 164.0];
const CPU_LIST_SCROLL: [f32; 4] = [-16.0, 23.0, 12.0, 137.0];
const CPU_BUTTON: [f32; 2] = [67.0, 22.0];
const CPU_ROWS: usize = 6;
/// palette.json's SELECTION_COLOR, DEFAULT_TEXT_COLOR.
const SELECTION: u32 = 0x00D5FF;
const TEXT: u32 = 0x404040;
/// The table's fills: AEColor.GREEN and YELLOW's blackVariant at 0x5A alpha; a missing item's overlay.
const CELL_ACTIVE: u32 = 0x5A45A021;
const CELL_PENDING: u32 = 0x5AFFF7AA;
const CELL_MISSING: u32 = 0x1AFF0000;

/// MathExpressionParser: + - * / % ^ and brackets over numbers. None for anything else.
fn math_value(text: &str) -> Option<f64> {
    struct P<'a>(std::iter::Peekable<std::str::Chars<'a>>);
    impl P<'_> {
        fn eat(&mut self, ch: char) -> bool {
            while self.0.peek() == Some(&' ') {
                self.0.next();
            }
            if self.0.peek() == Some(&ch) {
                self.0.next();
                return true;
            }
            false
        }
        fn sum(&mut self) -> Option<f64> {
            let mut v = self.product()?;
            loop {
                if self.eat('+') {
                    v += self.product()?;
                } else if self.eat('-') {
                    v -= self.product()?;
                } else {
                    return Some(v);
                }
            }
        }
        fn product(&mut self) -> Option<f64> {
            let mut v = self.power()?;
            loop {
                if self.eat('*') {
                    v *= self.power()?;
                } else if self.eat('/') {
                    let d = self.power()?;
                    if d == 0.0 {
                        return None;
                    }
                    v /= d;
                } else if self.eat('%') {
                    let d = self.power()?;
                    if d == 0.0 {
                        return None;
                    }
                    v %= d;
                } else {
                    return Some(v);
                }
            }
        }
        fn power(&mut self) -> Option<f64> {
            let base = self.unary()?;
            if self.eat('^') { Some(base.powf(self.power()?)) } else { Some(base) }
        }
        fn unary(&mut self) -> Option<f64> {
            if self.eat('-') {
                return Some(-self.unary()?);
            }
            if self.eat('+') {
                return self.unary();
            }
            if self.eat('(') {
                let v = self.sum()?;
                return self.eat(')').then_some(v);
            }
            while self.0.peek() == Some(&' ') {
                self.0.next();
            }
            let mut number = String::new();
            while let Some(c) = self.0.peek().copied().filter(|c| c.is_ascii_digit() || *c == '.') {
                number.push(c);
                self.0.next();
            }
            number.parse().ok()
        }
    }
    let mut p = P(text.chars().peekable());
    let v = p.sum()?;
    while p.0.peek() == Some(&' ') {
        p.0.next();
    }
    (p.0.peek().is_none() && v.is_finite()).then_some(v)
}

fn math_whole(text: &str) -> Option<i64> {
    math_value(text).filter(|v| v.fract() == 0.0).map(|v| v as i64)
}

/// `value` units as whole amounts (0.01 buckets is 10 mB, however the float falls).
fn scaled_amount(value: f64, unit: f64) -> Option<i64> {
    let v = value * unit;
    ((v - v.round()).abs() < 1e-6).then(|| v.round() as i64)
}

/// An amount as its number field shows it: a fluid's in buckets.
fn amount_text(id: &str, n: i64) -> String {
    if id.starts_with('~') {
        let text = format!("{:.3}", n as f64 / 1000.0);
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        n.to_string()
    }
}

/// NumberFormat.getInstance: "1,024".
fn group_digits(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// A key's amount as AE2 words it: `format` is AmountFormat.SLOT in a cell, FULL in its tooltip.
fn key_amount(id: &str, n: i64, format: super::port_ae2_core::AmountFormat) -> String {
    if id.starts_with('~') {
        return match format {
            super::port_ae2_core::AmountFormat::Full => format!("{} B ({n} mB)", amount_text(id, n)),
            _ if n < 1000 => format!("{n}mB"),
            _ => format!("{}B", readable_number::format_f64(n as f64 / 1000.0, 3)),
        };
    }
    super::port_ae2_core::KeyType::Items.format_amount(n, format)
}

fn key_words(game: &Game<'_>, id: &str) -> String {
    match id.strip_prefix('~') {
        Some(fluid) => item_words(fluid),
        None => game.pack.names.display_item(id).to_string(),
    }
}

/// Part of a GUI sprite times a colour (Blitter.colorRgb).
fn tinted_sprite(game: &Game<'_>, batch: &mut HudBatch, name: &str, src: [f32; 4], dst: [f32; 4], rgb: u32) {
    let Some(mut r) = game.gui.atlas.sprites.get(name).copied() else { return };
    let (du, dv) = ((r.u1 - r.u0) / r.w as f32, (r.v1 - r.v0) / r.h as f32);
    r.u0 += src[0] * du;
    r.v0 += src[1] * dv;
    r.u1 = r.u0 + src[2] * du;
    r.v1 = r.v0 + src[3] * dv;
    let [_, red, green, blue] = rgb.to_be_bytes();
    batch.rect_uv(dst[0], dst[1], dst[2], dst[3], r, horizon_client::hud::rgba(red, green, blue, 0xFF));
}

/// One cell of AbstractTableRenderer: its item, its lines, what its tooltip adds, and its fills.
struct TableCell {
    id: String,
    lines: Vec<String>,
    full: Vec<String>,
    background: u32,
    overlay: u32,
}

/// AbstractTableRenderer.render: three columns from (9, 19), each cell 67 by 22 with a line between, half-size text
/// right-aligned against the item. The cell under the pointer, if any.
fn draw_table(game: &Game<'_>, batch: &mut HudBatch, origin: [f32; 2], rows: usize, cells: &[TableCell], scroll: usize, mouse: [f32; 2]) -> Option<usize> {
    let mut hovered = None;
    for (n, cell) in cells.iter().enumerate().skip(scroll * STATUS_COLS).take(rows * STATUS_COLS) {
        let at = n - scroll * STATUS_COLS;
        let (x, y) = (origin[0] + 9.0 + (at % STATUS_COLS) as f32 * 68.0, origin[1] + 19.0 + (at / STATUS_COLS) as f32 * 23.0);
        if cell.background != 0 {
            game.native_fill(batch, [x, y, 67.0, 22.0], cell.background);
        }
        let count = cell.lines.len() as f32;
        let height = count * 4.5 + (count - 1.0).max(0.0);
        let mut text_y = (y + (22.0 - height) / 2.0).round();
        let item_x = x + 67.0 - 19.0;
        for line in &cell.lines {
            let width = game.native_text_width(line);
            let text_x = ((item_x - 2.0 - width * 0.5) * 2.0).floor() / 2.0;
            let start = batch.vertices.len();
            game.native_text(batch, line, [text_x, text_y], TEXT, false);
            batch.scale_since(start, text_x, text_y, 0.5, 0.5);
            text_y += 5.5;
        }
        draw_key(game, batch, &cell.id, [item_x, y + 3.0], 16.0);
        if cell.overlay != 0 {
            game.native_fill(batch, [x, y, 67.0, 22.0], cell.overlay);
        }
        if mouse[0] >= x && mouse[0] <= x + 67.0 && mouse[1] >= y && mouse[1] <= y + 22.0 {
            hovered = Some(n);
        }
    }
    hovered
}

fn table_tooltip(game: &Game<'_>, batch: &mut HudBatch, cell: &TableCell, mouse: [f32; 2]) {
    let mut lines = vec![(horizon_client::text::Text::legacy(&key_words(game, &cell.id)), u32::MAX)];
    lines.extend(cell.full.iter().map(|l| (horizon_client::text::Text::legacy(l), u32::MAX)));
    game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
}

/// Tooltips.ofBytes.
fn bytes_words(bytes: i64) -> String {
    let mut value = bytes as f64;
    let mut units = ["B", "kB", "MB", "GB", "TB"].iter();
    let mut unit = units.next().copied().unwrap_or("B");
    while value >= 1024.0 {
        let Some(next) = units.next() else { break };
        value /= 1024.0;
        unit = next;
    }
    let text = format!("{value:.2}");
    format!("{} {unit}", text.trim_end_matches('0').trim_end_matches('.'))
}

/// Tooltips.ofDuration.
fn duration_words(seconds: i64) -> String {
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h "));
    }
    if m > 0 {
        out.push_str(&format!("{m}m "));
    }
    if s > 0 || out.is_empty() {
        out.push_str(&format!("{s}s"));
    }
    out.trim_end().to_owned()
}

/// CPUSelectionList.drawBackgroundLayer's InfoBar: a row of icons, text and items at 0.6 scale.
struct InfoBar {
    x: f32,
    y: f32,
}

impl InfoBar {
    fn icon(&mut self, game: &Game<'_>, batch: &mut HudBatch, icon: [f32; 2]) {
        game.native_sprite(batch, "ae2:gui/states", [icon[0], icon[1], 16.0, 16.0], [self.x, self.y, 9.6, 9.6]);
        self.x += 10.0;
    }

    fn item(&mut self, game: &Game<'_>, batch: &mut HudBatch, id: &str) {
        draw_key(game, batch, id, [self.x, self.y], 9.6);
        self.x += 10.0;
    }

    fn text(&mut self, game: &Game<'_>, batch: &mut HudBatch, text: &str) {
        let start = batch.vertices.len();
        game.native_text(batch, text, [self.x, self.y + 3.0], TEXT, false);
        batch.scale_since(start, self.x, self.y + 3.0, 0.6, 0.6);
        self.x += (game.native_text_width(text) * 0.6).round();
    }
}

/// CraftingStatusMenu's choice while none is made: the first CPU with a job, or else the first.
fn status_selected(c: &mut Client) -> usize {
    if c.status_cpu >= c.status_cpus.len() && !c.status_cpus.is_empty() {
        c.status_cpu = c.status_cpus.iter().position(|k| k.item != "-").unwrap_or(0);
    }
    c.status_cpu
}

/// The selected CPU's entries in CraftingStatusEntry's order: most yet to come, then most stored.
fn status_cells(c: &Client, selected: usize) -> Vec<TableCell> {
    use super::port_ae2_core::AmountFormat::{Full, Slot};
    let mut entries: Vec<&(usize, String, i64, i64, i64)> = c.status_entries.iter().filter(|e| e.0 == selected).collect();
    entries.sort_by_key(|e| std::cmp::Reverse((e.3 + e.4, e.2)));
    entries
        .into_iter()
        .map(|(_, id, stored, active, pending)| {
            let mut lines = Vec::new();
            let mut full = Vec::new();
            for (n, word) in [(*stored, "Stored"), (*active, "Crafting"), (*pending, "Scheduled")] {
                if n > 0 {
                    lines.push(format!("{word}: {}", key_amount(id, n, Slot)));
                    full.push(format!("{word}: {}", key_amount(id, n, Full)));
                }
            }
            let background = if *active > 0 {
                CELL_ACTIVE
            } else if *pending > 0 {
                CELL_PENDING
            } else {
                0
            };
            TableCell { id: id.clone(), lines, full, background, overlay: 0 }
        })
        .collect()
}

/// CraftingStatusScreen (from a terminal: the CPUs down the side, a tab back) and CraftingCPUScreen (from a crafting
/// block: its CPU alone, and its auto-selection mode).
fn status_draw(game: &Game<'_>, batch: &mut HudBatch, mouse: [f32; 2]) {
    if !CLIENT.with_borrow(|c| c.show_jobs) {
        return;
    }
    let (cpus, cells, sel, scroll, list_scroll, block_mode, host, drag) = CLIENT.with_borrow_mut(|c| {
        let sel = status_selected(c);
        c.status_mouse = mouse;
        c.gui_size_seen = game.gui_size();
        (c.status_cpus.clone(), status_cells(c, sel), sel, c.status_scroll, c.status_list_scroll, c.status_block, c.status_host.clone(), c.scroll_drag)
    });
    if game.native_mod_screen().is_some() {
        let (w, h) = game.gui_size();
        game.native_fill(batch, [0.0, 0.0, w, h], 0xA0000000);
    }
    let [ox, oy] = status_origin(game);
    let on = |r: [f32; 4]| mouse[0] >= ox + r[0] && mouse[0] < ox + r[0] + r[2] && mouse[1] >= oy + r[1] && mouse[1] < oy + r[1] + r[3];
    let mut tip: Option<Vec<(horizon_client::text::Text, u32)>> = None;
    let legacy = |l: &str| (horizon_client::text::Text::legacy(l), u32::MAX);
    if !block_mode {
        // The CPU list, behind the window's left edge (CPUSelectionList).
        let [lx, ly] = [ox + CPU_LIST[0], oy + CPU_LIST[1]];
        game.native_sprite(batch, "ae2:gui/cpu_selector", [0.0, 0.0, 94.0, 164.0], [lx, ly, 94.0, 164.0]);
        game.native_text(batch, "Crafting CPUs", [ox - 86.0, oy + 12.0], TEXT, false);
        let most = cpus.len().saturating_sub(CPU_ROWS);
        let top = if drag == 2 { scrollbar_row(mouse[1], oy + CPU_LIST_SCROLL[1], CPU_LIST_SCROLL[3], most) } else { list_scroll.min(most) };
        if drag == 2 {
            CLIENT.with_borrow_mut(|c| c.status_list_scroll = top);
        }
        for (i, cpu) in cpus.iter().enumerate().skip(top).take(CPU_ROWS) {
            let (x, y) = (lx + 9.0, ly + 19.0 + (i - top) as f32 * (CPU_BUTTON[1] + 1.0));
            let selected = i == sel;
            tinted_sprite(game, batch, "ae2:gui/cpu_selector", [100.0, 0.0, CPU_BUTTON[0], CPU_BUTTON[1]], [x, y, CPU_BUTTON[0], CPU_BUTTON[1]], if selected { SELECTION } else { 0xFFFFFF });
            let name = format!("CPU #{}", i + 1);
            let start = batch.vertices.len();
            game.native_text(batch, &name, [x + 3.0, y + 3.0], TEXT, false);
            batch.scale_since(start, x + 3.0, y + 3.0, 0.8, 0.8);
            let mut bar = InfoBar { x: x + 2.0, y: y + CPU_BUTTON[1] - 12.0 };
            if cpu.item == "-" {
                bar.icon(game, batch, [64.0, 64.0]);
                bar.x += 1.0;
                bar.text(game, batch, &format!("{}k", cpu.bytes / 1024));
                bar.x += 1.0;
                if cpu.accel > 0 {
                    bar.icon(game, batch, [64.0, 16.0]);
                    bar.text(game, batch, &cpu.accel.to_string());
                    bar.x += 1.0;
                }
                match cpu.mode {
                    1 => bar.item(game, batch, "ae2:terminal"),
                    2 => bar.item(game, batch, "ae2:export_bus"),
                    _ => {}
                }
            } else {
                bar.icon(game, batch, [48.0, 144.0]);
                bar.x += 2.0;
                bar.text(game, batch, &key_amount(&cpu.item, cpu.amount, super::port_ae2_core::AmountFormat::Slot));
                bar.x += 1.0;
                bar.item(game, batch, &cpu.item);
                let progress = (cpu.permille as f32 / 1000.0 * (CPU_BUTTON[0] - 1.0)).floor();
                if progress > 1.0 {
                    let color = if selected { 0xFFFFFFFF } else { 0xFF000000 | SELECTION };
                    game.native_fill(batch, [x + 1.0, y + CPU_BUTTON[1] - 2.0, progress - 1.0, 1.0], color);
                }
            }
            if mouse[0] >= x && mouse[0] < x + CPU_BUTTON[0] && mouse[1] >= y && mouse[1] < y + CPU_BUTTON[1] {
                let mut lines = vec![legacy(&name), legacy(&format!("§7{} Storage", bytes_words(cpu.bytes)))];
                match cpu.accel {
                    0 => {}
                    1 => lines.push(legacy("§71 Co-Processor")),
                    n => lines.push(legacy(&format!("§7{n} Co-Processors"))),
                }
                match cpu.mode {
                    1 => lines.push(legacy("Only for requests by players")),
                    2 => lines.push(legacy("Only for requests by automation")),
                    _ => {}
                }
                if cpu.item != "-" {
                    lines.push(legacy(&format!("Crafting {} {}", key_amount(&cpu.item, cpu.amount, super::port_ae2_core::AmountFormat::Full), key_words(game, &cpu.item))));
                    lines.push(legacy(&format!("Crafted {}% in {}", cpu.permille / 10, duration_words(cpu.elapsed))));
                }
                tip = Some(lines);
            }
        }
        draw_scrollbar(game, batch, [ox + CPU_LIST_SCROLL[0], oy + CPU_LIST_SCROLL[1]], CPU_LIST_SCROLL[3], top, most);
    }
    game.native_sprite(batch, "ae2:gui/craftingcpu", [0.0, 0.0, 238.0, 184.0], [ox, oy, 238.0, 184.0]);
    // The title: "Crafting Status", the time left (GuiText.ETAFormat, HH:mm:ss), and CantStoreItems.
    let selected = cpus.get(sel);
    let mut title = "Crafting Status".to_owned();
    if let Some(cpu) = selected.filter(|k| k.item != "-" && k.secs >= 0) {
        title.push_str(&format!(" - {:02}:{:02}:{:02}", cpu.secs / 3600, cpu.secs % 3600 / 60, cpu.secs % 60));
    }
    game.native_text(batch, &title, [ox + 8.0, oy + 7.0], TEXT, false);
    if selected.is_some_and(|k| k.cant_store) {
        let at = ox + 8.0 + game.native_text_width(&title);
        game.native_text(batch, " - ", [at, oy + 7.0], TEXT, false);
        game.native_text(batch, "Can't Store Contents!", [at + game.native_text_width(" - "), oy + 7.0], 0xFF5555, false);
    }
    let rows = cells.len().div_ceil(STATUS_COLS);
    let most = rows.saturating_sub(STATUS_ROWS);
    let top = if drag == 1 { scrollbar_row(mouse[1], oy + STATUS_SCROLL[1], STATUS_SCROLL[3], most) } else { scroll.min(most) };
    if drag == 1 {
        CLIENT.with_borrow_mut(|c| c.status_scroll = top);
    }
    let hovered = draw_table(game, batch, [ox, oy], STATUS_ROWS, &cells, top, mouse);
    draw_scrollbar(game, batch, [ox + STATUS_SCROLL[0], oy + STATUS_SCROLL[1]], STATUS_SCROLL[3], top, most);
    let cancel = [ox + STATUS_CANCEL[0], oy + STATUS_CANCEL[1], STATUS_CANCEL[2], STATUS_CANCEL[3]];
    draw_button_at(game, batch, cancel, if cells.is_empty() { "§7Cancel" } else { "Cancel" }, !cells.is_empty() && on(STATUS_CANCEL));
    if block_mode {
        // SettingToggleButton on Settings.CPU_SELECTION_MODE.
        let mode = selected.map_or(0, |k| k.mode);
        let at = [ox + STATUS_MODE[0], oy + STATUS_MODE[1]];
        match mode {
            1 | 2 => {
                let [bx, by] = ICON_BUTTON_BACKGROUND;
                game.native_sprite(batch, "ae2:gui/states", [bx, by, 16.0, 16.0], [at[0], at[1], 16.0, 16.0]);
                draw_key(game, batch, if mode == 1 { "ae2:terminal" } else { "ae2:export_bus" }, at, 16.0);
            }
            _ => draw_state_button(game, batch, at, [48.0, 144.0]),
        }
        if on(STATUS_MODE) {
            let word = ["For requests by players or automation", "Only for requests by players", "Only for requests by automation"][mode.clamp(0, 2) as usize];
            tip = Some(vec![legacy("CPU Auto-Selection Mode"), legacy(&format!("§7{word}"))]);
        }
    } else {
        // AESubScreen.addBackButton: a corner tab with the terminal's icon.
        let at = [ox + STATUS_BACK[0], oy + STATUS_BACK[1]];
        game.native_sprite(batch, "ae2:gui/states", [128.0, 192.0, STATUS_BACK[2], STATUS_BACK[3]], [at[0], at[1], STATUS_BACK[2], STATUS_BACK[3]]);
        let icon = if host.is_empty() { "ae2:terminal" } else { host.as_str() };
        draw_key(game, batch, icon, [at[0] + 4.0, at[1] + 3.0], 16.0);
        if on(STATUS_BACK) {
            tip = Some(vec![legacy(&key_words(game, icon))]);
        }
    }
    if let Some(i) = hovered
        && let Some(cell) = cells.get(i)
    {
        table_tooltip(game, batch, cell, mouse);
    }
    if let Some(lines) = tip {
        game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
    }
}

/// A press while the status window is open: it takes them all.
fn status_click(game: &Game<'_>, mouse: [f32; 2], button: i32, canceled: &mut bool) -> bool {
    if !CLIENT.with_borrow(|c| c.show_jobs) {
        return false;
    }
    *canceled = true;
    let [ox, oy] = status_origin(game);
    let on = |r: [f32; 4]| mouse[0] >= ox + r[0] && mouse[0] < ox + r[0] + r[2] && mouse[1] >= oy + r[1] && mouse[1] < oy + r[1] + r[3];
    CLIENT.with_borrow_mut(|c| {
        let sel = status_selected(c);
        if c.status_block && on(STATUS_MODE) {
            let back = if button == 1 { " back" } else { "" };
            c.ui_out.push(format!("cpumode {sel}{back}"));
            return;
        }
        if button != 0 {
            return;
        }
        if !c.status_block && on(STATUS_BACK) {
            c.show_jobs = false;
        } else if on(STATUS_CANCEL) {
            if c.status_entries.iter().any(|e| e.0 == sel) {
                c.ui_out.push(format!("cpucancel {sel}"));
            }
        } else if on(STATUS_SCROLL) {
            c.scroll_drag = 1;
        } else if !c.status_block && on(CPU_LIST_SCROLL) {
            c.scroll_drag = 2;
        } else if !c.status_block {
            let top = c.status_list_scroll.min(c.status_cpus.len().saturating_sub(CPU_ROWS));
            for row in 0..CPU_ROWS {
                let at = [CPU_LIST[0] + 9.0, CPU_LIST[1] + 19.0 + row as f32 * (CPU_BUTTON[1] + 1.0), CPU_BUTTON[0], CPU_BUTTON[1]];
                if on(at) && top + row < c.status_cpus.len() {
                    c.status_cpu = top + row;
                    c.status_scroll = 0;
                }
            }
        }
    });
    true
}

/// `native_screen_render_post`: the open dialog, or the crafting status.
pub fn dialog_draw(game: &Game<'_>, batch: &mut HudBatch, mouse: [f32; 2]) {
    toolbar_tooltip(game, batch, mouse);
    toolbelt_draw(game, batch);
    toolbelt_tooltip(game, batch, mouse);
    if CLIENT.with_borrow(|c| c.settings_open) {
        settings_draw(game, batch, mouse);
    }
    status_draw(game, batch, mouse);
    let Some(d) = CLIENT.with_borrow(|c| c.dialog.clone()) else { return };
    if game.native_mod_screen().is_some() {
        let (w, h) = game.gui_size();
        game.native_fill(batch, [0.0, 0.0, w, h], 0xA0000000);
    }
    let [ox, oy] = dialog_origin(game, &d);
    let hover = |r: [f32; 4]| mouse[0] >= ox + r[0] && mouse[0] < ox + r[0] + r[2] && mouse[1] >= oy + r[1] && mouse[1] < oy + r[1] + r[3];
    let shifted = |r: [f32; 4]| [ox + r[0], oy + r[1], r[2], r[3]];
    match &d.kind {
        DialogKind::Confirm(_, _, _) => {
            use super::port_ae2_core::AmountFormat::{Full, Slot};
            game.native_sprite(batch, "ae2:gui/craftingreport", [0.0, 0.0, 238.0, 206.0], [ox, oy, 238.0, 206.0]);
            let cpus_left = d.asked_cpus.is_none_or(|t| t.elapsed().as_secs_f32() >= 1.0);
            if !d.waiting && cpus_left {
                let bytes = d.bytes;
                CLIENT.with_borrow_mut(|c| {
                    c.ui_out.push(format!("plancpus {bytes}"));
                    if let Some(open) = c.dialog.as_mut() {
                        open.asked_cpus = Some(std::time::Instant::now());
                    }
                });
            }
            // CraftConfirmScreen.updateBeforeRender.
            let title = if d.waiting { "Crafting Plan - Calculating Please Wait...".to_owned() } else { format!("Crafting Plan - {} Bytes Used", group_digits(d.bytes)) };
            game.native_text(batch, &title, [ox + 8.0, oy + 7.0], TEXT, false);
            let cells: Vec<TableCell> = d
                .entries
                .iter()
                .map(|(id, stored, missing, craft)| {
                    let mut lines = Vec::new();
                    let mut full = Vec::new();
                    for (n, word) in [(*stored, "Available"), (*missing, "Missing"), (*craft, "To Craft")] {
                        if n > 0 {
                            lines.push(format!("{word}: {}", key_amount(id, n, Slot)));
                            full.push(format!("{word}: {}", key_amount(id, n, Full)));
                        }
                    }
                    TableCell { id: id.clone(), lines, full, background: 0, overlay: if *missing > 0 { CELL_MISSING } else { 0 } }
                })
                .collect();
            let most = cells.len().div_ceil(STATUS_COLS).saturating_sub(5);
            let drag = CLIENT.with_borrow(|c| c.scroll_drag);
            let top = if drag == 3 { scrollbar_row(mouse[1], oy + CONFIRM_SCROLL[1], CONFIRM_SCROLL[3], most) } else { d.scroll.min(most) };
            if drag == 3 {
                CLIENT.with_borrow_mut(|c| {
                    if let Some(open) = c.dialog.as_mut() {
                        open.scroll = top;
                    }
                });
            }
            let hovered = draw_table(game, batch, [ox, oy], 5, &cells, top, mouse);
            draw_scrollbar(game, batch, [ox + CONFIRM_SCROLL[0], oy + CONFIRM_SCROLL[1]], CONFIRM_SCROLL[3], top, most);
            let startable = d.can_start && !d.waiting;
            let picked = usize::try_from(d.selected).ok().and_then(|i| d.cpus.get(i));
            let label = if d.cpus.is_empty() && !d.waiting {
                "No Crafting CPUs are Available".to_owned()
            } else if d.selected >= 0 {
                format!("Crafting CPU: #{}", d.selected + 1)
            } else {
                "Crafting CPU: Automatic".to_owned()
            };
            let label = if startable { label } else { format!("§7{label}") };
            draw_button_at(game, batch, shifted(CONFIRM_CPU), &label, startable && hover(CONFIRM_CPU));
            let status = if d.waiting {
                String::new()
            } else if !d.can_start {
                "Partial Plan (Missing Ingredients)".to_owned()
            } else if let Some(k) = picked {
                format!("Storage: {} : Co Processors: {}", k.1, k.2)
            } else {
                "Storage: N/A : Co Processors: N/A".to_owned()
            };
            let width = game.native_text_width(&status);
            game.native_text(batch, &status, [ox + 109.0 - (width / 2.0).floor(), oy + 165.0], TEXT, false);
            draw_button_at(game, batch, shifted(CONFIRM_CANCEL), "Cancel", hover(CONFIRM_CANCEL));
            let can = startable && !d.cpus.is_empty() && !d.starting;
            draw_button_at(game, batch, shifted(CONFIRM_START), if can { "Start" } else { "§7Start" }, can && hover(CONFIRM_START));
            if let Some(i) = hovered
                && let Some(cell) = cells.get(i)
            {
                table_tooltip(game, batch, cell, mouse);
            }
        }
        DialogKind::Error(..) => {
            game.native_sprite(batch, "ae2:gui/craft_error", [0.0, 0.0, 219.0, 87.0], [ox, oy, 219.0, 87.0]);
            let tw = game.native_text_width(&d.error).min(200.0);
            game.native_text(batch, &d.error, [ox + 110.0 - tw / 2.0, oy + 10.0], 0xFF5555, false);
            if let Some((_, id, _)) = d.plan.iter().find(|(k, _, _)| *k == 'm') {
                draw_key(game, batch, id, [ox + 101.0, oy + 35.0], 16.0);
            }
            for (r, label) in ERROR_BUTTONS.iter().zip(["Replan", "Retry", "Cancel"]) {
                draw_button_at(game, batch, shifted(*r), label, hover(*r));
            }
        }
        kind => {
            let (tex, height, title) = match kind {
                DialogKind::Priority => ("ae2:gui/priority", 125.0, "Priority"),
                DialogKind::Stock(..) => ("ae2:gui/craft_amt", 107.0, "Set Stock Amount"),
                DialogKind::Amount(..) => ("ae2:gui/craft_amt", 107.0, "Set Amount"),
                _ => ("ae2:gui/craft_amt", 107.0, "Select Amount"),
            };
            game.native_sprite(batch, tex, [0.0, 0.0, 176.0, height], [ox, oy, 176.0, height]);
            game.native_text(batch, title, [ox + 8.0, oy + 6.0], 0x404040, false);
            draw_button_at(game, batch, shifted(BACK_BUTTON), "<", hover(BACK_BUTTON));
            for (x, bw, n) in STEPS {
                draw_button_at(game, batch, [ox + x, oy + 30.0, bw, 20.0], &format!("+{n}"), hover([x, 30.0, bw, 20.0]));
                draw_button_at(game, batch, [ox + x, oy + 72.0, bw, 20.0], &format!("-{n}"), hover([x, 72.0, bw, 20.0]));
            }
            let [bx, bw] = if *kind == DialogKind::Priority { [60.0, 61.0] } else { [48.0, 66.0] };
            game.native_fill(batch, [ox + bx - 1.0, oy + 54.0, bw + 2.0, 14.0], 0xFFFFFFFF);
            game.native_fill(batch, [ox + bx, oy + 55.0, bw, 12.0], 0xFF000000);
            // TEXTFIELD_ERROR while it isn't a number, and why under the pointer.
            let problem = d.problem();
            game.native_text(batch, &format!("{}_", d.text), [ox + bx + 2.0, oy + 57.0], if problem.is_some() { 0xFF1900 } else { 0xFFFFFF }, false);
            // NumberEntryWidget's unit, at the box's end.
            if d.unit() > 1.0 {
                game.native_text(batch, "B", [ox + bx + bw - 8.0, oy + 57.0], 0xAAAAAA, false);
            }
            if let Some(why) = problem.filter(|_| hover([bx - 1.0, 54.0, bw + 2.0, 14.0])) {
                game.draw_tooltip_lines(batch, &[(horizon_client::text::Text::legacy(&format!("§c{why}")), u32::MAX)], mouse[0], mouse[1]);
            }
            match kind {
                DialogKind::Priority => {
                    game.native_text(batch, "Higher priority storage is filled first", [ox + 8.0, oy + 98.0], 0x404040, false);
                    game.native_text(batch, "and emptied last.", [ox + 8.0, oy + 110.0], 0x404040, false);
                }
                DialogKind::Craft(_, id) | DialogKind::Stock(_, id) | DialogKind::Amount(_, id) => {
                    draw_key(game, batch, id, [ox + 23.0, oy + 53.0], 16.0);
                    // CraftAmountScreen: with Shift down the button starts the job straight away.
                    let label = if !matches!(kind, DialogKind::Craft(..)) {
                        "Save"
                    } else if CLIENT.with_borrow(|c| c.shift) {
                        "Start"
                    } else {
                        "Next"
                    };
                    draw_button_at(game, batch, shifted(NEXT_BUTTON), label, hover(NEXT_BUTTON));
                }
                _ => {}
            }
        }
    }
}

/// `native_screen_mouse_button_pressed_pre`: a click on the open dialog (nothing under it gets the click).
pub fn dialog_click(game: &Game<'_>, mouse: [f32; 2], button: i32, canceled: &mut bool) {
    if settings_click(game, mouse, button, canceled) || status_click(game, mouse, button, canceled) {
        return;
    }
    if !overlay_shown()
        && let Some([x, y]) = CLIENT.with_borrow(|c| c.toolbelt_at)
        && mouse[0] >= x
        && mouse[0] < x + 68.0
        && mouse[1] >= y
        && mouse[1] < y + 68.0
    {
        *canceled = true;
        if let Some(cell) = toolbelt_cell([mouse[0] - x, mouse[1] - y]) {
            CLIENT.with_borrow_mut(|c| c.toolbelt_presses.push(TOOLBELT + ((cell as i32) << 1) + i32::from(button == 1)));
        }
        return;
    }
    let Some(d) = CLIENT.with_borrow(|c| c.dialog.clone()) else { return };
    *canceled = true;
    let [ox, oy] = dialog_origin(game, &d);
    // CraftConfirmScreen.selectNextCpu: left the next, right the one before (CraftingCPUCycler.cycleCpu).
    if matches!(d.kind, DialogKind::Confirm(..)) && d.can_start && !d.waiting && (button == 0 || button == 1) {
        let at = [mouse[0] - ox, mouse[1] - oy];
        if at[0] >= CONFIRM_CPU[0] && at[0] < CONFIRM_CPU[0] + CONFIRM_CPU[2] && at[1] >= CONFIRM_CPU[1] && at[1] < CONFIRM_CPU[1] + CONFIRM_CPU[3] {
            CLIENT.with_borrow_mut(|c| {
                let Some(open) = c.dialog.as_mut() else { return };
                let count = open.cpus.len() as i32;
                let next = open.selected + if button == 0 { 1 } else { -1 };
                open.selected = if next < -1 { count - 1 } else if next >= count { -1 } else { next };
            });
            return;
        }
    }
    if button != 0 {
        return;
    }
    let at = [mouse[0] - ox, mouse[1] - oy];
    let inside = |r: [f32; 4]| at[0] >= r[0] && at[0] < r[0] + r[2] && at[1] >= r[1] && at[1] < r[1] + r[3];
    CLIENT.with_borrow_mut(|c| match d.kind.clone() {
        DialogKind::Confirm(index, id, amount) => {
            if inside(CONFIRM_CANCEL) {
                let text = amount_text(&id, amount);
                c.dialog = Some(Dialog::new(DialogKind::Craft(index, id), text));
            } else if inside(CONFIRM_START) {
                dialog_confirm(c);
            } else if inside(CONFIRM_SCROLL) {
                c.scroll_drag = 3;
            }
        }
        DialogKind::Error(index, id, amount) => {
            if inside(ERROR_BUTTONS[0]) {
                let text = amount_text(&id, amount);
                c.dialog = Some(Dialog::new(DialogKind::Craft(index, id), text));
            } else if inside(ERROR_BUTTONS[1]) {
                c.ui_out.push(format!("plan {index} {amount}"));
                let mut next = Dialog::new(DialogKind::Confirm(index, id, amount), amount.to_string());
                next.waiting = true;
                c.dialog = Some(next);
            } else if inside(ERROR_BUTTONS[2]) {
                c.dialog = None;
            }
        }
        kind => {
            if inside(BACK_BUTTON) {
                c.dialog = None;
                return;
            }
            if kind != DialogKind::Priority && inside(NEXT_BUTTON) {
                dialog_confirm(c);
                return;
            }
            let least = if kind == DialogKind::Priority { i64::MIN / 2 } else { 1 };
            for (x, bw, n) in STEPS {
                let step = if inside([x, 30.0, bw, 20.0]) {
                    n
                } else if inside([x, 72.0, bw, 20.0]) {
                    -n
                } else {
                    continue;
                };
                if let Some(d) = c.dialog.as_mut() {
                    // The steps are whole buckets for a fluid: the box takes anything finer.
                    let amount = d.value().saturating_add(step * d.unit() as i64).max(least);
                    d.text = match &d.kind {
                        DialogKind::Craft(_, id) | DialogKind::Stock(_, id) | DialogKind::Amount(_, id) => amount_text(id, amount),
                        _ => amount.to_string(),
                    };
                    if kind == DialogKind::Priority {
                        c.ui_out.push(format!("set priority {}", d.text));
                    }
                }
            }
        }
    });
}

/// Keys while a dialog is open: digits, backspace, Enter (confirm), Escape (close). True when the dialog took it.
fn dialog_key(key: &str) -> bool {
    CLIENT.with_borrow_mut(|c| {
        if c.status_block {
            // A crafting block's screen is the menu itself: the keys that close a menu close it.
            return false;
        }
        if c.settings_open {
            if key.ends_with("escape") {
                c.settings_open = false;
            }
            return true;
        }
        if c.show_jobs {
            if key.ends_with("escape") {
                c.show_jobs = false;
            }
            return true;
        }
        let Some(d) = c.dialog.as_mut() else { return false };
        let priority = d.kind == DialogKind::Priority;
        if key.ends_with("backspace") {
            d.text.pop();
        } else if key.ends_with("escape") {
            c.dialog = None;
            return true;
        } else if key.ends_with("enter") {
            dialog_confirm(c);
            return true;
        } else {
            return true;
        }
        if priority && let Some(v) = math_whole(&d.text) {
            c.ui_out.push(format!("set priority {v}"));
        }
        true
    })
}

fn dialog_char(ch: char) -> bool {
    CLIENT.with_borrow_mut(|c| {
        let Some(d) = c.dialog.as_mut() else { return false };
        if matches!(d.kind, DialogKind::Confirm(..) | DialogKind::Error(..)) {
            return true;
        }
        let priority = d.kind == DialogKind::Priority;
        if (ch.is_ascii_digit() || "+-*/%^(). ".contains(ch)) && d.text.len() < 32 {
            if d.text == "0" && ch.is_ascii_digit() {
                d.text.clear();
            }
            d.text.push(ch);
            if priority && let Some(v) = math_whole(&d.text) {
                c.ui_out.push(format!("set priority {v}"));
            }
        }
        true
    })
}

/// The wheel over the crafting plan scrolls it.
fn dialog_scroll(dy: f64) -> bool {
    CLIENT.with_borrow_mut(|c| {
        if c.show_jobs {
            // Over the CPU list, the list (CPUSelectionList.onMouseWheel); anywhere else, the table.
            let (w, h) = c.gui_size_seen;
            let [ox, oy] = [((w - 238.0) / 2.0).floor(), ((h - 184.0) / 2.0).floor()];
            let [mx, my] = c.status_mouse;
            let in_list = !c.status_block && mx >= ox + CPU_LIST[0] && mx < ox && my >= oy + CPU_LIST[1] && my < oy + CPU_LIST[1] + CPU_LIST[3];
            if in_list {
                let most = c.status_cpus.len().saturating_sub(CPU_ROWS);
                c.status_list_scroll = if dy > 0.0 { c.status_list_scroll.saturating_sub(1) } else { (c.status_list_scroll + 1).min(most) };
            } else {
                let n = c.status_entries.iter().filter(|e| e.0 == c.status_cpu).count();
                let most = n.div_ceil(STATUS_COLS).saturating_sub(STATUS_ROWS);
                c.status_scroll = if dy > 0.0 { c.status_scroll.saturating_sub(1) } else { (c.status_scroll + 1).min(most) };
            }
            return true;
        }
        let Some(d) = c.dialog.as_mut() else { return false };
        let rows = d.entries.len().div_ceil(3);
        if dy > 0.0 {
            d.scroll = d.scroll.saturating_sub(1);
        } else if dy < 0.0 {
            d.scroll = (d.scroll + 1).min(rows.saturating_sub(5));
        }
        true
    })
}

/// The server's answer on `ae2:ui`: a plan (or why there's none), an item screen to open, its contents.
fn ui_from_server(data: &[u8]) {
    let text = String::from_utf8_lossy(data).into_owned();
    let mut lines = text.lines();
    let head = lines.next().unwrap_or("");
    let words: Vec<&str> = head.split(' ').collect();
    match words.as_slice() {
        // CompassResponsePacket.
        ["compass", what] => CLIENT.with_borrow_mut(|c| {
            c.compass = match *what {
                "spin" => Compass::Spin,
                "none" => Compass::None,
                r => r.parse().map_or(Compass::None, Compass::At),
            };
        }),
        ["plan", bytes, can] => CLIENT.with_borrow_mut(|c| {
            let Some(d) = c.dialog.as_mut() else { return };
            d.waiting = false;
            d.bytes = bytes.parse().unwrap_or(0);
            d.can_start = *can == "1";
            d.entries.clear();
            d.cpus.clear();
            for l in lines {
                let f: Vec<&str> = l.split(' ').collect();
                match f.as_slice() {
                    ["K", i, b, a] => d.cpus.push((i.parse().unwrap_or(0), b.parse().unwrap_or(0), a.parse().unwrap_or(0))),
                    [k, id, n] => {
                        let n: i64 = n.parse().unwrap_or(0);
                        let at = d.entries.iter().position(|e| e.0 == *id).unwrap_or_else(|| {
                            d.entries.push(((*id).to_owned(), 0, 0, 0));
                            d.entries.len() - 1
                        });
                        match *k {
                            "u" => d.entries[at].1 += n,
                            "m" => d.entries[at].2 += n,
                            _ => d.entries[at].3 += n,
                        }
                    }
                    _ => {}
                }
            }
            // CraftingPlanSummaryEntry.COMPARATOR: missing, then to craft, then available, most first.
            d.entries.sort_by_key(|e| std::cmp::Reverse((e.2, e.3, e.1)));
            // CraftConfirmMenu.isAutoStart: Shift on the amount screen's button skips this screen when it can.
            if d.auto_start && d.can_start && !d.cpus.is_empty() {
                dialog_confirm(c);
            }
        }),
        ["plancpus"] => CLIENT.with_borrow_mut(|c| {
            let Some(d) = c.dialog.as_mut() else { return };
            let picked = usize::try_from(d.selected).ok().and_then(|i| d.cpus.get(i)).map(|k| k.0);
            d.cpus = lines
                .filter_map(|l| {
                    let f: Vec<&str> = l.split(' ').collect();
                    match f.as_slice() {
                        ["K", i, b, a] => Some((i.parse().ok()?, b.parse().ok()?, a.parse().ok()?)),
                        _ => None,
                    }
                })
                .collect();
            d.selected = picked.and_then(|p| d.cpus.iter().position(|k| k.0 == p)).map_or(-1, |i| i as i32);
        }),
        ["started"] => CLIENT.with_borrow_mut(|c| c.dialog = None),
        ["jobdone", id, amount] => CLIENT.with_borrow_mut(|c| c.job_toast = Some(((*id).to_owned(), amount.parse().unwrap_or(0), None))),
        ["error", ..] => CLIENT.with_borrow_mut(|c| {
            let Some(d) = c.dialog.as_mut() else { return };
            if let DialogKind::Confirm(index, id, amount) = d.kind.clone() {
                let mut e = Dialog::new(DialogKind::Error(index, id, amount), amount.to_string());
                e.error = head.trim_start_matches("error ").to_owned();
                for l in lines {
                    let f: Vec<&str> = l.split(' ').collect();
                    if let [k, id, n] = f.as_slice() {
                        e.plan.push((k.chars().next().unwrap_or('m'), (*id).to_owned(), n.parse().unwrap_or(0)));
                    }
                }
                c.dialog = Some(e);
            }
        }),
        ["open", kind] => CLIENT.with_borrow_mut(|c| {
            c.item_screen = (*kind).to_owned();
            c.item_screen_state = 1;
            c.panel = lines.map(str::to_owned).collect();
            if *kind == "portable" {
                c.items.clear();
                c.view.take();
                c.scroll = 0;
            }
        }),
        ["panel"] => CLIENT.with_borrow_mut(|c| c.panel = lines.map(str::to_owned).collect()),
        ["toolbelt", rest @ ..] => CLIENT.with_borrow_mut(|c| {
            let text = rest.join(" ");
            c.toolbelt = if text == "none" {
                Vec::new()
            } else {
                text.split(';').map(|e| e.split_once(' ').map(|(id, n)| (id.to_owned(), n.parse().unwrap_or(0))).unwrap_or_default()).collect()
            };
        }),
        ["toolbox", rest @ ..] => CLIENT.with_borrow_mut(|c| {
            c.toolbox = rest
                .join(" ")
                .split(';')
                .map(|e| e.split_once(' ').map(|(id, n)| (id.to_owned(), n.parse().unwrap_or(0))).unwrap_or_default())
                .collect();
        }),
        ["close"] => CLIENT.with_borrow_mut(|c| c.item_screen_state = 3),
        _ => {}
    }
}

/// The terminal settings (AE2's TerminalSettingsScreen), kept beside the game.
fn settings_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join("horizon").join("ae2-client.txt"))
}

/// TerminalSettingsScreen's checkboxes (screens/terminals/terminal_settings.json): the setting, x, y, width, label, and
/// whether it shows with AE's own search (0), with JEI's (1), or always (2). Settings: 0 auto-focus, 1 remember the
/// search, 3 pin, 4 notify, 5 clear the grid, 6 use JEI's search, 7 sync with JEI, 8 clear JEI's on open.
const SETTINGS: [(usize, f32, f32, f32, &str, u8); 7] = [
    (3, 10.0, 25.0, 180.0, "Pin auto-crafted items to first row", 2),
    (4, 10.0, 50.0, 180.0, "Notify about finished crafting jobs (requires wireless terminal)", 2),
    (5, 10.0, 82.0, 180.0, "Automatically clear terminal grid on close (if applicable)", 2),
    (1, 10.0, 160.0, 150.0, "Remember last search", 0),
    (0, 10.0, 176.0, 150.0, "Auto-Focus on open", 0),
    (7, 10.0, 192.0, 150.0, "Sync with JEI search", 0),
    (8, 10.0, 160.0, 150.0, "Clear JEI search on open", 1),
];
/// The two radio buttons, "Use AE" and "Use JEI": x, y, width.
const SETTINGS_RADIOS: [(f32, f32, f32, &str); 2] = [(10.0, 124.0, 75.0, "Use AE"), (95.0, 124.0, 75.0, "Use JEI")];
const SETTINGS_SIZE: [f32; 2] = [200.0, 216.0];
const SETTINGS_BACK: [f32; 4] = [178.0, 0.0, 22.0, 22.0];
const SETTINGS_DEFAULT: [bool; 9] = [false, true, true, true, true, false, false, true, true];

fn settings(c: &mut Client) -> [bool; 9] {
    if !c.settings_loaded {
        c.settings_loaded = true;
        c.settings = SETTINGS_DEFAULT;
        c.term_style = 3;
        if let Some(text) = settings_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            for (i, l) in text.lines().take(9).enumerate() {
                c.settings[i] = l.trim() == "true";
            }
            // The toolbar's, after the checkboxes.
            let n: Vec<u8> = text.lines().skip(9).filter_map(|l| l.trim().parse().ok()).collect();
            if let [sort, view, filter, desc, style] = n.as_slice() {
                c.sort = *sort;
                c.view_mode = *view;
                c.type_filter = *filter;
                c.sort_desc = *desc != 0;
                c.term_style = (*style).min(3);
            }
        }
    }
    c.settings
}

fn save_settings(c: &Client) {
    if let Some(p) = settings_path() {
        let mut text = c.settings.iter().map(|b| format!("{b}\n")).collect::<String>();
        for n in [c.sort, c.view_mode, c.type_filter, u8::from(c.sort_desc), c.term_style] {
            text.push_str(&format!("{n}\n"));
        }
        let _ = std::fs::write(p, text);
    }
}

fn settings_origin(game: &Game<'_>) -> [f32; 2] {
    let (w, h) = game.gui_size();
    [((w - SETTINGS_SIZE[0]) / 2.0).floor(), ((h - SETTINGS_SIZE[1]) / 2.0).floor()]
}

/// BackgroundGenerator.draw: guis/background.png's corners, edges and middle stretched by tiling to the window's size.
fn draw_generated_background(game: &Game<'_>, batch: &mut HudBatch, at: [f32; 2], size: [f32; 2]) {
    let [x, y] = at;
    let [w, h] = size;
    let (iw, ih) = (w - 8.0, h - 8.0);
    let sprite = "ae2:gui/background";
    for (src, dst) in [
        ([0.0, 0.0, 4.0, 4.0], [x, y, 4.0, 4.0]),
        ([252.0, 0.0, 4.0, 4.0], [x + w - 4.0, y, 4.0, 4.0]),
        ([0.0, 252.0, 4.0, 4.0], [x, y + h - 4.0, 4.0, 4.0]),
        ([252.0, 252.0, 4.0, 4.0], [x + w - 4.0, y + h - 4.0, 4.0, 4.0]),
        ([4.0, 0.0, iw, 4.0], [x + 4.0, y, iw, 4.0]),
        ([4.0, 252.0, iw, 4.0], [x + 4.0, y + h - 4.0, iw, 4.0]),
        ([0.0, 4.0, 4.0, ih], [x, y + 4.0, 4.0, ih]),
        ([252.0, 4.0, 4.0, ih], [x + w - 4.0, y + 4.0, 4.0, ih]),
        ([4.0, 4.0, iw, ih], [x + 4.0, y + 4.0, iw, ih]),
    ] {
        game.native_sprite(batch, sprite, src, dst);
    }
}

/// AECheckbox: its 14-pixel box from guis/checkbox.png, the label wrapped to the rest of its width.
fn draw_checkbox(game: &Game<'_>, batch: &mut HudBatch, at: [f32; 2], width: f32, label: &str, on: bool, radio: bool, hover: bool) {
    let u = if radio { 28.0 } else { 0.0 } + if hover { 14.0 } else { 0.0 };
    let v = if on { 14.0 } else { 0.0 };
    game.native_sprite(batch, "ae2:gui/checkbox", [u, v, 14.0, 14.0], [at[0], at[1], 14.0, 14.0]);
    let mut lines: Vec<String> = Vec::new();
    for word in label.split(' ') {
        match lines.last_mut() {
            Some(line) if game.native_text_width(&format!("{line} {word}")) <= width - 16.0 => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    let mut y = at[1] + if lines.len() <= 1 { 4.0 } else { 1.0 };
    for line in &lines {
        game.native_text(batch, line, [at[0] + 16.0, y], TEXT, false);
        y += 9.0;
    }
}

/// TerminalSettingsScreen.
fn settings_draw(game: &Game<'_>, batch: &mut HudBatch, mouse: [f32; 2]) {
    let (s, host) = CLIENT.with_borrow_mut(|c| (settings(c), c.status_host.clone()));
    let [ox, oy] = settings_origin(game);
    if game.native_mod_screen().is_some() {
        let (w, h) = game.gui_size();
        game.native_fill(batch, [0.0, 0.0, w, h], 0xA0000000);
    }
    draw_generated_background(game, batch, [ox, oy], SETTINGS_SIZE);
    game.native_text(batch, "Terminal Settings", [ox + 8.0, oy + 7.0], TEXT, false);
    game.native_text(batch, "Search Settings", [ox + 8.0, oy + 110.0], TEXT, false);
    let over = |x: f32, y: f32, w: f32, h: f32| mouse[0] >= ox + x && mouse[0] < ox + x + w && mouse[1] >= oy + y && mouse[1] < oy + y + h;
    for (i, (x, y, w, label)) in SETTINGS_RADIOS.iter().enumerate() {
        draw_checkbox(game, batch, [ox + x, oy + y], *w, label, s[6] == (i == 1), true, over(*x, *y, *w, 14.0));
    }
    for (index, x, y, w, label, shows) in SETTINGS {
        if shows == 2 || (shows == 1) == s[6] {
            draw_checkbox(game, batch, [ox + x, oy + y], w, label, s[index], false, over(x, y, w, 14.0));
        }
    }
    // The back button: TabButton's BOX style, with the terminal's icon.
    let [bx, by, bw, bh] = SETTINGS_BACK;
    game.native_sprite(batch, "ae2:gui/states", [160.0, 192.0, bw, bh], [ox + bx, oy + by, bw, bh]);
    let icon = if host.is_empty() { "ae2:terminal" } else { host.as_str() };
    draw_key(game, batch, icon, [ox + bx + 3.0, oy + by + 3.0], 16.0);
    if over(bx, by, bw, bh) {
        let lines = vec![(horizon_client::text::Text::legacy(&key_words(game, icon)), u32::MAX)];
        game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
    }
}

/// A press while the settings are open: they take them all.
fn settings_click(game: &Game<'_>, mouse: [f32; 2], button: i32, canceled: &mut bool) -> bool {
    if !CLIENT.with_borrow(|c| c.settings_open) {
        return false;
    }
    *canceled = true;
    if button != 0 {
        return true;
    }
    let [ox, oy] = settings_origin(game);
    let over = |x: f32, y: f32, w: f32, h: f32| mouse[0] >= ox + x && mouse[0] < ox + x + w && mouse[1] >= oy + y && mouse[1] < oy + y + h;
    CLIENT.with_borrow_mut(|c| {
        settings(c);
        let [bx, by, bw, bh] = SETTINGS_BACK;
        if over(bx, by, bw, bh) {
            c.settings_open = false;
            return;
        }
        for (i, (x, y, w, _)) in SETTINGS_RADIOS.iter().enumerate() {
            if over(*x, *y, *w, 14.0) {
                c.settings[6] = i == 1;
                c.search_focus = false;
                save_settings(c);
                return;
            }
        }
        for (index, x, y, w, _, shows) in SETTINGS {
            if (shows == 2 || (shows == 1) == c.settings[6]) && over(x, y, w, 14.0) {
                c.settings[index] = !c.settings[index];
                save_settings(c);
                let notify = c.settings[4];
                c.ui_out.push(format!("notify {}", u8::from(notify)));
                return;
            }
        }
    });
    true
}

/// A craftable entry clicked: AE2 asks how many.
fn ask_craft(index: usize) -> bool {
    let id = CLIENT.with_borrow(|c| c.items.get(index).map(|(id, _)| id.clone()).unwrap_or_default());
    open_dialog(DialogKind::Craft(index, id), "1".to_owned());
    true
}

// ---- The item screens over the world (a mod screen, as the wireless terminal's).

/// The item screen's top left, for its size.
fn item_screen_size(kind: &str) -> [f32; 2] {
    match kind {
        "portable" => [195.0, 166.0],
        "status" => [195.0, 157.0],
        "knife" => [176.0, 184.0],
        _ => [176.0, 166.0],
    }
}

fn item_screen_origin(game: &Game<'_>, kind: &str) -> [f32; 2] {
    let (w, h) = game.gui_size();
    let [sw, sh] = item_screen_size(kind);
    [((w - sw) / 2.0).floor(), ((h - sh) / 2.0).floor()]
}

/// The player's inventory on a screen drawn by this mod: three rows, then the hotbar; the stack under the pointer.
fn draw_inventory(game: &Game<'_>, batch: &mut HudBatch, ox: f32, inv_y: f32, mouse: [f32; 2]) -> Option<ItemStack> {
    let mut hovered = None;
    game.native_text(batch, "Inventory", [ox + 8.0, inv_y - 11.0], 0x404040, false);
    for i in 0..36 {
        let (x, y) = if i < 9 { (ox + 8.0 + 18.0 * i as f32, inv_y + 58.0) } else { (ox + 8.0 + 18.0 * ((i - 9) % 9) as f32, inv_y + 18.0 * ((i - 9) / 9) as f32) };
        if let Some(Some(st)) = game.inventory.get(i) {
            if mouse[0] >= x && mouse[0] < x + 16.0 && mouse[1] >= y && mouse[1] < y + 16.0 {
                hovered = Some(st.clone());
            }
            game.native_item(batch, &st.item, [x, y], 16.0);
            if st.count > 1 {
                let l = st.count.to_string();
                let w = game.native_text_width(&l);
                game.native_text(batch, &l, [x + 17.0 - w, y + 9.0], 0xFFFFFF, true);
            }
        }
    }
    hovered
}

/// The inventory slot (as the server numbers them: hotbar 0..9, then the rows) under a click.
fn inventory_slot_at(at: [f32; 2], inv_y: f32) -> Option<i32> {
    let (cx, cy) = ((at[0] - 8.0) / 18.0, at[1] - inv_y);
    if !(0.0..9.0).contains(&cx) {
        return None;
    }
    if (0.0..54.0).contains(&cy) {
        Some(9 + (cy / 18.0) as i32 * 9 + cx as i32)
    } else if (58.0..76.0).contains(&cy) {
        Some(cx as i32)
    } else {
        None
    }
}

/// `native_screen_render_post` for the item screens.
pub fn item_screen_draw(game: &Game<'_>, batch: &mut HudBatch, mouse: [f32; 2]) {
    let (kind, state) = CLIENT.with_borrow(|c| (c.item_screen.clone(), c.item_screen_state));
    if state != 2 || game.native_mod_screen() != Some("ae2") {
        return;
    }
    let [ox, oy] = item_screen_origin(game, &kind);
    let mut hovered: Option<(ItemStack, Option<i64>)> = None;
    match kind.as_str() {
        "portable" => {
            // AE2's portable cell: a terminal of three rows over the cell's own contents.
            CLIENT.with_borrow_mut(|c| c.rows = 3);
            screen_draw(game, "ae2:terminal", &[1], batch, [ox, oy]);
            game.native_fill(batch, [ox + 8.0, oy + 5.0, 60.0, 10.0], 0xFFC6C6C6);
            game.native_text(batch, "Portable Cell", [ox + 8.0, oy + 6.0], 0x404040, false);
            let [gx, gy] = [mouse[0] - ox - TERM_GRID[0], mouse[1] - oy - TERM_GRID[1]];
            if gx >= 0.0 && gy >= 0.0 && gx < TERM_COLS as f32 * 18.0 && gy < 3.0 * 18.0 {
                let cell = CLIENT.with_borrow(|c| c.scroll * TERM_COLS) + (gy / 18.0) as usize * TERM_COLS + (gx / 18.0) as usize;
                hovered = CLIENT.with_borrow(|c| visible(c).get(cell).and_then(|&i| c.items.get(i)).map(|(id, n)| (ItemStack::new(&icon_of(id), 1), Some(*n))));
            }
            if let Some(st) = draw_inventory(game, batch, ox, oy + 86.0, mouse) {
                hovered = Some((st, None));
            }
        }
        "toolbox" => {
            game.native_sprite(batch, "ae2:gui/toolbox", [0.0, 0.0, 176.0, 166.0], [ox, oy, 176.0, 166.0]);
            game.native_text(batch, "Network Tool", [ox + 8.0, oy + 6.0], 0x404040, false);
            let stacks = CLIENT.with_borrow(|c| c.toolbox.clone());
            for (i, (id, n)) in stacks.iter().enumerate().take(9) {
                if id.is_empty() || *n <= 0 {
                    continue;
                }
                let (x, y) = (ox + 62.0 + 18.0 * (i % 3) as f32, oy + 19.0 + 18.0 * (i / 3) as f32);
                game.native_item(batch, id, [x, y], 16.0);
                if *n > 1 {
                    let l = n.to_string();
                    let w = game.native_text_width(&l);
                    game.native_text(batch, &l, [x + 17.0 - w, y + 9.0], 0xFFFFFF, true);
                }
                if mouse[0] >= x && mouse[0] < x + 16.0 && mouse[1] >= y && mouse[1] < y + 16.0 {
                    hovered = Some((ItemStack::new(id, 1), None));
                }
            }
            if let Some(st) = draw_inventory(game, batch, ox, oy + 84.0, mouse) {
                hovered = Some((st, None));
            }
        }
        "knife" => {
            game.native_sprite(batch, "ae2:gui/quartzknife", [0.0, 0.0, 176.0, 184.0], [ox, oy, 176.0, 184.0]);
            game.native_text(batch, "Cutting Knife", [ox + 8.0, oy + 6.0], 0x404040, false);
            let (name, focus) = CLIENT.with_borrow(|c| (c.knife_name.clone(), c.knife_focus));
            game.native_fill(batch, [ox + 20.0, oy + 29.0, 92.0, 14.0], if focus { 0xFFFFFFFF } else { 0xFFA0A0A0 });
            game.native_fill(batch, [ox + 21.0, oy + 30.0, 90.0, 12.0], 0xFF000000);
            game.native_text(batch, &if focus { format!("{name}_") } else { name.clone() }, [ox + 23.0, oy + 32.0], 0xFFFFFF, false);
            // The input: the iron the player carries (the knife cuts from it); the output: the press it makes.
            let iron: u32 = game.inventory.iter().flatten().filter(|s| &*s.item == "minecraft:iron_ingot").map(|s| s.count).sum();
            if iron > 0 {
                game.native_item(batch, "minecraft:iron_ingot", [ox + 94.0, oy + 44.0], 16.0);
                let l = iron.to_string();
                let w = game.native_text_width(&l);
                game.native_text(batch, &l, [ox + 111.0 - w, oy + 53.0], 0xFFFFFF, true);
                if !name.is_empty() {
                    game.native_item(batch, "ae2:name_press", [ox + 134.0, oy + 44.0], 16.0);
                    if mouse[0] >= ox + 134.0 && mouse[0] < ox + 150.0 && mouse[1] >= oy + 44.0 && mouse[1] < oy + 60.0 {
                        let lines = vec![
                            (horizon_client::text::Text::legacy(&format!("Name Press: {name}")), u32::MAX),
                            (horizon_client::text::Text::legacy("§7Click to cut it"), u32::MAX),
                        ];
                        game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
                    }
                }
            } else {
                game.native_text(batch, "§7Needs an Iron Ingot", [ox + 8.0, oy + 66.0], 0x404040, false);
            }
            if let Some(st) = draw_inventory(game, batch, ox, oy + 102.0, mouse) {
                hovered = Some((st, None));
            }
        }
        "status" => {
            // AE2's NetworkStatusScreen: power at the top, a grid of the network's devices, the rates under it.
            game.native_sprite(batch, "ae2:gui/networkstatus", [0.0, 0.0, 195.0, 157.0], [ox, oy, 195.0, 157.0]);
            let panel = CLIENT.with_borrow(|c| c.panel.clone());
            let channels = panel.iter().find_map(|l| l.strip_prefix("c ")).unwrap_or("0");
            game.native_text(batch, &format!("Network Details ({channels} Channels)"), [ox + 8.0, oy + 6.0], 0x404040, false);
            let mut texts = Vec::new();
            let mut devices = Vec::new();
            for l in &panel {
                if let Some(t) = l.strip_prefix("t ") {
                    texts.push(t.to_owned());
                } else if let Some((id, n)) = l.strip_prefix("n ").and_then(|r| r.split_once(' ')) {
                    devices.push((id.to_owned(), n.parse::<i64>().unwrap_or(0)));
                }
            }
            for (i, t) in texts.iter().take(2).enumerate() {
                game.native_text(batch, t, [ox + 13.0, oy + 16.0 + 10.0 * i as f32], 0x404040, false);
            }
            for (i, t) in texts.iter().skip(2).take(3).enumerate() {
                game.native_text(batch, t, [ox + 13.0, oy + 123.0 + 10.0 * i as f32], 0x404040, false);
            }
            for (i, (id, n)) in devices.iter().take(32).enumerate() {
                let (x, y) = (ox + 14.0 + 20.0 * (i % 8) as f32, oy + 39.0 + 19.0 * (i / 8) as f32);
                game.native_item(batch, id, [x, y], 16.0);
                let l = readable_number::format_i64(*n, 4);
                let w = game.native_text_width(&l) * 0.5;
                let start = batch.vertices.len();
                game.native_text(batch, &l, [x + 16.0 - w, y + 12.0], 0xFFFFFF, true);
                batch.scale_since(start, x + 16.0 - w, y + 12.0, 0.5, 0.5);
                if mouse[0] >= x && mouse[0] < x + 16.0 && mouse[1] >= y && mouse[1] < y + 16.0 {
                    hovered = Some((ItemStack::new(id, 1), Some(*n)));
                }
            }
        }
        _ => return,
    }
    if let Some((stack, count)) = hovered {
        super::port_ae2_guide::hovered(&stack.item);
        let mut lines = game.stack_tooltip(&stack);
        if let Some(n) = count {
            let label = if kind == "status" { "Count" } else { "Stored" };
            lines.push((
                horizon_client::text::Text::legacy(&format!("§7{label}: {}", super::port_ae2_core::KeyType::Items.format_amount(n, super::port_ae2_core::AmountFormat::Full))),
                u32::MAX,
            ));
        }
        game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
    }
}

/// A click on an item screen.
pub fn item_screen_click(game: &Game<'_>, mouse: [f32; 2], button: i32, canceled: &mut bool) {
    let (kind, state) = CLIENT.with_borrow(|c| (c.item_screen.clone(), c.item_screen_state));
    if state != 2 || game.native_mod_screen() != Some("ae2") {
        return;
    }
    *canceled = true;
    let [ox, oy] = item_screen_origin(game, &kind);
    let at = [mouse[0] - ox, mouse[1] - oy];
    let right = button == 1;
    let shift = game.keys.contains(&winit::keyboard::KeyCode::ShiftLeft) || game.keys.contains(&winit::keyboard::KeyCode::ShiftRight);
    let mut out = Vec::new();
    match kind.as_str() {
        "portable" => {
            let [gx, gy] = [at[0] - TERM_GRID[0], at[1] - TERM_GRID[1]];
            if gx >= 0.0 && gy >= 0.0 && gx < TERM_COLS as f32 * 18.0 && gy < 3.0 * 18.0 {
                let cell = CLIENT.with_borrow(|c| c.scroll * TERM_COLS) + (gy / 18.0) as usize * TERM_COLS + (gx / 18.0) as usize;
                if let Some(index) = CLIENT.with_borrow(|c| visible(c).get(cell).copied()) {
                    out.push(format!("take {index} {} {}", u8::from(right), u8::from(shift)));
                }
            } else if let Some(slot) = inventory_slot_at(at, 86.0) {
                out.push(format!("put {slot}"));
            } else {
                let mut ignored = None;
                CLIENT.with_borrow_mut(|c| c.rows = 3);
                screen_click(game, "ae2:terminal", at, right, &mut ignored);
            }
        }
        "toolbox" => {
            let (cx, cy) = ((at[0] - 62.0) / 18.0, (at[1] - 19.0) / 18.0);
            if (0.0..3.0).contains(&cx) && (0.0..3.0).contains(&cy) {
                out.push(format!("take {}", cy as i32 * 3 + cx as i32));
            } else if let Some(slot) = inventory_slot_at(at, 84.0) {
                out.push(format!("put {slot}"));
            }
        }
        "knife" => {
            let in_box = at[0] >= 21.0 && at[0] < 111.0 && at[1] >= 30.0 && at[1] < 42.0;
            CLIENT.with_borrow_mut(|c| {
                c.knife_focus = in_box;
                if in_box && right {
                    c.knife_name.clear();
                }
            });
            if at[0] >= 134.0 && at[0] < 150.0 && at[1] >= 44.0 && at[1] < 60.0 {
                let name = CLIENT.with_borrow(|c| c.knife_name.clone());
                if !name.is_empty() {
                    out.push(format!("knife {name}"));
                }
            }
        }
        _ => {}
    }
    CLIENT.with_borrow_mut(|c| c.ui_out.extend(out));
}

/// Keys for the knife's name box. True when it took the key.
fn item_screen_key(key: &str) -> bool {
    CLIENT.with_borrow_mut(|c| {
        if c.item_screen_state != 2 || c.item_screen != "knife" || !c.knife_focus {
            return false;
        }
        if key.ends_with("backspace") {
            c.knife_name.pop();
        } else if key.ends_with("escape") || key.ends_with("enter") {
            c.knife_focus = false;
        }
        true
    })
}

fn item_screen_char(ch: char) -> bool {
    CLIENT.with_borrow_mut(|c| {
        if c.item_screen_state != 2 || c.item_screen != "knife" || !c.knife_focus {
            return false;
        }
        if c.knife_name.chars().count() < 32 && !ch.is_control() {
            c.knife_name.push(ch);
        }
        true
    })
}

/// The frame's part: messages to the server, and the item screen opened (or its closing told).
fn pump_ui(game: &mut Game<'_>) {
    // Menu buttons the scroll hook queued, sent as a click on a screen's button is (native_screen_press).
    let buttons = CLIENT.with_borrow_mut(|c| std::mem::take(&mut c.buttons_out));
    if !buttons.is_empty()
        && let Some(container) = game.remote.as_ref().and_then(|r| r.menu.as_ref()).map(|(w, _)| *w)
    {
        for button in buttons {
            game.send(horizon_protocol::packets::play::Serverbound::ContainerButtonClick(
                horizon_protocol::packets::play::ContainerButtonClick { container, button },
            ));
        }
    }
    let out = CLIENT.with_borrow_mut(|c| std::mem::take(&mut c.ui_out));
    for m in out {
        let _ = game.native_net_send_to_server(NET_UI, m.as_bytes());
    }
    let state = CLIENT.with_borrow(|c| c.item_screen_state);
    if state == 1 && game.native_mod_screen_open("ae2") {
        CLIENT.with_borrow_mut(|c| c.item_screen_state = 2);
    } else if state == 2 && game.native_mod_screen() != Some("ae2") {
        CLIENT.with_borrow_mut(|c| c.item_screen_state = 0);
        let _ = game.native_net_send_to_server(NET_UI, b"close");
    } else if state == 3 {
        CLIENT.with_borrow_mut(|c| c.item_screen_state = 0);
        game.native_mod_screen_close("ae2");
    }
    // A dialog stays only over a screen.
    if !game.native_screen_shown() && game.native_mod_screen().is_none() && CLIENT.with_borrow(|c| c.dialog.is_some()) {
        CLIENT.with_borrow_mut(|c| c.dialog = None);
    }
}

// ---- Server side of the dialogs and item screens.

fn ui_from_client(game: &mut Game<'_>, player: u128, text: &str) {
    if let Some(mode) = game.native_server_players().into_iter().find(|p| p.uuid == player).map(|p| p.game_mode) {
        button_clicker(player, mode);
    }
    ui_message(game, player, text);
    button_done();
}

fn ui_message(game: &mut Game<'_>, player: u128, text: &str) {
    let w: Vec<&str> = text.split(' ').collect();
    let viewing = SERVER.with_borrow(|s| s.viewers.get(&player).copied());
    match w.as_slice() {
        // InventoryAction.CREATIVE_DUPLICATE: a full stack of the terminal's item, from nothing (creative only).
        ["clone", index] => {
            let Ok(index) = index.parse::<usize>() else { return };
            let creative = creative_player(game, player);
            let Some((key, _)) = SERVER.with_borrow(|s| s.sent.get(&player).and_then(|v| v.keys.get(index).cloned())) else { return };
            if creative && key.key_type() == super::port_ae2_core::KeyType::Items {
                let stack = stack_of(&key, 1);
                let most = max_stack(game, &stack.item);
                let _ = game.native_give_stack(player, stack_of(&key, most));
            }
        }
        // The screen with the toolbelt closed: what its clicks did goes on the tool.
        ["toolbeltdone"] => {
            SERVER.with_borrow_mut(|s| {
                s.toolbelt_flush.insert(player);
            });
        }
        // Notify about finished crafting jobs.
        ["notify", on] => SERVER.with_borrow_mut(|s| {
            if *on == "1" {
                s.no_notify.remove(&player);
            } else {
                s.no_notify.insert(player);
            }
        }),
        // Clear crafting grid on close: what is left in it, back to the network.
        ["cleargrid"] => {
            let Some(pos) = viewing else { return };
            if block_id(game, pos) == "ae2:crafting_terminal"
                && let Some(grid) = SERVER.with_borrow(|s| s.grid_of.get(&pos).copied())
            {
                let old: Vec<ItemStack> = game.native_with_mod_block_entity(pos, |e| e.items[CT_GRID].iter_mut().filter_map(Option::take).collect()).unwrap_or_default();
                for stack in old {
                    let key = key_of(game, &stack);
                    let stored = with_storage(game, grid, None, |st| st.insert(&key, i64::from(stack.count), Actionable::Modulate));
                    if stored < i64::from(stack.count) {
                        let mut left = stack.clone();
                        left.count -= stored as u32;
                        let _ = game.native_give_stack(player, left);
                    }
                }
            }
        }
        // The crafting status screen: cancel a CPU's job (CraftingCPUMenu.cancelCrafting), step its mode.
        ["cpucancel", i] | ["cpumode", i] => {
            let Ok(i) = i.parse::<usize>() else { return };
            let Some(at) = viewing else { return };
            let found = SERVER.with_borrow(|s| {
                let grid = *s.grid_of.get(&at)?;
                let g = s.grids.get(grid)?;
                Some((grid, g.cpus.get(i)?.0, g.patterns.clone()))
            });
            let Some((grid, key, patterns)) = found else { return };
            if w[0] == "cpumode" {
                let step = if w.get(2) == Some(&"back") { 2 } else { 1 };
                let next = (provider_nbt(game, key, "cpu_mode") + step) % 3;
                game.native_with_mod_block_entity(key, |e| set_nbt_i64(&mut e.nbt, "cpu_mode", next));
                return;
            }
            let Some(mut cpu) = SERVER.with_borrow_mut(|s| s.cpus.remove(&key)) else { return };
            let tick = SERVER.with_borrow(|s| s.tick);
            {
                let mut net = CpuNet { game, grid, cpu: key, patterns, tick };
                cpu.cancel(&mut net);
            }
            SERVER.with_borrow_mut(|s| s.cpus.insert(key, cpu));
        }
        // CompassRequestPacket: the direction from the player's chunk (CompassService, 174 chunks out).
        ["compass", cx, cz] => {
            let (Ok(cx), Ok(cz)) = (cx.parse::<i32>(), cz.parse::<i32>()) else { return };
            let dim = game.dim.id().to_owned();
            let reply = match super::port_ae2_meteor::nearest(game, &dim, [cx, cz], 174) {
                Some(c) if c == [cx, cz] => "compass spin".to_owned(),
                Some(c) => {
                    let (up, side) = (f64::from(c[1] - cz), f64::from(c[0] - cx));
                    format!("compass {}", (-up).atan2(side) - std::f64::consts::FRAC_PI_2)
                }
                None => "compass none".to_owned(),
            };
            let _ = game.native_net_send_to_player(player, NET_UI, reply.as_bytes());
        }
        ["plan", index, amount] => {
            let (Ok(index), Ok(amount)) = (index.parse::<usize>(), amount.parse::<i64>()) else { return };
            plan_reply(game, player, index, amount.max(1));
        }
        ["craft", index, amount, cpu] => {
            let (Ok(index), Ok(amount)) = (index.parse::<usize>(), amount.parse::<i64>()) else { return };
            let Some(grid) = viewing.and_then(|p| SERVER.with_borrow(|s| s.grid_of.get(&p).copied())) else { return };
            let Some((key, _)) = SERVER.with_borrow(|s| s.sent.get(&player).and_then(|v| v.keys.get(index).cloned())) else { return };
            // CraftConfirmMenu.startJob: the screen stays until the job has started, or says why it didn't.
            let reply = match submit_craft(game, grid, player, &key, amount.max(1), cpu.parse::<usize>().ok()) {
                Ok(()) => "started".to_owned(),
                Err(e) => format!("error {e}"),
            };
            let _ = game.native_net_send_to_player(player, NET_UI, reply.as_bytes());
        }
        // CraftingCPUCycler.detectAndSendChanges: the plan's CPU button keeps up with the network.
        ["plancpus", bytes] => {
            let Ok(bytes) = bytes.parse::<i64>() else { return };
            let Some(grid) = viewing.and_then(|p| SERVER.with_borrow(|s| s.grid_of.get(&p).copied())) else { return };
            let reply = format!("plancpus\n{}", plan_cpus(grid, bytes));
            let _ = game.native_net_send_to_player(player, NET_UI, reply.as_bytes());
        }
        // A JEI ghost ingredient dropped on config slot button `id`: the click with the item held.
        ["ghost", id, item, components @ ..] => {
            let (Some(pos), Ok(id)) = (viewing, id.parse::<i32>()) else { return };
            if !(GHOST..GHOST + (1 << 20)).contains(&id) {
                return;
            }
            let components = components.join(" ");
            let (damage, extra) = horizon_client::item_data::command_components(&components)
                .map_or((0, None), |tag| horizon_client::item_data::decode_components(Some(&tag)));
            let Some(mut entity) = game.native_with_mod_block_entity(pos, |e| e.clone()) else { return };
            let mut carried = Some(ItemStack { item: (*item).into(), count: 1, damage, extra });
            menu_button(game, pos, &mut entity, id, &mut carried);
            game.native_with_mod_block_entity(pos, |e| *e = entity);
        }
        ["button", id] => {
            let (Some(pos), Ok(id)) = (viewing, id.parse::<i32>()) else { return };
            let Some(mut entity) = game.native_with_mod_block_entity(pos, |e| e.clone()) else { return };
            let mut carried = None;
            menu_button(game, pos, &mut entity, id, &mut carried);
            game.native_with_mod_block_entity(pos, |e| *e = entity);
        }
        ["set", "priority", n] => {
            let (Some(pos), Ok(n)) = (viewing, n.parse::<i64>()) else { return };
            if has_priority(&block_id(game, pos)) || canon(&block_id(game, pos)) == "ae2:pattern_provider" {
                game.native_with_mod_block_entity(pos, |e| set_nbt_i64(&mut e.nbt, "priority", n.clamp(i32::MIN as i64 / 2, i32::MAX as i64 / 2)));
                // The network's storage is mounted again in the new order.
                SERVER.with_borrow_mut(|s| s.mounted.clear());
            }
        }
        ["set", what @ ("stock" | "amount"), cell, n] => {
            let (Some(pos), Ok(cell), Ok(n)) = (viewing, cell.parse::<usize>(), n.parse::<i64>()) else { return };
            let block = block_id(game, pos);
            let ok = match *what {
                "stock" => canon(&block) == "ae2:interface" && cell < 9,
                _ => block == "ae2:pattern_encoding_terminal" && cell < ENC_CELLS,
            };
            if ok {
                game.native_with_mod_block_entity(pos, |e| {
                    let mut cells = if e.id == "ae2:pattern_encoding_terminal" { enc_ghosts(e) } else { ghosts_of(e) };
                    if let Some(Some((_, amount))) = cells.get_mut(cell) {
                        *amount = n.max(1);
                    }
                    set_ghosts(e, &cells);
                });
            }
        }
        ["close"] => SERVER.with_borrow_mut(|s| {
            s.item_screens.remove(&player);
            s.portable_sent.remove(&player);
            s.item_ops.retain(|(u, _)| *u != player);
        }),
        _ => {
            // The item screens' clicks wait for the player's inventory tick (the held item is changed there).
            if SERVER.with_borrow(|s| s.item_screens.contains_key(&player)) {
                SERVER.with_borrow_mut(|s| s.item_ops.push((player, text.to_owned())));
            }
        }
    }
}

/// Blocks with AE2's priority: storage the network fills in order.
fn has_priority(block: &str) -> bool {
    matches!(ae2_name(block), Some("drive" | "chest" | "storage_bus" | "formation_plane"))
}

fn priority_of(game: &mut Game<'_>, pos: Pos) -> i32 {
    game.native_with_mod_block_entity(pos, |e| nbt_i64(&e.nbt, "priority")).unwrap_or(0) as i32
}

/// StorageLevelEmitterPart.getEmitableItems: what the grid's emitters set to craft by redstone can make.
fn emitable_keys(game: &Game<'_>, grid: usize) -> Vec<AeKey> {
    let nodes: Vec<Pos> = SERVER.with_borrow(|s| s.grids.get(grid).map_or(Vec::new(), |g| g.nodes.iter().copied().collect()));
    let mut out = Vec::new();
    for pos in nodes {
        if block_id(game, pos) != "ae2:level_emitter" {
            continue;
        }
        let Some(horizon_client::containers::BlockEntity::Mod(m)) = game.world.block_entities.get(&(pos[0], pos[1], pos[2])) else { continue };
        if nbt_i64(&m.nbt, "craft_redstone") == 0 || cards(m, 1, "ae2:crafting_card") == 0 {
            continue;
        }
        if let Some((id, _)) = ghosts_of(m).into_iter().next().flatten() {
            out.push(ghost_key(game, &id));
        }
    }
    out
}

/// CraftingCPUCycler: the CPUs the plan's button can choose, those free and big enough, most co-processors then
/// most storage first: a `K index bytes co-processors` line each.
fn plan_cpus(grid: usize, bytes: i64) -> String {
    SERVER.with_borrow(|s| {
        let Some(g) = s.grids.get(grid) else { return String::new() };
        let mut free: Vec<(usize, i64, i32)> = g
            .cpus
            .iter()
            .enumerate()
            .filter(|(_, c)| c.1 >= bytes && s.cpus.get(&c.0).is_some_and(|cpu| !cpu.has_job()))
            .map(|(i, c)| (i, c.1, c.2))
            .collect();
        free.sort_by_key(|c| (std::cmp::Reverse(c.2), std::cmp::Reverse(c.1)));
        free.iter().map(|(i, b, a)| format!("K {i} {b} {a}\n")).collect()
    })
}

/// AE2's crafting calculation for the confirm screen: what the job takes from storage, what it crafts, what is
/// missing, its bytes and whether a CPU can take it.
fn plan_reply(game: &mut Game<'_>, uuid: u128, index: usize, amount: i64) {
    let send = |game: &mut Game<'_>, text: String| {
        let _ = game.native_net_send_to_player(uuid, NET_UI, text.as_bytes());
    };
    let Some(grid) = SERVER.with_borrow(|s| s.viewers.get(&uuid).and_then(|p| s.grid_of.get(p)).copied()) else {
        send(game, "error The network is offline".to_owned());
        return;
    };
    let Some((key, _)) = SERVER.with_borrow(|s| s.sent.get(&uuid).and_then(|v| v.keys.get(index).cloned())) else {
        send(game, "error That item is no longer on the network".to_owned());
        return;
    };
    let patterns = SERVER.with_borrow(|s| s.grids.get(grid).map_or(Vec::new(), |g| g.patterns.clone()));
    let mut book = PatternBook::new();
    for (_, p) in &patterns {
        book.add((**p).clone(), 0);
    }
    for key in emitable_keys(game, grid) {
        book.add_emitable(key);
    }
    let storage = with_storage(game, grid, None, |st| st.contents());
    let plan = match calculate(&book, &storage, &key, amount, CalculationStrategy::ReportMissingItems) {
        Ok(plan) => plan,
        Err(e) => {
            send(game, format!("error {e}"));
            return;
        }
    };
    let mut out = format!("plan {} {}\n{}", plan.bytes, u8::from(!plan.simulation), plan_cpus(grid, plan.bytes));
    for (k, n) in plan.used_items.iter() {
        if n > 0 {
            out.push_str(&format!("u {} {n}\n", wire_id(k)));
        }
    }
    let mut crafted: Vec<(String, i64)> = Vec::new();
    for (pattern, times) in &plan.pattern_times {
        for o in &pattern.outputs {
            let id = wire_id(&o.what);
            match crafted.iter_mut().find(|(i, _)| *i == id) {
                Some((_, n)) => *n += o.amount * times,
                None => crafted.push((id, o.amount * times)),
            }
        }
    }
    for (id, n) in crafted {
        out.push_str(&format!("c {id} {n}\n"));
    }
    // CraftingPlanSummary.fromJob: what emitters make counts as both.
    for (k, n) in plan.emitted_items.iter() {
        if n > 0 {
            out.push_str(&format!("u {} {n}\nc {} {n}\n", wire_id(k), wire_id(k)));
        }
    }
    for (k, n) in plan.missing_items.iter() {
        if n > 0 {
            out.push_str(&format!("m {} {n}\n", wire_id(k)));
        }
    }
    send(game, out);
}

/// AE2's portable cells: the cell type of `portable_item_cell_1k` and the like.
fn portable_type(item: &str) -> Option<CellType> {
    let name = ae2_name(item)?;
    let (size, fluid) = match name.strip_prefix("portable_item_cell_") {
        Some(s) => (s, false),
        None => (name.strip_prefix("portable_fluid_cell_")?, true),
    };
    let tier = StorageTier::ALL.into_iter().find(|t| t.name_prefix == size)?;
    Some(if fluid { CellType::fluid_cell(tier) } else { CellType::item_cell(tier) })
}

fn is_knife(item: &str) -> bool {
    matches!(item, "ae2:certus_quartz_cutting_knife" | "ae2:nether_quartz_cutting_knife")
}

/// `native_use_item`: a portable cell, the network tool (in the air) and the cutting knife open their screens.
fn open_item_screen(game: &mut Game<'_>, stack: &ItemStack) -> bool {
    let kind = if portable_type(&stack.item).is_some() {
        "portable"
    } else if &*stack.item == "ae2:network_tool" {
        "toolbox"
    } else if is_knife(&stack.item) {
        "knife"
    } else {
        return false;
    };
    let Some(player) = game.native_acting_player() else { return false };
    SERVER.with_borrow_mut(|s| {
        s.item_screens.insert(player.uuid, kind.to_owned());
        s.portable_sent.remove(&player.uuid);
        s.item_ops.retain(|(u, _)| *u != player.uuid);
    });
    let _ = game.native_net_send_to_player(player.uuid, NET_UI, format!("open {kind}").as_bytes());
    true
}

/// The network tool on a block of a network: AE2's network status (power, the devices, what they draw).
fn open_network_status(game: &mut Game<'_>, uuid: u128, grid: usize) {
    let lines = SERVER.with_borrow(|s| {
        let g = s.grids.get(grid)?;
        let root = g.nodes.iter().min().copied()?;
        let stored = s.energy.get(&root).copied().unwrap_or(0.0);
        let cap = grid_capacity(g, &s.devices);
        let mut counts: Vec<(String, i64)> = Vec::new();
        for p in &g.nodes {
            let Some((id, _)) = s.devices.get(p) else { continue };
            match counts.iter_mut().find(|(i, _)| i == id) {
                Some((_, n)) => *n += 1,
                None => counts.push((id.clone(), 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut out = vec![
            format!("t Stored Power: {} AE", readable_number::format_i64(stored as i64, 6)),
            format!("t Max Power: {} AE", readable_number::format_i64(cap as i64, 6)),
            format!("c {}", g.channels),
            format!("t Devices: {}", g.nodes.len()),
            format!("t Power Usage: {:.1} AE/t", g.idle),
            format!("t Status: {}", if g.online { "§2online" } else { "§4offline" }),
        ];
        out.extend(counts.into_iter().map(|(id, n)| format!("n {id} {n}")));
        Some(out)
    });
    let Some(lines) = lines else { return };
    SERVER.with_borrow_mut(|s| {
        s.item_screens.insert(uuid, "status".to_owned());
    });
    let _ = game.native_net_send_to_player(uuid, NET_UI, format!("open status\n{}", lines.join("\n")).as_bytes());
}

/// The held item's inventory tick while its screen is open: the stacks the screen asked to put in come out of
/// their slots, and the held item (the selected slot) does what was asked.
fn item_screen_tick(game: &mut Game<'_>, slot: i32) {
    let Some(player) = game.native_acting_player() else { return };
    let uuid = player.uuid;
    let Some(kind) = SERVER.with_borrow(|s| s.item_screens.get(&uuid).cloned()) else { return };
    let selected = game.selected as i32;
    // `put <slot>`: that slot's stack, taken out now, goes into the held item at its tick.
    let put = SERVER.with_borrow_mut(|s| {
        let want = format!("put {slot}");
        let i = s.item_ops.iter().position(|(u, m)| *u == uuid && *m == want)?;
        Some(s.item_ops.remove(i))
    });
    if put.is_some() && slot != selected {
        let fits = |id: &str| match kind.as_str() {
            "toolbox" => ae2_name(id).is_some_and(|c| c.ends_with("_card")),
            "portable" => cell_type(id).is_none() && portable_type(id).is_none(),
            _ => false,
        };
        if let Some(s) = game.native_carried_stack(slot)
            && s.count > 0
            && fits(&s.item)
        {
            let taken = s.clone();
            s.count = 0;
            SERVER.with_borrow_mut(|sv| sv.item_carry.entry(uuid).or_default().push(taken));
        }
        return;
    }
    if slot != selected {
        return;
    }
    let Some(held) = game.native_carried_stack(slot).filter(|s| s.count > 0).map(|s| s.clone()) else {
        close_item_screen(game, uuid);
        return;
    };
    match kind.as_str() {
        "portable" if portable_type(&held.item).is_some() => portable_tick(game, uuid, slot, held),
        "toolbox" if &*held.item == "ae2:network_tool" => toolbox_tick(game, uuid, slot, held),
        "knife" if is_knife(&held.item) => knife_tick(game, uuid, slot, held),
        "status" => {}
        _ => close_item_screen(game, uuid),
    }
}

fn close_item_screen(game: &mut Game<'_>, uuid: u128) {
    let carry = SERVER.with_borrow_mut(|s| {
        s.item_screens.remove(&uuid);
        s.portable_sent.remove(&uuid);
        s.item_carry.remove(&uuid).unwrap_or_default()
    });
    for st in carry {
        let _ = game.native_give_stack(uuid, st);
    }
    let _ = game.native_net_send_to_player(uuid, NET_UI, b"close");
}

fn take_ops(uuid: u128) -> Vec<String> {
    SERVER.with_borrow_mut(|s| {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut s.item_ops).into_iter().partition(|(u, m)| *u == uuid && !m.starts_with("put "));
        s.item_ops = rest;
        mine.into_iter().map(|(_, m)| m).collect()
    })
}

/// A portable cell: what was put in goes in, what was taken comes out to the inventory, and the screen gets the
/// cell's contents when they change.
fn portable_tick(game: &mut Game<'_>, uuid: u128, slot: i32, mut cell: ItemStack) {
    let Some(ty) = portable_type(&cell.item) else { return };
    let config = cell_config(&cell);
    let mut inv = BasicCellInventory::new(ty, &config, cell_fuzzy(&cell), cell_upgrades(&cell), &read_cell(&cell));
    // AE2's powered storage (StorageHelper.poweredInsert/Extraction): an AE an item moved, from the cell's
    // battery, and its idle drain (0.5 AE a tick) while it is open.
    let start_power = portable_power(&cell);
    let mut power = start_power;
    let carry = SERVER.with_borrow_mut(|s| s.item_carry.remove(&uuid)).unwrap_or_default();
    for st in carry {
        let key = key_of(game, &st);
        let can = i64::from(st.count).min(power.floor() as i64);
        let stored = inv.insert(&key, can, Actionable::Modulate);
        power -= stored as f64;
        if stored < i64::from(st.count) {
            let mut left = st.clone();
            left.count -= stored as u32;
            let _ = game.native_give_stack(uuid, left);
        }
    }
    for op in take_ops(uuid) {
        let w: Vec<&str> = op.split(' ').collect();
        let ["take", i, right, _shift] = w.as_slice() else { continue };
        let Some(key) = i.parse::<usize>().ok().and_then(|i| SERVER.with_borrow(|s| s.portable_sent.get(&uuid).and_then(|(keys, _)| keys.get(i).cloned()))) else { continue };
        if key.key_type() == super::port_ae2_core::KeyType::Fluids {
            continue;
        }
        let most = i64::from(max_stack(game, key.id()));
        let have = inv.extract(&key, most, Actionable::Simulate);
        let want = (if *right == "1" { (have + 1) / 2 } else { have }).min(power.floor() as i64);
        let got = inv.extract(&key, want, Actionable::Modulate);
        power -= got as f64;
        if got > 0
            && let Some(left) = game.native_give_stack(uuid, stack_of(&key, got as u32))
        {
            let back = key_of(game, &left);
            inv.insert(&back, i64::from(left.count), Actionable::Modulate);
        }
    }
    let changed = inv.persist();
    if let Some(contents) = &changed {
        write_cell(&mut cell, contents);
    }
    // The battery as the screen leaves it (written each second, or with a change, to spare the item's updates).
    let tick = SERVER.with_borrow(|s| s.tick);
    if tick % 20 == 0 {
        // The idle drain of the second gone by.
        power = (power - 0.5 * 20.0).max(0.0);
    }
    if changed.is_some() || (power != start_power && tick % 20 == 0) {
        set_portable_power(&mut cell, power);
        if let Some(s) = game.native_carried_stack(slot) {
            *s = cell.clone();
        }
    }
    // The screen's list: every entry by a stable index (gone ones -1), sent when it changes.
    let mut list = super::port_ae2_core::KeyCounter::new();
    inv.available_stacks(&mut list);
    let now: Vec<(AeKey, i64)> = list.iter().map(|(k, n)| (k.clone(), n)).filter(|(_, n)| *n > 0).collect();
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for (k, n) in &now {
            key_to_string(k).hash(&mut h);
            n.hash(&mut h);
        }
        h.finish()
    };
    let (old, last) = SERVER.with_borrow(|s| s.portable_sent.get(&uuid).cloned()).map_or((Vec::new(), None), |(k, h)| (k, Some(h)));
    if last == Some(hash) {
        return;
    }
    let mut keys = old.clone();
    let mut text = if last.is_none() { "#reset\n".to_owned() } else { String::new() };
    for (i, k) in keys.iter().enumerate() {
        if !now.iter().any(|(n, _)| n == k) {
            text.push_str(&format!("{i} -1\n"));
        }
    }
    for (k, n) in &now {
        match keys.iter().position(|o| o == k) {
            Some(i) => text.push_str(&format!("{i} {n}\n")),
            None => {
                text.push_str(&format!("{} {} {n}\n", keys.len(), wire_id(k)));
                keys.push(k.clone());
            }
        }
    }
    if game.native_net_send_to_player(uuid, NET_ITEMS, text.as_bytes()).is_ok() {
        SERVER.with_borrow_mut(|s| s.portable_sent.insert(uuid, (keys, hash)));
    }
}

/// The network tool's toolbox: nine stacks of upgrade cards on the tool (AE2's `NetworkToolMenu`).
fn toolbox_of(stack: &ItemStack) -> Vec<(String, i64)> {
    let mut out: Vec<(String, i64)> = vec![(String::new(), 0); 9];
    let Some(Tag::Compound(data)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        return out;
    };
    if let Some(Tag::String(s)) = data.get(&java("ae2:toolbox")) {
        for (i, e) in s.to_string().unwrap_or_default().split(';').enumerate().take(9) {
            if let Some((id, n)) = e.split_once(' ') {
                out[i] = (id.to_owned(), n.parse().unwrap_or(0));
            }
        }
    }
    out
}

fn set_custom(stack: &mut ItemStack, key: &str, value: Tag) {
    let extra = stack.extra.get_or_insert_with(Default::default);
    let mut data = match extra.other.iter().position(|(k, _)| k == CELL_DATA) {
        Some(i) => match extra.other.remove(i).1 {
            Tag::Compound(c) => c,
            _ => Compound::new(),
        },
        None => Compound::new(),
    };
    data.insert(java(key), value);
    extra.other.push((CELL_DATA.to_owned(), Tag::Compound(data)));
    extra.other.sort_by(|a, b| a.0.cmp(&b.0));
}

fn toolbox_tick(game: &mut Game<'_>, uuid: u128, slot: i32, mut tool: ItemStack) {
    let mut boxed = toolbox_of(&tool);
    let before = boxed.clone();
    for st in SERVER.with_borrow_mut(|s| s.item_carry.remove(&uuid)).unwrap_or_default() {
        let mut left = i64::from(st.count);
        for (id, n) in boxed.iter_mut() {
            if left > 0 && *id == *st.item && *n < 64 {
                let add = left.min(64 - *n);
                *n += add;
                left -= add;
            }
        }
        for (id, n) in boxed.iter_mut() {
            if left > 0 && *n == 0 {
                *id = st.item.to_string();
                let add = left.min(64);
                *n = add;
                left -= add;
            }
        }
        if left > 0 {
            let _ = game.native_give_stack(uuid, ItemStack::new(&st.item, left as u32));
        }
    }
    for op in take_ops(uuid) {
        if let Some(i) = op.strip_prefix("take ").and_then(|i| i.parse::<usize>().ok())
            && let Some((id, n)) = boxed.get_mut(i)
            && *n > 0
        {
            let stack = ItemStack::new(id, *n as u32);
            *n = 0;
            id.clear();
            if let Some(left) = game.native_give_stack(uuid, stack) {
                boxed[i] = (left.item.to_string(), i64::from(left.count));
            }
        }
    }
    let text: Vec<String> = boxed.iter().map(|(id, n)| if *n > 0 { format!("{id} {n}") } else { String::new() }).collect();
    if boxed != before {
        set_custom(&mut tool, "ae2:toolbox", Tag::String(java(&text.join(";"))));
        if let Some(s) = game.native_carried_stack(slot) {
            *s = tool;
        }
    }
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        h.finish()
    };
    if SERVER.with_borrow(|s| s.portable_sent.get(&uuid).map(|(_, h)| *h)) != Some(hash) {
        let _ = game.native_net_send_to_player(uuid, NET_UI, format!("toolbox {}", text.join(";")).as_bytes());
        SERVER.with_borrow_mut(|s| s.portable_sent.insert(uuid, (Vec::new(), hash)));
    }
}

/// NetworkToolItem.findNetworkToolInv: the first network tool the player carries.
fn network_tool_slot(game: &mut Game<'_>) -> Option<i32> {
    (0..36).find(|&i| game.native_carried_stack(i).is_some_and(|s| s.count > 0 && &*s.item == "ae2:network_tool"))
}

fn toolbelt_parse(text: &str) -> Vec<(String, i64)> {
    let mut out: Vec<(String, i64)> = text.split(';').map(|e| e.split_once(' ').map(|(id, n)| (id.to_owned(), n.parse().unwrap_or(0))).unwrap_or_default()).collect();
    out.resize(9, (String::new(), 0));
    out
}

/// A slot of ToolboxMenu (RestrictedInputSlot of UPGRADES): the cursor's cards in or the slot's out, as any slot does.
/// A menu's button has no player's inventory at hand, and the tool can't change under an open menu: it is written when
/// the screen closes (`toolbelt_stream`).
fn toolbelt_click(game: &mut Game<'_>, pos: Pos, cell: usize, right: bool, carried: &mut Option<ItemStack>) {
    let Some(uuid) = menu_player(pos) else { return };
    let Some(text) = SERVER.with_borrow(|s| s.toolbelt_sent.get(&uuid).cloned()).filter(|t| t != "none") else { return };
    let mut boxed = toolbelt_parse(&text);
    let Some((id, n)) = boxed.get(cell).cloned() else { return };
    match carried.take() {
        Some(mut hand) if ae2_name(&hand.item).is_some_and(|c| c.ends_with("_card")) => {
            if n > 0 && id != *hand.item {
                if right {
                    *carried = Some(hand);
                    return;
                }
                // Another card: they change places.
                boxed[cell] = (hand.item.to_string(), i64::from(hand.count));
                *carried = Some(ItemStack::new(&id, n as u32));
            } else {
                let room = 64 - n;
                let add = (if right { 1 } else { i64::from(hand.count) }).min(room);
                boxed[cell] = (hand.item.to_string(), n + add);
                hand.count -= add as u32;
                *carried = (hand.count > 0).then_some(hand);
            }
        }
        Some(hand) => {
            *carried = Some(hand);
            return;
        }
        None if n > 0 => {
            let take = if right { (n + 1) / 2 } else { n };
            *carried = Some(ItemStack::new(&id, take as u32));
            boxed[cell] = if n > take { (id, n - take) } else { (String::new(), 0) };
        }
        None => return,
    }
    let text = boxed.iter().map(|(id, n)| if *n > 0 { format!("{id} {n}") } else { String::new() }).collect::<Vec<_>>().join(";");
    let _ = game.native_net_send_to_player(uuid, NET_UI, format!("toolbelt {text}").as_bytes());
    SERVER.with_borrow_mut(|s| {
        s.toolbelt_sent.insert(uuid, text.clone());
        s.toolbelt_writes.insert(uuid, text);
    });
}

/// Each tick, for a viewer: the tool written with what the toolbelt's clicks did, and what their client should show
/// of it, when that changed.
fn toolbelt_stream(game: &mut Game<'_>, uuid: u128) {
    let tick = SERVER.with_borrow_mut(|s| {
        let tick = s.tick;
        if !s.viewers.contains_key(&uuid) || s.toolbelt_seen.insert(uuid, tick) == Some(tick) {
            return None;
        }
        Some(tick)
    });
    if tick.is_none() {
        return;
    }
    // While the screen's clicks wait to be written, the client's list is the truth.
    if SERVER.with_borrow(|s| s.toolbelt_writes.contains_key(&uuid)) && !SERVER.with_borrow_mut(|s| s.toolbelt_flush.remove(&uuid)) {
        return;
    }
    let slot = network_tool_slot(game);
    if let Some(write) = SERVER.with_borrow_mut(|s| s.toolbelt_writes.remove(&uuid))
        && let Some(slot) = slot
        && let Some(tool) = game.native_carried_stack(slot)
    {
        set_custom(tool, "ae2:toolbox", Tag::String(java(&write)));
    }
    let text = match slot.and_then(|slot| game.native_carried_stack(slot).map(|s| s.clone())) {
        Some(tool) => toolbox_of(&tool).iter().map(|(id, n)| if *n > 0 { format!("{id} {n}") } else { String::new() }).collect::<Vec<_>>().join(";"),
        None => "none".to_owned(),
    };
    if SERVER.with_borrow(|s| s.toolbelt_sent.get(&uuid) == Some(&text)) {
        return;
    }
    let _ = game.native_net_send_to_player(uuid, NET_UI, format!("toolbelt {text}").as_bytes());
    SERVER.with_borrow_mut(|s| s.toolbelt_sent.insert(uuid, text));
}

/// The cutting knife (AE2's `QuartzKnifeMenu`): an iron ingot and the name typed make a Name Press of that name;
/// the knife wears a little each cut.
fn knife_tick(game: &mut Game<'_>, uuid: u128, slot: i32, mut knife: ItemStack) {
    for op in take_ops(uuid) {
        let Some(name) = op.strip_prefix("knife ").map(str::trim).filter(|n| !n.is_empty()) else { continue };
        if game.native_take_items("minecraft:iron_ingot", 1) != 1 {
            continue;
        }
        let mut press = ItemStack::new("ae2:name_press", 1);
        set_custom(&mut press, "ae2:name", Tag::String(java(name)));
        press.extra.get_or_insert_with(Default::default).custom_name = Some(Tag::String(java(name)));
        let _ = game.native_give_stack(uuid, press);
        let max = game.res.items().get(&knife.item).map_or(0, |i| i.max_damage);
        knife.damage += 1;
        if max > 0 && knife.damage >= max {
            knife.count = 0;
        }
        if let Some(s) = game.native_carried_stack(slot) {
            *s = knife.clone();
        }
        if knife.count == 0 {
            close_item_screen(game, uuid);
            return;
        }
    }
}

// ---- The cell workbench (a cell's partition), the condenser, the quantum network bridge.

/// A cell's partition (AE2's config inventory on the cell): the keys it accepts.
fn cell_config(stack: &ItemStack) -> Vec<AeKey> {
    let Some(Tag::Compound(data)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        return Vec::new();
    };
    match data.get(&java("ae2:config")) {
        Some(Tag::List(keys)) => keys
            .iter()
            .filter_map(|k| match k {
                Tag::String(s) => s.to_string().ok().and_then(|s| key_from_string(&s)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The workbench's 63 config cells, as the cell has them ("" empty).
fn workbench_cells(stack: Option<&ItemStack>) -> Vec<String> {
    let mut cells = vec![String::new(); 63];
    if let Some(st) = stack {
        let Some(Tag::Compound(data)) = st.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
            return cells;
        };
        if let Some(Tag::List(keys)) = data.get(&java("ae2:config")) {
            for (i, k) in keys.iter().enumerate().take(63) {
                if let Tag::String(s) = k {
                    cells[i] = s.to_string().unwrap_or_default();
                }
            }
        }
    }
    cells
}

/// Writes the config cells onto the cell. Its id goes, so a drive reads it again with the new partition.
fn set_workbench_cells(stack: &mut ItemStack, cells: &[String]) {
    let mut last = cells.len();
    while last > 0 && cells[last - 1].is_empty() {
        last -= 1;
    }
    set_custom(stack, "ae2:config", Tag::List(cells[..last].iter().map(|s| Tag::String(java(s))).collect()));
    if let Some(extra) = stack.extra.as_mut()
        && let Some((_, Tag::Compound(data))) = extra.other.iter_mut().find(|(k, _)| k == CELL_DATA)
    {
        data.remove(&java("ae2:cell_id"));
        data.remove(&java("ae2:cell_ver"));
    }
}

const PARTITION: i32 = 1;
const CLEAR_CONFIG: i32 = 2;
const WB_FUZZY: i32 = 3;
const WB_COPY: i32 = 4;
/// The cell's upgrade slots on the workbench (BasicStorageCell.getUpgrades: four for an item cell; a fluid
/// cell takes three of them).
const WB_UPGRADES: usize = 4;
/// AE2's fuzzy mode icons (Icon.FUZZY_IGNORE, FUZZY_PERCENT_99 .. 25) and their tooltips.
const ICON_FUZZY: [[f32; 2]; 5] = [[64.0, 96.0], [48.0, 96.0], [32.0, 96.0], [16.0, 96.0], [0.0, 96.0]];
const FUZZY_TIPS: [&str; 5] = [
    "Fuzzy Comparison Ignores all.",
    "Splits damaged / undamaged at 99%.",
    "Splits damaged / undamaged at 75%.",
    "Splits damaged / undamaged at 50%.",
    "Splits damaged / undamaged at 25%.",
];

/// AE2's CellWorkbenchScreen left toolbar, packed top down: fuzzy mode (only with a fuzzy card), partition,
/// clear, copy mode.
fn wb_toolbar(fuzzy: bool) -> Vec<(i32, [f32; 2])> {
    let buttons: &[i32] = if fuzzy { &[WB_FUZZY, PARTITION, CLEAR_CONFIG, WB_COPY] } else { &[PARTITION, CLEAR_CONFIG, WB_COPY] };
    buttons.iter().enumerate().map(|(i, b)| (*b, [-18.0, 8.0 + 20.0 * i as f32])).collect()
}

/// The upgrade cards a storage cell takes (its UpgradeInventory) and how many: an item cell four, a fluid three.
fn cell_upgrade_slots(stack: &ItemStack) -> usize {
    if ae2_name(&stack.item) == Some("view_cell") {
        // ViewCellItem.getUpgrades: two (fuzzy, inverter).
        return 2;
    }
    match cell_type(&stack.item) {
        Some(t) if t.key_type == super::port_ae2_core::KeyType::Fluids => 3,
        Some(_) => 4,
        None => 0,
    }
}

fn is_cell_card(id: &str) -> bool {
    matches!(id, "ae2:fuzzy_card" | "ae2:inverter_card" | "ae2:equal_distribution_card" | "ae2:void_card")
}

fn custom_tag<'a>(stack: &'a ItemStack, key: &str) -> Option<&'a Tag> {
    let Some(Tag::Compound(data)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        return None;
    };
    data.get(&java(key))
}

/// The cell's upgrade cards, as the workbench put them in (`ae2:upgrades`).
fn cell_cards(stack: &ItemStack) -> Vec<String> {
    match custom_tag(stack, "ae2:upgrades") {
        Some(Tag::List(cards)) => cards.iter().filter_map(|t| if let Tag::String(s) = t { s.to_string().ok() } else { None }).collect(),
        _ => Vec::new(),
    }
}

/// What the cell's cards do (CellUpgrades: inverter, fuzzy, equal distribution, void).
fn cell_upgrades(stack: &ItemStack) -> CellUpgrades {
    let cards = cell_cards(stack);
    let has = |c: &str| cards.iter().any(|x| x == c);
    CellUpgrades {
        inverter: has("ae2:inverter_card"),
        fuzzy: has("ae2:fuzzy_card"),
        equal_distribution: has("ae2:equal_distribution_card"),
        void: has("ae2:void_card"),
    }
}

/// The cell's fuzzy mode (`ae2:fuzzy`, AE2's FuzzyMode name).
fn cell_fuzzy(stack: &ItemStack) -> FuzzyMode {
    match custom_tag(stack, "ae2:fuzzy") {
        Some(Tag::String(s)) => FuzzyMode::from_saved(&s.to_string().unwrap_or_default()),
        _ => FuzzyMode::IgnoreAll,
    }
}

/// A cell changed what it does: its id goes, so a drive mounts it again.
fn cell_changed(stack: &mut ItemStack) {
    if let Some(extra) = stack.extra.as_mut()
        && let Some((_, Tag::Compound(data))) = extra.other.iter_mut().find(|(k, _)| k == CELL_DATA)
    {
        data.remove(&java("ae2:cell_id"));
        data.remove(&java("ae2:cell_ver"));
    }
}

/// The workbench's own config pane (CellWorkbenchBlockEntity.config): 63 keys ("" empty).
fn wb_config(entity: &ModBlockEntity) -> Vec<String> {
    let mut cells = vec![String::new(); 63];
    if let Some(Tag::List(keys)) = entity.nbt.get(&java("config")) {
        for (i, k) in keys.iter().enumerate().take(63) {
            if let Tag::String(s) = k {
                cells[i] = s.to_string().unwrap_or_default();
            }
        }
    }
    cells
}

fn set_wb_config(entity: &mut ModBlockEntity, cells: &[String]) {
    entity.nbt.insert(java("config"), Tag::List(cells.iter().map(|s| Tag::String(java(s))).collect()));
}

/// The cards in the workbench's upgrade slots, in order ("" empty).
fn wb_cards(entity: &ModBlockEntity) -> Vec<String> {
    (1..=WB_UPGRADES).map(|i| entity.items.get(i).cloned().flatten().map_or(String::new(), |s| s.item.to_string())).collect()
}

/// CellWorkbenchBlockEntity.onChangeInventory and the menu's upgrade slots: a cell put in brings its partition
/// (or takes the pane's, when it has none) and its cards; one taken out takes its cards and, in "clear on
/// remove" copy mode, clears the pane; cards put in or taken out while it's there go onto the cell.
fn workbench_tick(entity: &mut ModBlockEntity) {
    if entity.items.len() < 1 + WB_UPGRADES {
        entity.items.resize(1 + WB_UPGRADES, None);
    }
    let had = nbt_i64(&entity.nbt, "had") != 0;
    let saved = match entity.nbt.get(&java("cards")) {
        Some(Tag::String(s)) => s.to_string().unwrap_or_default(),
        _ => String::new(),
    };
    let cell = entity.items[0].clone().filter(|s| cell_type(&s.item).is_some() || is_creative_cell(&s.item) || ae2_name(&s.item) == Some("view_cell"));
    let now = wb_cards(entity).join(",");
    match (had, cell) {
        (false, Some(mut cell)) => {
            let own = workbench_cells(Some(&cell));
            if own.iter().any(|k| !k.is_empty()) {
                set_wb_config(entity, &own);
            } else {
                let pane = wb_config(entity);
                if pane.iter().any(|k| !k.is_empty()) {
                    set_workbench_cells(&mut cell, &pane);
                }
            }
            // The cell's cards into the slots (cards left in the slots beforehand stay where there's no room), and
            // what the slots hold now onto the cell.
            let mut cards = cell_cards(&cell).into_iter();
            for i in 1..=WB_UPGRADES {
                if entity.items[i].is_none()
                    && let Some(c) = cards.next()
                {
                    entity.items[i] = Some(ItemStack::new(&c, 1));
                }
            }
            let room = cell_upgrade_slots(&cell);
            let on: Vec<Tag> = wb_cards(entity).iter().take(room).filter(|c| is_cell_card(c)).map(|c| Tag::String(java(c))).collect();
            if on.len() != cell_cards(&cell).len() {
                set_custom(&mut cell, "ae2:upgrades", Tag::List(on));
                cell_changed(&mut cell);
            }
            entity.items[0] = Some(cell);
            set_nbt_i64(&mut entity.nbt, "had", 1);
            entity.nbt.insert(java("cards"), Tag::String(java(&wb_cards(entity).join(","))));
        }
        (true, None) => {
            // The cards went with the cell, unless they changed since they were last put on it.
            if now == saved {
                for i in 1..=WB_UPGRADES {
                    entity.items[i] = None;
                }
            }
            if nbt_i64(&entity.nbt, "copy") == 0 {
                set_wb_config(entity, &vec![String::new(); 63]);
            }
            set_nbt_i64(&mut entity.nbt, "had", 0);
            entity.nbt.insert(java("cards"), Tag::String(java("")));
        }
        (true, Some(mut cell)) if now != saved => {
            let room = cell_upgrade_slots(&cell);
            let cards: Vec<Tag> = wb_cards(entity).iter().take(room).filter(|c| is_cell_card(c)).map(|c| Tag::String(java(c))).collect();
            set_custom(&mut cell, "ae2:upgrades", Tag::List(cards));
            cell_changed(&mut cell);
            entity.items[0] = Some(cell);
            entity.nbt.insert(java("cards"), Tag::String(java(&now)));
        }
        _ => {}
    }
    // The screen: copy mode, the fuzzy mode (-1 with no fuzzy card), the slots the cell takes.
    let cell = entity.items[0].clone();
    entity.data.resize(3, 0);
    entity.data[0] = nbt_i64(&entity.nbt, "copy") as i32;
    entity.data[1] = match &cell {
        Some(c) if cell_upgrades(c).fuzzy => FuzzyMode::VALUES.iter().position(|m| *m == cell_fuzzy(c)).unwrap_or(0) as i32,
        _ => -1,
    };
    entity.data[2] = cell.as_ref().map_or(0, cell_upgrade_slots) as i32;
}

fn workbench_button(game: &Game<'_>, entity: &mut ModBlockEntity, id: i32, carried: &Option<ItemStack>) {
    if id == WB_COPY {
        let m = nbt_i64(&entity.nbt, "copy");
        set_nbt_i64(&mut entity.nbt, "copy", 1 - m);
        return;
    }
    let cell = entity.items.first().cloned().flatten().filter(|s| cell_type(&s.item).is_some() || portable_type(&s.item).is_some() || is_creative_cell(&s.item) || ae2_name(&s.item) == Some("view_cell"));
    if id == WB_FUZZY {
        if let Some(mut c) = cell {
            let next = FuzzyMode::VALUES[(FuzzyMode::VALUES.iter().position(|m| *m == cell_fuzzy(&c)).unwrap_or(0) + 1) % 5];
            set_custom(&mut c, "ae2:fuzzy", Tag::String(java(next.java_name())));
            cell_changed(&mut c);
            entity.items[0] = Some(c);
        }
        return;
    }
    // The pane works without a cell only in "keep on remove" mode (CellWorkbenchMenu.isPartitionSlotEnabled).
    if cell.is_none() && nbt_i64(&entity.nbt, "copy") == 0 {
        return;
    }
    let mut cells = wb_config(entity);
    if id >= GHOST {
        let i = ((id - GHOST) >> 3) as usize;
        if i < 63 {
            cells[i] = carried.as_ref().map_or(String::new(), |st| {
                let mut one = st.clone();
                one.count = 1;
                key_to_string(&key_of(game, &one))
            });
        }
    } else if id == PARTITION {
        // AE2's "partition storage": the config becomes what the cell holds.
        let Some(c) = cell.as_ref() else { return };
        let contents = read_cell(c);
        cells = contents.keys.iter().map(key_to_string).take(63).collect();
        cells.resize(63, String::new());
    } else if id == CLEAR_CONFIG {
        cells = vec![String::new(); 63];
    } else {
        return;
    }
    set_wb_config(entity, &cells);
    if let Some(mut c) = cell {
        set_workbench_cells(&mut c, &cells);
        entity.items[0] = Some(c);
    }
}

/// AE2's condenser: everything put in is destroyed into power; with a storage component, the power makes
/// matter balls (256) or singularities (256 000).
fn condenser_tick(entity: &mut ModBlockEntity) {
    entity.items.resize(3, None);
    entity.data.resize(4, 0);
    let mode = nbt_i64(&entity.nbt, "mode");
    let storage = entity.items[2].as_ref().map_or(0, |s| match &*s.item {
        "ae2:cell_component_1k" => 1024 * 8,
        "ae2:cell_component_4k" => 4096 * 8,
        "ae2:cell_component_16k" => 16384 * 8,
        "ae2:cell_component_64k" => 65536 * 8,
        "ae2:cell_component_256k" => 262_144 * 8,
        _ => 0,
    });
    let (required, output) = match mode {
        1 => (256, "ae2:matter_ball"),
        2 => (256_000, "ae2:singularity"),
        _ => (0, ""),
    };
    let mut power = nbt_i64(&entity.nbt, "power");
    if let Some(input) = entity.items[0].take() {
        power += i64::from(input.count);
    }
    power = power.clamp(0, storage);
    while required > 0 && power >= required {
        let out = &mut entity.items[1];
        match out {
            Some(s) if &*s.item == output && s.count < 64 => s.count += 1,
            None => *out = Some(ItemStack::new(output, 1)),
            _ => break,
        }
        power -= required;
    }
    set_nbt_i64(&mut entity.nbt, "power", power);
    set_nbt_i64(&mut entity.nbt, "storage", storage);
    entity.data[0] = power.min(i64::from(i32::MAX)) as i32;
    entity.data[1] = storage as i32;
    entity.data[2] = mode as i32;
    entity.data[3] = required as i32;
}

/// An entangled singularity's frequency (both of a pair share it).
fn singularity_freq(stack: &ItemStack) -> Option<i64> {
    if &*stack.item != "ae2:quantum_entangled_singularity" {
        return None;
    }
    let Some(Tag::Compound(data)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        return None;
    };
    match data.get(&java("ae2:freq")) {
        Some(Tag::Long(f)) => Some(*f),
        _ => None,
    }
}

fn new_frequency() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as i64 & 0x3FFF_FFFF_FFFF_FFFF).max(1)
}

/// A pair made without a frequency gets one (AE2 gives it as the explosion makes them).
fn give_frequency(stack: &mut ItemStack) {
    if &*stack.item == "ae2:quantum_entangled_singularity" && singularity_freq(stack).is_none() {
        set_custom(stack, "ae2:freq", Tag::Long(new_frequency()));
    }
}

/// AE2's quantum bridge: the link chamber in the middle of a 3x3 of rings, in any plane.
fn qnb_formed(game: &Game<'_>, link: Pos) -> bool {
    [(1usize, 2usize), (0, 2), (0, 1)].iter().any(|&(i, j)| {
        (-1..=1).all(|a| {
            (-1..=1).all(|b| {
                if a == 0 && b == 0 {
                    return true;
                }
                let mut q = link;
                q[i] += a;
                q[j] += b;
                block_id(game, q) == "ae2:quantum_ring"
            })
        })
    })
}

/// `native_explosion_detonate`: a singularity and ender dust caught in an explosion become a pair of entangled
/// singularities (AE2's transform). The items would burn in the blast, so the pair goes to the nearest player.
pub fn explosion(game: &mut Game<'_>, dimension: &str, center: [f64; 3]) {
    let lo = [center[0] - 4.0, center[1] - 4.0, center[2] - 4.0];
    let hi = [center[0] + 4.0, center[1] + 4.0, center[2] + 4.0];
    let items = game.native_item_entities(dimension, lo, hi);
    let singularity = items.iter().find(|e| &*e.stack.item == "ae2:singularity");
    let dust = items.iter().find(|e| &*e.stack.item == "ae2:ender_dust");
    let (Some(s), Some(d)) = (singularity, dust) else { return };
    let (sid, mut s_stack, did, mut d_stack) = (s.id, s.stack.clone(), d.id, d.stack.clone());
    let pairs = s_stack.count.min(d_stack.count);
    s_stack.count -= pairs;
    d_stack.count -= pairs;
    game.native_set_item_entity(dimension, sid, s_stack);
    game.native_set_item_entity(dimension, did, d_stack);
    let nearest = game
        .native_server_players()
        .into_iter()
        .min_by(|a, b| {
            let da = (a.position[0] - center[0]).powi(2) + (a.position[1] - center[1]).powi(2) + (a.position[2] - center[2]).powi(2);
            let db = (b.position[0] - center[0]).powi(2) + (b.position[1] - center[1]).powi(2) + (b.position[2] - center[2]).powi(2);
            da.total_cmp(&db)
        })
        .map(|p| p.uuid);
    if let Some(uuid) = nearest {
        for _ in 0..pairs {
            let mut pair = ItemStack::new("ae2:quantum_entangled_singularity", 2);
            set_custom(&mut pair, "ae2:freq", Tag::Long(new_frequency()));
            let _ = game.native_give_stack(uuid, pair);
        }
    }
}

// ---- The pattern encoder's smithing table and stonecutting modes.

const SET_SMITHING: i32 = 5;
const SET_STONECUTTING: i32 = 6;
/// A stonecutting result picked: this plus its index.
const STONECUT_PICK: i32 = 1 << 26;

/// The encoder's ghost cells by mode: crafting and processing as before, smithing's template, base and
/// addition, stonecutting's one input (AE2's encoding/*.json).
fn ghost_cell_mode(i: usize, mode: i32) -> [f32; 2] {
    match mode {
        2 => [21.0 + 18.0 * i as f32, 111.0],
        3 => [21.0, 110.0],
        m => ghost_cell(i, m == 0),
    }
}

fn mode_cells(mode: i32) -> usize {
    match mode {
        0 => 9,
        1 => 12,
        2 => 3,
        _ => 1,
    }
}

/// What a smithing table makes of the encoder's three cells.
fn smithing_preview(game: &Game<'_>, cells: &[Option<(String, i64)>]) -> Option<ItemStack> {
    let st = |i: usize| cells.get(i).cloned().flatten().map(|(id, _)| ItemStack::new(&id, 1));
    let (t, b, a) = (st(0)?, st(1)?, st(2)?);
    let data = game.res.stations();
    data.recipes.smithing_result(&data.enchantments, Some(&t), Some(&b), Some(&a))
}

/// What a stonecutter makes of the encoder's input, in its order.
fn stonecut_options(game: &Game<'_>, cells: &[Option<(String, i64)>]) -> Vec<ItemStack> {
    let Some(Some((id, _))) = cells.first() else { return Vec::new() };
    let input = ItemStack::new(id, 1);
    game.res.stations().recipes.stonecutter(&input).into_iter().map(|r| r.result.clone()).collect()
}

/// Encodes the smithing or stonecutting pattern (crafted by molecular assemblers, as crafting patterns are).
fn encode_special(game: &mut Game<'_>, entity: &mut ModBlockEntity, mode: i32) {
    let cells = ghosts_of(entity);
    let (inputs, result, item): (Vec<String>, Option<ItemStack>, &str) = if mode == 2 {
        (cells.iter().take(3).flatten().map(|(id, _)| id.clone()).collect(), smithing_preview(game, &cells), "ae2:smithing_table_pattern")
    } else {
        let options = stonecut_options(game, &cells);
        let pick = nbt_i64(&entity.nbt, "stonecut") as usize;
        (cells.iter().take(1).flatten().map(|(id, _)| id.clone()).collect(), options.get(pick).or(options.first()).cloned(), "ae2:stonecutting_pattern")
    };
    let Some(result) = result else { return };
    let mut inputs_keys: Vec<(AeKey, i64)> = Vec::new();
    for id in inputs {
        inputs_keys.push((key_of(game, &ItemStack::new(&id, 1)), 1));
    }
    let mut one = result.clone();
    let count = i64::from(one.count.max(1));
    one.count = 1;
    let outputs = vec![(key_of(game, &one), count)];
    write_pattern(game, entity, false, &inputs_keys, &outputs);
    if let Some(p) = entity.items.get_mut(1).and_then(Option::as_mut) {
        p.item = item.into();
    }
}

// ---------------------------------------------------------------------------------------------------------
// Buses as AE2 has them: ghost filters (a row of 9 for import/export, 63 for the storage bus, unlocked by
// capacity cards), the container seen through the face the bus touches (vanilla's WorldlyContainer rules, both
// halves of a double chest, a mod container's faces and outputs), fuzzy / inverter / redstone / crafting cards,
// the export bus' scheduling, and fluids through tanks and cauldrons.

thread_local! {
    /// Mod containers by block state (asked once: Horizon builds this table from every mod's hook).
    static MOD_CONTAINERS: RefCell<Option<std::rc::Rc<HashMap<u32, Arc<ModContainer>>>>> = const { RefCell::new(None) };
    /// Items' stack sizes, for what a storage bus puts in an empty slot.
    static MAX_STACKS: RefCell<HashMap<String, i64>> = RefCell::new(HashMap::new());
}

fn mod_containers(game: &Game<'_>) -> std::rc::Rc<HashMap<u32, Arc<ModContainer>>> {
    MOD_CONTAINERS.with_borrow_mut(|m| {
        m.get_or_insert_with(|| std::rc::Rc::new(game.native_containers().into_iter().collect())).clone()
    })
}

/// A container as a bus sees it through one face.
struct Sided {
    /// Its block entities and their slot counts (a double chest's two halves, in vanilla's order).
    parts: Vec<(Pos, usize)>,
    /// Per slot: may an item go in, may one come out, through this face.
    insert: Vec<bool>,
    extract: Vec<bool>,
    /// A furnace seen from below: its fuel slot gives only buckets. A brewing stand: its ingredient slot gives
    /// only glass bottles... (vanilla's `canTakeItemThroughFace`).
    bucket_slot: Option<usize>,
    bottle_slot: Option<usize>,
}

impl Sided {
    fn locate(&self, mut slot: usize) -> Option<(Pos, usize)> {
        for &(pos, n) in &self.parts {
            if slot < n {
                return Some((pos, slot));
            }
            slot -= n;
        }
        None
    }

    fn can_take(&self, slot: usize, item: &str) -> bool {
        self.extract.get(slot).copied().unwrap_or(false)
            && (self.bucket_slot != Some(slot) || matches!(item, "minecraft:bucket" | "minecraft:water_bucket"))
            && (self.bottle_slot != Some(slot) || item == "minecraft:glass_bottle")
    }
}

fn face_index(face: horizon_core::geometry::Direction) -> usize {
    use horizon_core::geometry::Direction;
    match face {
        Direction::Down => 0,
        Direction::Up => 1,
        Direction::North => 2,
        Direction::South => 3,
        Direction::West => 4,
        Direction::East => 5,
    }
}

fn sided(game: &Game<'_>, target: Pos, face: horizon_core::geometry::Direction) -> Option<Sided> {
    use horizon_core::geometry::Direction;
    let block = block_id(game, target);
    let len_at = |p: Pos| game.world.block_entities.get(&(p[0], p[1], p[2])).map(|be| be.slots().len());
    let n = len_at(target)?;
    let mut parts = vec![(target, n)];
    // A double chest is one container of both halves (ChestBlock.getContainer).
    if matches!(block.as_str(), "minecraft:chest" | "minecraft:trapped_chest")
        && let Some(kind) = game.native_block_property(target, "type").filter(|t| t != "single")
    {
        let cw = match game.native_block_property(target, "facing").as_deref() {
            Some("east") => [0, 0, 1],
            Some("south") => [-1, 0, 0],
            Some("west") => [0, 0, -1],
            _ => [1, 0, 0],
        };
        let d = if kind == "left" { cw } else { [-cw[0], -cw[1], -cw[2]] };
        let other = add(target, d);
        if block_id(game, other) == block
            && let Some(m) = len_at(other)
        {
            if kind == "left" {
                parts.insert(0, (other, m));
            } else {
                parts.push((other, m));
            }
        }
    }
    let total: usize = parts.iter().map(|p| p.1).sum();
    let mut insert = vec![false; total];
    let mut extract = vec![false; total];
    let open = |v: &mut Vec<bool>, slots: &[usize]| {
        for &s in slots {
            if s < v.len() {
                v[s] = true;
            }
        }
    };
    let (mut bucket_slot, mut bottle_slot) = (None, None);
    match block.as_str() {
        "minecraft:furnace" | "minecraft:blast_furnace" | "minecraft:smoker" => {
            let s: &[usize] = match face {
                Direction::Down => &[2, 1],
                Direction::Up => &[0],
                _ => &[1],
            };
            open(&mut insert, s);
            open(&mut extract, s);
            if face == Direction::Down {
                bucket_slot = Some(1);
            }
        }
        "minecraft:brewing_stand" => {
            let s: &[usize] = match face {
                Direction::Up => &[3],
                Direction::Down => &[0, 1, 2, 3],
                _ => &[0, 1, 2, 4],
            };
            open(&mut insert, s);
            open(&mut extract, s);
            bottle_slot = Some(3);
        }
        _ => {
            let state = game.world.block(target[0], target[1], target[2]).0;
            match mod_containers(game).get(&state) {
                Some(c) => {
                    let reach = &c.faces[face_index(face)];
                    let ins: Vec<usize> = reach.iter().copied().filter(|s| c.inputs.contains(s)).collect();
                    let ext: Vec<usize> = reach.iter().copied().filter(|s| c.outputs.contains(s)).collect();
                    open(&mut insert, &ins);
                    open(&mut extract, &ext);
                }
                None => {
                    insert.iter_mut().for_each(|s| *s = true);
                    extract.iter_mut().for_each(|s| *s = true);
                }
            }
        }
    }
    Some(Sided { parts, insert, extract, bucket_slot, bottle_slot })
}

fn sided_slots(game: &Game<'_>, view: &Sided) -> Vec<Option<ItemStack>> {
    view.parts
        .iter()
        .flat_map(|(p, n)| {
            let mut s = game.world.block_entities.get(&(p[0], p[1], p[2])).map(|be| be.slots().to_vec()).unwrap_or_default();
            s.resize(*n, None);
            s
        })
        .collect()
}

/// A bus' filter (AE2's config with its cards): the keys, fuzzy, inverted.
#[derive(Default)]
struct BusFilter {
    keys: Vec<AeKey>,
    fuzzy: bool,
    inverted: bool,
    /// Settings.FUZZY_MODE; Settings.ACCESS (0 both ways, 1 extract only, 2 insert only); Settings.FILTER_ON_EXTRACT
    /// turned off; Settings.STORAGE_FILTER set to report what can't be extracted.
    mode: FuzzyMode,
    access: i64,
    no_extract_filter: bool,
    report_all: bool,
}

impl BusFilter {
    fn allows(&self, key: &AeKey) -> bool {
        if self.keys.is_empty() {
            return true;
        }
        let hit = self.keys.iter().any(|k| k == key || (self.fuzzy && k.fuzzy_equals(key, self.mode)));
        hit != self.inverted
    }
}

/// The filter slots a bus has unlocked: import/export 1, 5, 9 (capacity cards), the storage bus 18 + 9 a card.
fn bus_slots(entity: &ModBlockEntity) -> usize {
    let cap = cards(entity, 0, "ae2:capacity_card");
    match ae2_name(&entity.id) {
        Some("storage_bus" | "formation_plane") => (18 + 9 * cap).min(63),
        _ => [1, 5, 9][cap.min(2)],
    }
}

fn bus_cells(entity: &ModBlockEntity) -> usize {
    if matches!(entity.id.as_str(), "ae2:storage_bus" | "ae2:formation_plane") { 63 } else { 9 }
}

/// A ghost cell's key: an item, or a fluid (`~id`, a filled bucket clicked on it).
fn ghost_key(game: &Game<'_>, id: &str) -> AeKey {
    match id.strip_prefix('~') {
        Some(fluid) => AeKey::fluid(fluid, ""),
        None => key_of(game, &ItemStack::new(id, 1)),
    }
}

/// A click on a bus' ghost cell: a copy of the cursor's item (a filled bucket: its fluid), or cleared.
fn bus_ghost_click(entity: &mut ModBlockEntity, cell: usize, carried: Option<&ItemStack>) {
    let n = bus_cells(entity);
    if cell >= bus_slots(entity).min(n) {
        return;
    }
    let mut cells = ghosts_n(entity, n);
    cells[cell] = carried.map(|s| match bucket_fluid(&s.item) {
        Some(fluid) => (format!("~{fluid}"), 1),
        None => (s.item.to_string(), 1),
    });
    set_ghosts(entity, &cells);
}

/// AE2's redstone modes with a redstone card: ignore (no card), active with signal, without, on a pulse.
const REDSTONE_MODES: [&str; 3] = ["Active with signal", "Active without signal", "Active once per pulse"];
/// The export bus' scheduling: in order, round robin, random.
const SCHEDULES: [&str; 3] = ["Default", "Round robin", "Random"];
const BUS_REDSTONE: i32 = 1;
const BUS_SCHEDULE: i32 = 2;
const BUS_CRAFT_ONLY: i32 = 3;
const BUS_FUZZY: i32 = 4;
const BUS_CLEAR: i32 = 5;
const BUS_PARTITION: i32 = 6;
const BUS_REPORT: i32 = 7;
const BUS_EXTRACT_FILTER: i32 = 8;
const BUS_ACCESS: i32 = 9;
const BUS_PLACE: i32 = 10;
/// The fuzzy mode's icons and words (FuzzyMode), by its setting.
const FUZZY_BUTTON: [([f32; 2], &str); 5] = [
    ([64.0, 96.0], "Match Any"),
    ([48.0, 96.0], "Split Damaged Items"),
    ([32.0, 96.0], "Split Damage at 75%"),
    ([16.0, 96.0], "Split Damage at 50%"),
    ([0.0, 96.0], "Split Damage at 25%"),
];

/// IOBusScreen's and StorageBusScreen's left toolbar, those that show: id, icon, title, value. On the storage bus and
/// the plane the first place is the priority button's.
fn bus_buttons(block: &str, d: &dyn Fn(usize) -> i32) -> (usize, Vec<(i32, [f32; 2], &'static str, &'static str)>) {
    let mut out = Vec::new();
    let storage = block == "ae2:storage_bus";
    if storage {
        out.push((BUS_CLEAR, [96.0, 0.0], "Clear", "Clear Config/Settings"));
        out.push((BUS_PARTITION, [32.0, 64.0], "Partition Storage", "Configures Partition based on currently stored items."));
        out.push(if d(8) != 0 {
            (BUS_REPORT, [96.0, 48.0], "Report Inaccessible Items", "Yes: Items that cannot be extracted will be visible.")
        } else {
            (BUS_REPORT, [80.0, 48.0], "Report Inaccessible Items", "No: Only extractable items will be visible.")
        });
        out.push(if d(7) != 0 {
            (BUS_EXTRACT_FILTER, [176.0, 48.0], "Filtered Operations", "Filter on insertion only.")
        } else {
            (BUS_EXTRACT_FILTER, [160.0, 48.0], "Filtered Operations", "Filter on insert and extract.")
        });
    } else if d(1) >= 0 {
        let v = d(1).clamp(0, 2) as usize;
        out.push((BUS_REDSTONE, [[16.0, 0.0], [0.0, 0.0], [32.0, 0.0]][v], "Redstone Mode", REDSTONE_MODES[v]));
    }
    if block == "ae2:formation_plane" {
        out.push(if d(6) != 0 {
            (BUS_PLACE, [16.0, 224.0], "Block Placement", "Blocks will be dropped as item.")
        } else {
            (BUS_PLACE, [0.0, 224.0], "Block Placement", "Blocks will be placed as block.")
        });
    }
    if d(5) >= 0 {
        let (icon, word) = FUZZY_BUTTON[d(5).clamp(0, 4) as usize];
        out.push((BUS_FUZZY, icon, "Fuzzy Comparison", word));
    }
    if block == "ae2:export_bus" {
        if d(4) >= 0 {
            out.push(if d(4) != 0 {
                (BUS_CRAFT_ONLY, [48.0, 16.0], "Crafting Behavior", "Do not use stocked items, only craft items while exporting.")
            } else {
                (BUS_CRAFT_ONLY, [32.0, 16.0], "Crafting Behavior", "Use stocked items, or craft items while exporting.")
            });
        }
        let v = d(2).clamp(0, 2) as usize;
        out.push((
            BUS_SCHEDULE,
            [[0.0, 240.0], [16.0, 240.0], [32.0, 240.0]][v],
            "Scheduling Mode",
            ["Export the first item until the network is empty, then try the next ones.", "Export using round robin mode.", "Export items in random mode."][v],
        ));
    }
    if storage {
        let v = d(6).clamp(0, 2) as usize;
        out.push((BUS_ACCESS, [[32.0, 144.0], [16.0, 144.0], [0.0, 144.0]][v], "Input/Output Mode", ["Bi-Directional", "Extract Only", "Insert Only"][v]));
    }
    (usize::from(has_priority(block)), out)
}

/// The place of the toolbar's `i`th button (16 pixels, 4 between).
fn bar_place(i: usize) -> [f32; 2] {
    [-20.0, 8.0 + 20.0 * i as f32]
}

fn bus_tick(game: &mut Game<'_>, pos: Pos, entity: &mut ModBlockEntity) {
    // Buses placed before filters were ghosts: their nine filter items become ghosts, the cards move up.
    let ups = upgrade_slots(&entity.id);
    if entity.items.len() > ups {
        let old = std::mem::take(&mut entity.items);
        let filters = old.len() - ups;
        let mut cells = ghosts_n(entity, bus_cells(entity));
        for (i, s) in old.iter().take(filters).enumerate() {
            if let Some(s) = s
                && i < cells.len()
            {
                cells[i] = Some((s.item.to_string(), 1));
            }
        }
        set_ghosts(entity, &cells);
        entity.items = old[filters..].to_vec();
    }
    entity.data.resize(9, 0);
    entity.data[4] = if cards(entity, 0, "ae2:crafting_card") > 0 { nbt_i64(&entity.nbt, "craft_only") as i32 } else { -1 };
    entity.data[5] = if cards(entity, 0, "ae2:fuzzy_card") > 0 { nbt_i64(&entity.nbt, "fuzzy") as i32 } else { -1 };
    entity.data[6] = nbt_i64(&entity.nbt, if entity.id == "ae2:formation_plane" { "drop_items" } else { "access" }) as i32;
    entity.data[7] = nbt_i64(&entity.nbt, "no_extract_filter") as i32;
    entity.data[8] = nbt_i64(&entity.nbt, "report_all") as i32;
    entity.data[0] = bus_slots(entity) as i32;
    entity.data[1] = if cards(entity, 0, "ae2:redstone_card") > 0 { nbt_i64(&entity.nbt, "redstone") as i32 } else { -1 };
    entity.data[2] = nbt_i64(&entity.nbt, "schedule") as i32;
    entity.data[3] = i32::from(cards(entity, 0, "ae2:inverter_card") > 0);
    // The redstone card's signal, kept for the pulse mode.
    if cards(entity, 0, "ae2:redstone_card") > 0 {
        let dim = game.dim.id().to_owned();
        let on = game.native_neighbor_signal(&dim, pos) > 0;
        let was = nbt_i64(&entity.nbt, "rs") != 0;
        if on && !was {
            set_nbt_i64(&mut entity.nbt, "pulse", 1);
        }
        set_nbt_i64(&mut entity.nbt, "rs", i64::from(on));
    }
}

/// Whether the bus works this time (its redstone card's mode), using up a pulse.
fn bus_enabled(game: &mut Game<'_>, bus: Pos) -> bool {
    game.native_with_mod_block_entity(bus, |e| {
        if cards(e, 0, "ae2:redstone_card") == 0 {
            return true;
        }
        let on = nbt_i64(&e.nbt, "rs") != 0;
        match nbt_i64(&e.nbt, "redstone") {
            1 => !on,
            2 => {
                let pulse = nbt_i64(&e.nbt, "pulse") != 0;
                set_nbt_i64(&mut e.nbt, "pulse", 0);
                pulse
            }
            _ => on,
        }
    })
    .unwrap_or(true)
}

/// AE2's import and export buses: items (and fluids, a bucket's worth an item) through the face they touch.
fn run_buses(game: &mut Game<'_>) {
    let buses: Vec<(usize, Pos, Pos, bool)> = SERVER.with_borrow(|s| {
        s.grids
            .iter()
            .enumerate()
            .filter(|(_, g)| g.online)
            .flat_map(|(i, g)| g.io_buses.iter().map(move |(p, t, import)| (i, *p, *t, *import)))
            .collect()
    });
    let dim = game.dim.id().to_owned();
    for (grid, bus, target, import) in buses {
        let target_block = block_id(game, target);
        // AE2's own blocks: only an interface's storage is a container to a bus.
        if target_block.starts_with("ae2:") && canon(&target_block) != "ae2:interface" {
            continue;
        }
        if !bus_enabled(game, bus) {
            continue;
        }
        let Some((raw, speed, crafting, schedule, turn)) = game.native_with_mod_block_entity(bus, |e| {
            let turn = nbt_i64(&e.nbt, "turn");
            set_nbt_i64(&mut e.nbt, "turn", turn + 1);
            (bus_filter_raw(e), cards(e, 0, "ae2:speed_card"), cards(e, 0, "ae2:crafting_card") > 0, nbt_i64(&e.nbt, "schedule"), turn)
        }) else {
            continue;
        };
        let filter = raw.resolve(game);
        let per = [1i64, 8, 32, 64, 96][speed.min(4)];
        let craft_only = crafting && game.native_with_mod_block_entity(bus, |e| nbt_i64(&e.nbt, "craft_only") != 0).unwrap_or(false);
        let face = face_toward(target, bus);
        if import {
            let mut moved = 0;
            if let Some(view) = sided(game, target, face) {
                let slots = sided_slots(game, &view);
                for (i, stack) in slots.iter().enumerate() {
                    let Some(stack) = stack else { continue };
                    if moved >= per {
                        break;
                    }
                    if !view.can_take(i, &stack.item) {
                        continue;
                    }
                    let mut one = stack.clone();
                    one.count = 1;
                    let key = key_of(game, &one);
                    if !filter.allows(&key) {
                        continue;
                    }
                    let want = (per - moved).min(i64::from(stack.count));
                    let stored = with_storage(game, grid, None, |st| st.insert(&key, want, Actionable::Modulate));
                    if stored > 0
                        && let Some((p, slot)) = view.locate(i)
                    {
                        moved += stored;
                        if let Some(be) = game.world.block_entities.get_mut(&(p[0], p[1], p[2]))
                            && let Some(s) = be.slots_mut().get_mut(slot)
                            && let Some(st) = s.as_mut()
                        {
                            st.count -= stored as u32;
                            if st.count == 0 {
                                *s = None;
                            }
                        }
                        game.container_changed((p[0], p[1], p[2]));
                    }
                }
            }
            // Fluids: a bucket's worth for each item the bus may move.
            if let Some(tanks) = game.native_fluid_tanks(&dim, target, face) {
                let fluid = tanks.iter().filter(|t| t.can_drain && t.amount > 0).find_map(|t| t.fluid.clone());
                if let Some(fluid) = fluid {
                    let key = AeKey::fluid(&fluid, "");
                    if filter.allows(&key) {
                        let room = with_storage(game, grid, None, |st| st.insert(&key, per * 1000, Actionable::Simulate));
                        if room > 0
                            && let Some((got_fluid, got)) = game.native_fluid_extract(&dim, target, face, room.min(i64::from(i32::MAX)) as i32)
                        {
                            let k = AeKey::fluid(&got_fluid, "");
                            let stored = with_storage(game, grid, None, |st| st.insert(&k, i64::from(got), Actionable::Modulate));
                            if stored < i64::from(got) {
                                game.native_fluid_insert(target, face, &got_fluid, (i64::from(got) - stored) as i32);
                            }
                        }
                    }
                }
            }
        } else {
            // The export bus: its config keys in order, from a turning start, or at random.
            let mut keys = filter.keys.clone();
            if !keys.is_empty() {
                match schedule {
                    1 => {
                        let n = keys.len() as i64;
                        keys.rotate_left(turn.rem_euclid(n) as usize);
                    }
                    2 => {
                        let r = (SERVER.with_borrow(|s| s.tick) as usize).wrapping_mul(2654435761) % keys.len();
                        keys.rotate_left(r);
                    }
                    _ => {}
                }
            }
            let mut moved = 0;
            for key in keys {
                if moved >= per {
                    break;
                }
                if key.key_type() == super::port_ae2_core::KeyType::Fluids {
                    let have = with_storage(game, grid, None, |st| st.extract(&key, per * 1000, Actionable::Simulate));
                    if have > 0 {
                        let left = game.native_fluid_insert(target, face, key.id(), have.min(i64::from(i32::MAX)) as i32);
                        let put = have - i64::from(left);
                        if put > 0 {
                            with_storage(game, grid, None, |st| st.extract(&key, put, Actionable::Modulate));
                            moved += (put + 999) / 1000;
                        }
                    }
                    continue;
                }
                // ExportBusPart: with a Fuzzy Card, the first of what matches its config in the network.
                let key = if filter.fuzzy {
                    with_storage(game, grid, None, |st| st.contents().find_fuzzy(&key, filter.mode)).into_iter().find(|(_, n)| *n > 0).map_or(key, |(k, _)| k)
                } else {
                    key
                };
                let got = if craft_only { 0 } else { with_storage(game, grid, None, |st| st.extract(&key, per - moved, Actionable::Modulate)) };
                if got <= 0 {
                    // The crafting card: what isn't there is crafted (once at a time, as AE2's bus waits for it).
                    if crafting {
                        let busy = SERVER.with_borrow(|s| {
                            s.grids.get(grid).is_some_and(|g| g.cpus.iter().any(|(c, _, _)| s.cpus.get(c).and_then(|cpu| cpu.job()).is_some_and(|j| j.final_output.what == key)))
                        });
                        let craftable = SERVER.with_borrow(|s| s.grids.get(grid).is_some_and(|g| g.patterns.iter().any(|(_, p)| p.outputs.iter().any(|o| o.what == key))));
                        if craftable && !busy {
                            request_craft(game, grid, 0, &key, per - moved);
                        }
                    }
                    continue;
                }
                moved += got;
                if let Some(left) = game.native_insert_item(target, face, stack_of(&key, got as u32)) {
                    let back = key_of(game, &left);
                    with_storage(game, grid, None, |st| st.insert(&back, i64::from(left.count), Actionable::Modulate));
                }
            }
        }
    }
}

/// A bus' filter as saved (ids), turned into keys with the game.
struct RawFilter {
    ids: Vec<String>,
    fuzzy: bool,
    inverted: bool,
    mode: FuzzyMode,
    access: i64,
    no_extract_filter: bool,
    report_all: bool,
}

/// Settings.FUZZY_MODE, in the button's order.
fn fuzzy_mode_of(e: &ModBlockEntity) -> FuzzyMode {
    [FuzzyMode::IgnoreAll, FuzzyMode::Percent99, FuzzyMode::Percent75, FuzzyMode::Percent50, FuzzyMode::Percent25][nbt_i64(&e.nbt, "fuzzy").clamp(0, 4) as usize]
}

impl RawFilter {
    fn resolve(self, game: &Game<'_>) -> BusFilter {
        BusFilter {
            keys: self.ids.iter().map(|id| ghost_key(game, id)).collect(),
            fuzzy: self.fuzzy,
            inverted: self.inverted,
            mode: self.mode,
            access: self.access,
            no_extract_filter: self.no_extract_filter,
            report_all: self.report_all,
        }
    }
}

fn bus_filter_raw(e: &ModBlockEntity) -> RawFilter {
    RawFilter {
        ids: ghosts_n(e, bus_cells(e)).into_iter().take(bus_slots(e)).flatten().map(|(id, _)| id).collect(),
        fuzzy: cards(e, 0, "ae2:fuzzy_card") > 0,
        inverted: cards(e, 0, "ae2:inverter_card") > 0,
        mode: fuzzy_mode_of(e),
        access: nbt_i64(&e.nbt, "access"),
        no_extract_filter: nbt_i64(&e.nbt, "no_extract_filter") != 0,
        report_all: nbt_i64(&e.nbt, "report_all") != 0,
    }
}

/// A storage bus' view of fluid tanks (and cauldrons): what they hold, changed through the face afterwards.
struct ExternalFluids {
    tanks: Vec<(Option<String>, i64, i64, bool, bool)>,
    filter: BusFilter,
    changed: bool,
}

impl super::port_ae2_grid::MeStorage for ExternalFluids {
    fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        if what.key_type() != super::port_ae2_core::KeyType::Fluids || self.filter.access == 1 || !self.filter.allows(what) {
            return 0;
        }
        let mut left = amount;
        for (fluid, n, cap, fill, _) in self.tanks.iter_mut() {
            if left <= 0 || !*fill || fluid.as_deref().is_some_and(|f| f != what.id()) {
                continue;
            }
            let put = left.min(*cap - *n);
            if put > 0 && mode == Actionable::Modulate {
                *fluid = Some(what.id().to_owned());
                *n += put;
                self.changed = true;
            }
            left -= put.max(0);
        }
        amount - left
    }
    fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        if self.filter.access == 2 || !(self.filter.no_extract_filter || self.filter.allows(what)) {
            return 0;
        }
        let mut got = 0;
        for (fluid, n, _, _, drain) in self.tanks.iter_mut() {
            if got >= amount || !*drain || fluid.as_deref() != Some(what.id()) {
                continue;
            }
            let take = (amount - got).min(*n);
            got += take;
            if mode == Actionable::Modulate {
                *n -= take;
                if *n == 0 {
                    *fluid = None;
                }
                self.changed = true;
            }
        }
        got
    }
    fn available_stacks(&self, out: &mut super::port_ae2_core::KeyCounter) {
        for (fluid, n, _, _, _) in &self.tanks {
            if let Some(f) = fluid
                && *n > 0
                && self.filter.access != 2
                && (self.filter.no_extract_filter || self.filter.allows(&AeKey::fluid(f, "")))
            {
                out.add(&AeKey::fluid(f, ""), *n);
            }
        }
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ---- Parts join through their back; the multiblocks' looks.

/// AE2's parts sit on a cable: a bus, terminal, plane, tunnel or emitter joins the network only through its
/// back (the side away from what it faces).
fn is_part(block: &str) -> bool {
    block.strip_prefix("ae2:").is_some_and(|n| {
        n.ends_with("_bus")
            || n.ends_with("_p2p_tunnel")
            || n.ends_with("_plane")
            || matches!(n, "terminal" | "crafting_terminal" | "pattern_encoding_terminal" | "pattern_access_terminal" | "level_emitter" | "energy_level_emitter"
                | "storage_monitor" | "conversion_monitor" | "cable_interface" | "cable_pattern_provider" | "cable_energy_acceptor"
                | "monitor" | "semi_dark_monitor" | "dark_monitor" | "hz_monitor_lit" | "hz_semi_dark_monitor_lit" | "hz_dark_monitor_lit"
                | "quartz_fiber" | "cable_anchor")
    })
}

fn part_back(game: &Game<'_>, p: Pos) -> Pos {
    let f = facing_dir(game.native_block_property(p, "facing").as_deref());
    [-f[0], -f[1], -f[2]]
}

/// Whether two neighbouring network blocks connect: cable colours, and parts only through their backs.
fn links(game: &Game<'_>, p: Pos, a: &str, q: Pos, b: &str) -> bool {
    if !joins(a, b) {
        return false;
    }
    let d = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
    let back = [-d[0], -d[1], -d[2]];
    match (is_cable_bus(a), is_cable_bus(b)) {
        (true, true) => return bus_side(game, p, d) && bus_side(game, q, back),
        (true, false) => return bus_side(game, p, d) && bus_joins(game, p, b) && (!back_only(b) || part_back(game, q) == back),
        (false, true) => return bus_side(game, q, back) && bus_joins(game, q, a) && (!back_only(a) || part_back(game, p) == d),
        _ => {}
    }
    if (is_part(a) && cable(b).is_none()) || (is_part(b) && cable(a).is_none()) {
        return false;
    }
    (!back_only(a) || part_back(game, p) == d) && (!back_only(b) || part_back(game, q) == back)
}

/// A bus, plane or tunnel: AE2's cable bus, its part on the front face and (once a cable is beside it) a cable in
/// the middle of its block.
fn is_cable_bus(block: &str) -> bool {
    block.strip_prefix("ae2:").is_some_and(|n| n.ends_with("_bus") || n.ends_with("_p2p_tunnel") || n.ends_with("_plane"))
}

/// The cable in a bus's block: the cable at its back, else the first one beside it (not in front, where the part
/// is). None: the part alone, no cable.
fn bus_wire(game: &Game<'_>, p: Pos) -> Option<String> {
    let back = part_back(game, p);
    let front = [-back[0], -back[1], -back[2]];
    std::iter::once(back)
        .chain(SIDES.into_iter().filter(move |d| *d != back && *d != front))
        .map(|d| block_id(game, add(p, d)))
        .find(|b| cable(b).is_some())
}

/// A side of a bus its cable reaches: any but the part's front, once it has a cable; a toggle bus's front too while
/// it is on (ToggleBusPart: redstone joins its outer node, the inverted one's without redstone).
fn bus_side(game: &Game<'_>, p: Pos, d: Pos) -> bool {
    let back = part_back(game, p);
    if bus_wire(game, p).is_none() {
        return false;
    }
    if d != [-back[0], -back[1], -back[2]] {
        return true;
    }
    matches!(ae2_name(&block_id(game, p)), Some("toggle_bus" | "inverted_toggle_bus")) && SERVER.with_borrow(|s| s.toggles_on.contains(&p))
}

/// The bus's cable joins `other` as that cable would (colours).
fn bus_joins(game: &Game<'_>, p: Pos, other: &str) -> bool {
    bus_wire(game, p).is_some_and(|w| joins(&w, other))
}

/// Parts, and the crystal resonance generator (CrystalResonanceGeneratorBlockEntity.getGridConnectableSides is
/// its back alone), join the network on one side only.
fn back_only(block: &str) -> bool {
    is_part(block) || ae2_name(block) == Some("crystal_resonance_generator")
}

/// AE2's controller look (ControllerBlockEntity.updateState): a block, a column along an axis, or inside a
/// bigger structure (two alternating looks); online, offline or conflicted.
fn controller_look(game: &mut Game<'_>, pos: Pos, grid: Option<usize>) {
    let is_c = |game: &Game<'_>, d: Pos| block_id(game, add(pos, d)) == "ae2:controller";
    let xx = is_c(game, [-1, 0, 0]) && is_c(game, [1, 0, 0]);
    let yy = is_c(game, [0, -1, 0]) && is_c(game, [0, 1, 0]);
    let zz = is_c(game, [0, 0, -1]) && is_c(game, [0, 0, 1]);
    let kind = match (u8::from(xx) + u8::from(yy) + u8::from(zz), xx, yy, zz) {
        (n, ..) if n >= 2 => {
            if (pos[0] + pos[1] + pos[2]) & 1 == 0 { "inside_a" } else { "inside_b" }
        }
        (_, true, _, _) => "column_x",
        (_, _, true, _) => "column_y",
        (_, _, _, true) => "column_z",
        _ => "block",
    };
    let state = SERVER.with_borrow(|s| match grid.and_then(|g| s.grids.get(g)) {
        Some(g) if g.conflict => "conflicted",
        Some(g) if g.online => "online",
        _ => "offline",
    });
    for (key, value) in [("type", kind), ("state", state)] {
        if game.native_block_property(pos, key).is_some_and(|v| v != value) {
            game.native_set_block_property_with(pos, key, value, 2);
        }
    }
}

/// AE2's spatial pylon look: a column along one axis (start, middle, end) or alone; lit while online.
fn pylon_look(game: &mut Game<'_>, pos: Pos, online: bool) {
    let is_p = |game: &Game<'_>, d: Pos| block_id(game, add(pos, d)) == "ae2:spatial_pylon";
    let axes: Vec<(char, bool, bool)> = [('x', [1, 0, 0]), ('y', [0, 1, 0]), ('z', [0, 0, 1])]
        .iter()
        .map(|(a, d)| (*a, is_p(game, [-d[0], -d[1], -d[2]]), is_p(game, *d)))
        .filter(|(_, lo, hi)| *lo || *hi)
        .collect();
    let span = match axes.as_slice() {
        [(axis, lo, hi)] => format!("{axis}_{}", if !lo { "start" } else if !hi { "end" } else { "middle" }),
        _ => "none".to_owned(),
    };
    let state = if online && span != "none" { "online" } else { "offline" };
    for (key, value) in [("span", span.as_str()), ("state", state)] {
        if game.native_block_property(pos, key).is_some_and(|v| v != value) {
            game.native_set_block_property_with(pos, key, value, 2);
        }
    }
}

/// AE2's formed quantum bridge: the link chamber, the corner and edge rings (lit while powered), and which
/// sides of the chamber and corners join the structure.
fn qnb_look(game: &mut Game<'_>, pos: Pos, powered: bool) {
    let block = block_id(game, pos);
    let mut role = "none";
    let mut plane: Option<(usize, usize)> = None;
    if block == "ae2:quantum_link" {
        if qnb_formed(game, pos) {
            role = "link";
            plane = [(1usize, 2usize), (0, 2), (0, 1)].into_iter().find(|&(i, j)| {
                (-1..=1).all(|a| {
                    (-1..=1).all(|b| {
                        let mut q = pos;
                        q[i] += a;
                        q[j] += b;
                        (a == 0 && b == 0) || block_id(game, q) == "ae2:quantum_ring"
                    })
                })
            });
        }
    } else {
        // A ring: the formed chamber beside it in its plane, and whether it sits at a corner.
        'find: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let l = add(pos, [dx, dy, dz]);
                    if (dx, dy, dz) == (0, 0, 0) || block_id(game, l) != "ae2:quantum_link" || !qnb_formed(game, l) {
                        continue;
                    }
                    let off = [-dx, -dy, -dz];
                    let zeros = off.iter().filter(|v| **v == 0).count();
                    if zeros == 0 {
                        continue;
                    }
                    role = if zeros == 1 { "corner" } else { "edge" };
                    plane = Some(match (off[0] == 0, off[1] == 0, off[2] == 0) {
                        (true, false, false) => (1, 2),
                        (false, true, false) => (0, 2),
                        (false, false, true) => (0, 1),
                        _ => {
                            // An edge has two zero axes: its plane is the one the chamber's ring fills.
                            let n = if off[0] != 0 { 0 } else if off[1] != 0 { 1 } else { 2 };
                            let other = (0..3).find(|a| *a != n && block_id(game, {
                                let mut q = l;
                                q[*a] += 1;
                                q
                            }) == "ae2:quantum_ring").unwrap_or((n + 1) % 3);
                            (n.min(other), n.max(other))
                        }
                    });
                    break 'find;
                }
            }
        }
    }
    let lit = if powered && role != "none" { "on" } else { "off" };
    // Joins: the chamber to the four edges, a corner to its two edges (the sides in its plane that are rings).
    let sides = ["down", "up", "north", "south", "west", "east"];
    let mut joined = [false; 6];
    if let Some((i, j)) = plane.filter(|_| role == "link" || role == "corner") {
        for (k, d) in SIDES.iter().enumerate() {
            let in_plane = (0..3).all(|a| a == i || a == j || d[a] == 0);
            joined[k] = in_plane && matches!(block_id(game, add(pos, *d)).as_str(), "ae2:quantum_ring" | "ae2:quantum_link");
        }
    }
    let mut want: Vec<(String, String)> = vec![("qnb".into(), role.into()), ("power".into(), lit.into())];
    for (k, s) in sides.iter().enumerate() {
        want.push((format!("side_{s}"), joined[k].to_string()));
    }
    for (key, value) in want {
        if game.native_block_property(pos, &key).is_some_and(|v| v != value) {
            game.native_set_block_property_with(pos, &key, &value, 2);
        }
    }
}

/// The controller's status screen numbers: online, channels in use, devices, stored and capacity (kAE), draw.
fn controller_data(grid: Option<usize>, pos: Pos, entity: &mut ModBlockEntity) {
    // The status screen's numbers: online, channels in use, devices, stored and capacity (kAE), draw (AE/t ×10).
    entity.data.resize(6, 0);
    let info = SERVER.with_borrow(|s| {
        let g = grid.and_then(|g| s.grids.get(g))?;
        let root = g.nodes.iter().min().copied()?;
        let stored = s.energy.get(&root).copied().unwrap_or(0.0);
        let cap = grid_capacity(g, &s.devices);
        // Online is AE2's: a controller with power (it glows then).
        let on = g.online && s.powered.get(&pos).copied().unwrap_or(false);
        Some([i32::from(on), g.channels as i32, g.nodes.len() as i32, (stored / 1000.0) as i32, (cap / 1000.0).min(32_000.0) as i32, (g.idle * 10.0) as i32])
    });
    if let Some(v) = info {
        entity.data.copy_from_slice(&v);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Placement and the wrench as AE2 has them, slot frames AE2 draws in code, its number entry, fluids' pictures,
// and a pattern showing what it makes while shift is held.

/// The six sides by name, as `facing` values.
const FACING_NAMES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

fn opposite_facing(f: &str) -> &'static str {
    match f {
        "down" => "up",
        "up" => "down",
        "north" => "south",
        "south" => "north",
        "west" => "east",
        _ => "west",
    }
}

fn facing_name(d: Pos) -> &'static str {
    FACING_NAMES[SIDES.iter().position(|s| *s == d).unwrap_or(2)]
}

/// The block the player clicked to place the block at `pos`: the solid neighbour most along their view.
fn clicked_side(game: &Game<'_>, pos: Pos, yaw: f32, pitch: f32) -> Option<Pos> {
    let (yaw, pitch) = (f64::from(yaw).to_radians(), f64::from(pitch).to_radians());
    let view = [-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()];
    SIDES
        .iter()
        .filter(|d| !matches!(block_id(game, add(pos, **d)).as_str(), "minecraft:air" | "minecraft:cave_air" | "minecraft:water" | "minecraft:lava"))
        .max_by(|a, b| {
            let da = f64::from(a[0]) * view[0] + f64::from(a[1]) * view[1] + f64::from(a[2]) * view[2];
            let db = f64::from(b[0]) * view[0] + f64::from(b[1]) * view[1] + f64::from(b[2]) * view[2];
            da.total_cmp(&db)
        })
        .copied()
}

fn is_network_block(block: &str) -> bool {
    (is_device(block) && !is_budding(block) && ae2_name(block) != Some("light_detector")) || cable(block).is_some()
}

/// `native_block_placed` for AE2's directional blocks: a part on a cable sits on it facing out, as AE2 puts it on
/// the cable's face; a bus, plane or tunnel against anything else faces into it (what it works on); emitters
/// need a cable or network block to sit on; the crystal resonance generator points out of the clicked face.
pub fn placed(game: &mut Game<'_>, pos: Pos) {
    placed_inner(game, pos);
    cable_sides(game, pos);
    for d in SIDES {
        cable_sides(game, add(pos, d));
    }
}

fn placed_inner(game: &mut Game<'_>, pos: Pos) {
    let block = block_id(game, pos);
    // QuartzFixtureBlock.getStateForPlacement: every other block in a row looks the other way (`odd`).
    if matches!(ae2_name(&block), Some("quartz_fixture" | "light_detector")) {
        let odd = (pos[0] + pos[1] + pos[2]).rem_euclid(2) != 0;
        game.native_set_block_property_with(pos, "odd", if odd { "true" } else { "false" }, 3);
        return;
    }
    let Some(name) = ae2_name(&block).map(str::to_owned) else { return };
    let Some(player) = game.native_acting_player() else { return };
    if name == "annihilation_plane" {
        SERVER.with_borrow_mut(|s| {
            if let Some(e) = s.last_enchants.get(&player.uuid).filter(|e| !e.is_empty()).cloned() {
                s.plane_enchants.insert(pos, e);
            }
        });
    }
    // The block the player clicked to place this one: the face they hit, else the neighbour most along their view.
    let hit = SERVER.with_borrow(|s| s.last_click.get(&player.uuid).copied());
    let clicked = match hit {
        Some((at, face)) if add(at, face) == pos => Some([-face[0], -face[1], -face[2]]),
        _ => clicked_side(game, pos, player.yaw, player.pitch),
    };
    let clicked_block = clicked.map(|d| block_id(game, add(pos, d))).unwrap_or_default();
    if is_part(&block) {
        let Some(d) = clicked else { return };
        let on_network = is_network_block(&clicked_block);
        let facing = if on_network {
            facing_name([-d[0], -d[1], -d[2]])
        } else if name.ends_with("level_emitter") {
            // AE2's emitters are parts: here they sit only on a cable (or a network block).
            let dim = game.dim.id().to_owned();
            if let Some(air) = game.pack.assets.registry.default_state("minecraft:air") {
                game.native_set_block(&dim, pos, air, 3);
            }
            let _ = game.native_give_stack(player.uuid, ItemStack::new(&block, 1));
            game.native_action_bar(player.uuid, horizon_client::text::Text::legacy("§cPlace it on a cable"));
            return;
        } else if name.ends_with("_bus") || name.ends_with("_p2p_tunnel") || name.ends_with("_plane") {
            facing_name(d)
        } else {
            facing_name([-d[0], -d[1], -d[2]])
        };
        game.native_set_block_property_with(pos, "facing", facing, 3);
        return;
    }
    if name == "crystal_resonance_generator" {
        if let Some(d) = clicked {
            game.native_set_block_property_with(pos, "facing", facing_name([-d[0], -d[1], -d[2]]), 3);
        }
        return;
    }
    let faces_player = name == "wireless_access_point";
    if !faces_player && !matches!(name.as_str(), "drive" | "chest" | "charger" | "molecular_assembler" | "inscriber" | "sky_stone_chest" | "smooth_sky_stone_chest" | "crank") {
        return;
    }
    let upright = matches!(name.as_str(), "charger" | "inscriber" | "sky_stone_chest" | "smooth_sky_stone_chest");
    let facing = if !upright && player.pitch > 45.0 {
        "up"
    } else if !upright && player.pitch < -45.0 {
        "down"
    } else {
        // Yaw 0 looks south: the block then faces north, back at the player.
        match (((player.yaw.rem_euclid(360.0) + 45.0) / 90.0) as i32) % 4 {
            0 => "north",
            1 => "east",
            2 => "south",
            _ => "west",
        }
    };
    game.native_set_block_property_with(pos, "facing", facing, 3);
}

/// The wrench on a block of a network: it turns only to a facing with a cable or network block behind it (a
/// part keeps its cable; a device keeps its connection), the next such facing each time.
fn wrench_turn(game: &mut Game<'_>, pos: Pos, block: &str) -> bool {
    let Some(facing) = game.native_block_property(pos, "facing") else { return false };
    let networked = SIDES.iter().any(|d| is_network_block(&block_id(game, add(pos, *d))));
    let at = FACING_NAMES.iter().position(|n| *n == facing).unwrap_or(0);
    for step in 1..FACING_NAMES.len() {
        let next = FACING_NAMES[(at + step) % FACING_NAMES.len()];
        let back = facing_dir(Some(opposite_facing(next)));
        if networked && !is_network_block(&block_id(game, add(pos, back))) {
            continue;
        }
        if game.native_set_block_property_with(pos, "facing", next, 3) {
            SERVER.with_borrow_mut(|s| s.flips.push(pos));
            return true;
        }
    }
    if let Some(player) = game.native_acting_player() {
        let _ = block;
        game.native_action_bar(player.uuid, horizon_client::text::Text::legacy("§7Nothing to turn to: no cable on that side"));
    }
    true
}

/// A slot's frame (vanilla's: dark top-left, light bottom-right), for the slots AE2 draws in code.
fn draw_slot(game: &Game<'_>, batch: &mut HudBatch, x: f32, y: f32) {
    game.native_fill(batch, [x - 1.0, y - 1.0, 18.0, 18.0], 0xFF373737);
    game.native_fill(batch, [x, y, 17.0, 17.0], 0xFFFFFFFF);
    game.native_fill(batch, [x, y, 16.0, 16.0], 0xFF8B8B8B);
}

/// An AE key's picture: an item's icon, or a fluid as its still texture (animated, tinted as in the world).
fn draw_key(game: &Game<'_>, batch: &mut HudBatch, id: &str, at: [f32; 2], size: f32) {
    if let Some(fluid) = id.strip_prefix("~minecraft:")
        && matches!(fluid, "water" | "lava")
        && game.native_item_model(batch, "minecraft:bucket", &format!("ae2:hz_fluid_{fluid}"), at, size)
    {
        return;
    }
    game.native_item(batch, &icon_of(id), at, size);
}

/// AE2's NumberEntryWidget on the level emitters: +1 +10 +100 +1000 above the box, the same less below.
const LEVEL_ROWS: [f32; 2] = [17.0, 59.0];
const EMIT_MODE: i32 = 3;
/// Settings.AUTO_EXPORT on the inscriber.
const INSCRIBER_EXPORT: i32 = 5;
/// Settings.FUZZY_MODE on the level emitter.
const EMIT_FUZZY: i32 = 6;

fn draw_level_widget(game: &Game<'_>, batch: &mut HudBatch, ox: f32, oy: f32, box_w: f32) {
    let mouse = CLIENT.with_borrow(|c| c.mouse);
    for (x, w, n) in STEPS {
        for (row, sign) in LEVEL_ROWS.iter().zip(["+", "-"]) {
            let r = [x, *row, w, 20.0];
            let hover = mouse[0] >= r[0] && mouse[0] < r[0] + r[2] && mouse[1] >= r[1] && mouse[1] < r[1] + r[3];
            draw_button_at(game, batch, [ox + x, oy + row, w, 20.0], &format!("{sign}{n}"), hover);
        }
    }
    let (text, focus) = CLIENT.with_borrow(|c| (c.level_text.clone(), c.level_focus));
    game.native_fill(batch, [ox + 22.0, oy + 41.0, box_w + 2.0, 14.0], if focus { 0xFFFFFFFF } else { 0xFFA0A0A0 });
    game.native_fill(batch, [ox + 23.0, oy + 42.0, box_w, 12.0], 0xFF000000);
    game.native_text(batch, &if focus { format!("{text}_") } else { text }, [ox + 26.0, oy + 44.0], 0xFFFFFF, false);
    // AE2's redstone mode button on the toolbar: emit above or below the level.
    let below = CLIENT.with_borrow(|c| c.emit_below);
    draw_button_hover(game, batch, [ox + TOOLBAR[0][0], oy + TOOLBAR[0][1], 18.0, 18.0], "", [ox, oy]);
    game.native_item(batch, if below { "minecraft:lever" } else { "minecraft:redstone_torch" }, [ox + TOOLBAR[0][0] + 1.0, oy + TOOLBAR[0][1] + 1.0], 16.0);
}

/// A click on the number entry: a step, the box (focus; right-click clears), or the mode button. True if taken.
fn level_widget_click(at: [f32; 2], right: bool, box_w: f32, button: &mut Option<i32>) -> bool {
    let on = |r: [f32; 4]| at[0] >= r[0] && at[0] < r[0] + r[2] && at[1] >= r[1] && at[1] < r[1] + r[3];
    if on([TOOLBAR[0][0], TOOLBAR[0][1], 18.0, 18.0]) {
        *button = Some(EMIT_MODE);
        return true;
    }
    for (x, w, n) in STEPS {
        for (row, sign) in LEVEL_ROWS.iter().zip([1i64, -1]) {
            if on([x, *row, w, 20.0]) {
                CLIENT.with_borrow_mut(|c| {
                    let v = math_value(&c.level_text).unwrap_or(0.0);
                    let next = (v + (sign * n) as f64).max(0.0);
                    c.level_text = if next.fract() == 0.0 { format!("{next:.0}") } else { format!("{next:.3}").trim_end_matches('0').to_owned() };
                    c.level_dirty = true;
                });
                return true;
            }
        }
    }
    let in_box = on([23.0, 42.0, box_w, 12.0]);
    CLIENT.with_borrow_mut(|c| {
        if in_box && right {
            c.level_text.clear();
            c.level_dirty = true;
        }
        c.level_focus = in_box;
    });
    in_box
}

/// `native_item_decorations`: an encoded pattern shows the item it makes while shift is held (AE2's
/// pattern rendering).
pub fn pattern_decoration(game: &Game<'_>, stack: &ItemStack, x: f32, y: f32, batch: &mut HudBatch) {
    if &*stack.item == "ae2:meteorite_compass" {
        return;
    }
    if !matches!(&*stack.item, "ae2:crafting_pattern" | "ae2:processing_pattern" | "ae2:smithing_table_pattern" | "ae2:stonecutting_pattern") {
        return;
    }
    let shift = game.keys.contains(&winit::keyboard::KeyCode::ShiftLeft) || game.keys.contains(&winit::keyboard::KeyCode::ShiftRight);
    if !shift {
        return;
    }
    let Some(Tag::Compound(data)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else { return };
    let Some(Tag::Compound(p)) = data.get(&java(PATTERN_KEY)) else { return };
    let Some((out, _)) = read_entries(p.get(&java("out"))).into_iter().next() else { return };
    game.native_fill(batch, [x, y, 16.0, 16.0], 0xFF8B8B8B);
    draw_key(game, batch, &wire_id(&out), [x, y], 16.0);
}

/// AE2's portable cell battery (AEConfig `portableCellBattery`).
const PORTABLE_BATTERY: f64 = 20_000.0;

/// AE2's powered items (AEBasePoweredItem): the battery (AEConfig's `battery` section) and the charge rate a
/// charger gives (`getChargeRate`, without energy cards).
fn item_battery(item: &str) -> Option<(f64, f64)> {
    if portable_type(item).is_some() {
        return Some((PORTABLE_BATTERY, 80.0));
    }
    match ae2_name(item)? {
        "wireless_terminal" | "wireless_crafting_terminal" => Some((1_600_000.0, 800.0)),
        "entropy_manipulator" => Some((200_000.0, 800.0)),
        "matter_cannon" => Some((200_000.0, 800.0)),
        "charged_staff" => Some((8_000.0, 32.0)),
        "color_applicator" => Some((20_000.0, 80.0)),
        _ => None,
    }
}

/// A powered item's charge (AE2's `internalCurrentPower` on the stack).
fn portable_power(stack: &ItemStack) -> f64 {
    let Some(Tag::Compound(d)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        return 0.0;
    };
    match d.get(&java("internalCurrentPower")) {
        Some(Tag::Double(p)) => f64::from_bits(*p),
        _ => 0.0,
    }
}

fn set_portable_power(stack: &mut ItemStack, power: f64) {
    let most = item_battery(&stack.item).map_or(PORTABLE_BATTERY, |b| b.0);
    set_custom(stack, "internalCurrentPower", Tag::Double(power.clamp(0.0, most).to_bits()));
}

/// Uses `ae` of a powered item's charge (AEBasePoweredItem.extractAEPower): false, and nothing taken, when it
/// holds less. Creative players use none.
fn use_item_power(stack: &mut ItemStack, ae: f64, creative: bool) -> bool {
    if creative {
        return true;
    }
    let have = portable_power(stack);
    if have < ae {
        return false;
    }
    set_portable_power(stack, have - ae);
    true
}

/// Tooltips.energyStorageComponent: "Stored Energy: 1.2/1.6M AE (75%)", the numbers in AE2's number colour and
/// the percent from red to green.
fn energy_line(power: f64, most: f64) -> horizon_client::text::Text {
    let (cur, max, unit) = maxed_amount(power, most);
    let ratio = if most > 0.0 { power / most } else { 0.0 };
    let r = (255.0 * (2.0 - 2.0 * ratio).clamp(0.0, 1.0)) as u32;
    let g = (255.0 * (2.0 * ratio).clamp(0.0, 1.0)) as u32;
    let pct = {
        let p = format!("{:.2}", ratio * 100.0);
        format!("{}%", p.trim_end_matches('0').trim_end_matches('.'))
    };
    let number = if cur == "0" { cur } else { format!("{cur}{unit}") };
    rgb_text(&[
        ("Stored Energy: ", 0xAAAAAA),
        (&number, 0x886EFF),
        ("/", 0xAAAAAA),
        (&format!("{max}{unit}"), 0x886EFF),
        (" AE", 0xAAAAAA),
        (" (", 0xAAAAAA),
        (&pct, (r << 16) | (g << 8)),
        (")", 0xAAAAAA),
    ])
}

/// Text in spans of their own colours.
fn rgb_text(parts: &[(&str, u32)]) -> horizon_client::text::Text {
    let mut t = horizon_client::text::Text::default();
    for (text, color) in parts {
        t.spans.push(horizon_client::text::Span { text: (*text).to_owned(), style: horizon_client::text::Style::color(*color) });
    }
    t
}

/// Tooltips.getMaxedAmount: both numbers in the larger's unit (k, M, G...), "#.###" style.
fn maxed_amount(amount: f64, max: f64) -> (String, String, &'static str) {
    const UNITS: [&str; 6] = ["k", "M", "G", "T", "P", "E"];
    let short = |v: f64, by: f64| {
        let f = v / by;
        let s = if f < 10.0 { format!("{f:.3}") } else if f < 100.0 { format!("{f:.2}") } else { format!("{f:.1}") };
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    };
    if max < 10_000.0 {
        return (short(amount, 1.0), short(max, 1.0), "");
    }
    let mut i = 0;
    while i + 1 < UNITS.len() && max / 1000f64.powi(i as i32 + 1) >= 1000.0 {
        i += 1;
    }
    let by = 1000f64.powi(i as i32 + 1);
    (short(amount, by), short(max, by), UNITS[i])
}

/// `native_item_bar`: a powered item's charge as AE2 shows it (its bar, blue-green as it fills).
pub fn item_bar(stack: &ItemStack, bar: &mut Option<(i32, u32)>) {
    let Some((most, _)) = item_battery(&stack.item) else { return };
    // AEBasePoweredItem.getBarWidth/getBarColor: always shown, green (hsv 1/3).
    let ratio = portable_power(stack) / most;
    *bar = Some((((13.0 * ratio) as i32).clamp(0, 13), 0x00FF00));
}

// ---------------------------------------------------------------------------------------------------------
// AE2's storage and conversion monitors (AbstractMonitorPart, ConversionMonitorPart): a configured item shown on
// the screen with the network's amount; locked with Shift and an empty hand. The conversion monitor stores the
// held stack (locked and empty-handed, every matching stack carried) and hands out a stack on a punch (one with
// Shift).

fn is_monitor(block: &str) -> bool {
    matches!(ae2_name(block), Some("storage_monitor" | "conversion_monitor"))
}

fn nbt_string(nbt: &Compound, key: &str) -> String {
    match nbt.get(&java(key)) {
        Some(Tag::String(s)) => s.to_string().unwrap_or_default(),
        _ => String::new(),
    }
}

/// "minecraft:iron_ingot" as "Iron Ingot".
fn plain_name(id: &str) -> String {
    let path = id.split_once(':').map_or(id, |(_, p)| p);
    path.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The monitor's network, when it is on a working one.
fn monitor_grid(pos: Pos) -> Option<usize> {
    SERVER.with_borrow(|s| {
        let g = *s.grid_of.get(&pos)?;
        s.grids.get(g).filter(|x| x.online && x.active.contains(&pos)).map(|_| g)
    })
}

/// What the monitor shows, and the network's amount of it, on the player's action bar.
fn monitor_report(game: &mut Game<'_>, pos: Pos, uuid: u128) {
    let key = game.native_with_mod_block_entity(pos, |e| nbt_string(&e.nbt, "item")).and_then(|k| key_from_string(&k));
    let text = match (key, monitor_grid(pos)) {
        (Some(key), Some(g)) => {
            let n = with_storage(game, g, None, |st| st.contents().get(&key));
            format!("{}: {}", plain_name(key.id()), key.format_amount(n, super::port_ae2_core::AmountFormat::Full))
        }
        (Some(key), None) => plain_name(key.id()),
        (None, _) => return,
    };
    game.native_action_bar(uuid, horizon_client::text::Text::legacy(&text));
}

/// Right-click on a monitor. True when the monitor took the click.
fn monitor_click(game: &mut Game<'_>, pos: Pos) -> bool {
    let block = block_id(game, pos);
    if !is_monitor(&block) {
        return false;
    }
    let Some(player) = game.native_acting_player() else { return true };
    let Some(grid) = monitor_grid(pos) else { return true };
    let held = game.held().cloned();
    let (shown, locked) = game
        .native_with_mod_block_entity(pos, |e| (nbt_string(&e.nbt, "item"), nbt_i64(&e.nbt, "locked") != 0))
        .unwrap_or_default();
    let shown = key_from_string(&shown);
    if sneaking(game) {
        if held.is_none() {
            game.native_with_mod_block_entity(pos, |e| set_nbt_i64(&mut e.nbt, "locked", i64::from(!locked)));
            let msg = if locked { "Monitor is now Unlocked." } else { "Monitor is now Locked." };
            game.native_action_bar(player.uuid, horizon_client::text::Text::legacy(msg));
        }
        return true;
    }
    let conversion = ae2_name(&block) == Some("conversion_monitor");
    let held_key = held.as_ref().map(|s| key_of(game, s));
    let matches = held_key.is_some() && held_key == shown;
    if conversion && (matches || (locked && held.is_none())) {
        if held.is_none() {
            // Every carried stack of the shown item.
            let Some(key) = shown else { return true };
            for slot in 0..game.inventory.len() {
                let Some(stack) = game.inventory[slot].clone() else { continue };
                if key_of(game, &stack) != key {
                    continue;
                }
                let stored = with_storage(game, grid, None, |st| st.insert(&key, i64::from(stack.count), Actionable::Modulate));
                if stored > 0 {
                    let left = stack.count - stored as u32;
                    game.inventory[slot] = (left > 0).then(|| ItemStack { count: left, ..stack });
                }
            }
        } else if let (Some(stack), Some(key)) = (held, held_key) {
            let stored = with_storage(game, grid, None, |st| st.insert(&key, i64::from(stack.count), Actionable::Modulate));
            let hand = game.held_mut();
            if let Some(h) = hand.as_mut() {
                h.count -= stored as u32;
                if h.count == 0 {
                    *hand = None;
                }
            }
        }
    } else if !locked {
        let item = held_key.map(|k| key_to_string(&k)).unwrap_or_default();
        game.native_with_mod_block_entity(pos, |e| {
            e.nbt.insert(java("item"), Tag::String(java(&item)));
        });
        // The screen's item goes to the clients.
        game.native_block_entity_sync(pos);
    }
    monitor_report(game, pos, player.uuid);
    true
}

/// A punch on a conversion monitor: a stack of the shown item from the network (one with Shift). True when the
/// monitor took it (AE2's BlockAttackHook: the punch then doesn't dig the monitor).
pub fn monitor_punch(game: &mut Game<'_>, pos: Pos) -> bool {
    if ae2_name(&block_id(game, pos)) != Some("conversion_monitor") {
        return false;
    }
    let Some(player) = game.native_acting_player() else { return false };
    let Some(grid) = monitor_grid(pos) else { return false };
    let Some(key) = game.native_with_mod_block_entity(pos, |e| nbt_string(&e.nbt, "item")).and_then(|k| key_from_string(&k)) else { return false };
    let max = i64::from(game.native_max_stack_size(key.id()).unwrap_or(64));
    let want = if sneaking(game) { 1 } else { max };
    let got = with_storage(game, grid, None, |st| st.extract(&key, want, Actionable::Modulate));
    if got > 0
        && let Some(left) = game.native_give_stack(player.uuid, stack_of(&key, got as u32))
    {
        let back = with_storage(game, grid, None, |st| st.insert(&key, i64::from(left.count), Actionable::Modulate));
        if back < i64::from(left.count) {
            let _ = game.native_give_stack(player.uuid, ItemStack { count: left.count - back as u32, ..left });
        }
    }
    monitor_report(game, pos, player.uuid);
    true
}

/// The monitor font's characters, in the order of its sheet (ae2:gui/hz/monitor_font, tools/monitor_font.py),
/// and their widths in its pixels.
const MONITOR_GLYPHS: &str = "0123456789.KMGTPEBm~Craft";
const MONITOR_GLYPH_WIDTHS: [u8; 25] = [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 1, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 4, 3];

/// BlockEntityRenderHelper.renderItem2dWithAmount's text: centred 0.23 under the item, a font pixel 1/124 of a
/// block, black (fluix's contrast colour), full bright. The pose faces +z at the item's centre.
fn monitor_text(game: &Game<'_>, draw: &mut super::native_block_entity_draw::BlockEntityDraw, text: &str, spacing: f32, color: u32) {
    let glyphs: Vec<(usize, f32)> = text
        .chars()
        .filter_map(|ch| MONITOR_GLYPHS.find(ch).map(|i| (i, f32::from(MONITOR_GLYPH_WIDTHS[i]))))
        .collect();
    if glyphs.is_empty() {
        return;
    }
    let px = 1.0 / 124.0;
    let width: f32 = glyphs.iter().map(|(_, w)| w + 1.0).sum::<f32>() - 1.0;
    let (light, z) = (draw.light, 0.001);
    draw.light = 0xF0F0;
    let mut x = -width / 2.0 * px;
    let top = spacing;
    for (i, w) in glyphs {
        // The quad seen from the screen's front: top left, bottom left, bottom right, top right.
        let (x0, x1) = (x, x + w * px);
        let (y0, y1) = (top, top - 7.0 * px);
        // The sheet is 256 wide and 8 tall: a texel is 1/16 of the sprite's width, 2 of its height.
        let u0 = i as f32 * 8.0 / 16.0;
        let uv = [u0, 0.0, u0 + w / 16.0, 14.0];
        // Both windings: the glyph shows whichever way the turned pose leaves it facing.
        let drawn = game.native_draw_quad(draw, "ae2:gui/hz/monitor_font", [[x0, y0, z], [x0, y1, z], [x1, y1, z], [x1, y0, z]], uv, color);
        game.native_draw_quad(draw, "ae2:gui/hz/monitor_font", [[x1, y0, z], [x1, y1, z], [x0, y1, z], [x0, y0, z]], [uv[2], uv[1], uv[0], uv[3]], color);
        if !drawn {
            once("monitor text: the font sprite ae2:gui/hz/monitor_font is not in the GUI atlas");
        }
        x += (w + 1.0) * px;
    }
    draw.light = light;
}

/// The monitor's screen on the client: the shown item, flat on the part's front.
fn monitor_submit(game: &Game<'_>, state: horizon_world::registry::BlockStateId, entity: &ModBlockEntity, draw: &mut super::native_block_entity_draw::BlockEntityDraw) {
    let id = nbt_string(&entity.nbt, "show");
    if id.is_empty() {
        return;
    }
    let c = [0.5, 0.5, 0.5];
    draw.push();
    // The display's front looks north from 3 texels in front of the block's south side: turned to its facing.
    match game.native_block_state_property(state, "facing").unwrap_or("north") {
        "south" => draw.rotate_about([0.0, 1.0, 0.0], 180.0, c),
        "east" => draw.rotate_about([0.0, 1.0, 0.0], -90.0, c),
        "west" => draw.rotate_about([0.0, 1.0, 0.0], 90.0, c),
        "up" => draw.rotate_about([1.0, 0.0, 0.0], 90.0, c),
        "down" => draw.rotate_about([1.0, 0.0, 0.0], -90.0, c),
        _ => {}
    }
    // AE2's renderItem2d: the GUI look, flattened, just in front of the screen, 0.05 above its middle.
    draw.translate([0.5, 0.55, 12.9 / 16.0]);
    draw.rotate_about([0.0, 1.0, 0.0], 180.0, [0.0, 0.0, 0.0]);
    draw.push();
    monitor_text(game, draw, &nbt_string(&entity.nbt, "text"), -0.23, 0x000000);
    draw.pop();
    draw.scale([0.4, 0.4, 0.0001]);
    // Full bright: the part's block is solid, so the light inside it is none (AE2 lights the screen's item as a
    // screen is lit).
    let light = draw.light;
    draw.light = 0xF0F0;
    game.native_draw_item(draw, &ItemStack::new(&id, 1), horizon_assets::item_model::DisplayContext::Gui, None, 0);
    draw.light = light;
    draw.pop();
}

/// AEColor: name, its black, medium and white variants, and the text colour that reads over it.
const AE_COLORS: [(&str, u32, u32, u32, u32); 17] = [
    ("white", 12500670, 14408667, 16448250, 0),
    ("orange", 16357177, 16428612, 16047811, 0),
    ("magenta", 8527490, 12069560, 12949704, 0),
    ("light_blue", 6458827, 8563943, 14219007, 0),
    ("yellow", 16775082, 16318282, 16777192, 0),
    ("lime", 8191818, 12320593, 15202263, 0),
    ("pink", 14454197, 16299479, 16244459, 0),
    ("gray", 8158332, 10526880, 13224393, 0),
    ("light_gray", 10329501, 13487565, 15724527, 0),
    ("cyan", 3120037, 5352134, 11460084, 0),
    ("purple", 8532146, 10769358, 13083596, 0),
    ("blue", 2959776, 5327615, 14542591, 0),
    ("brown", 7491125, 12031615, 14734024, 0),
    ("green", 4562977, 6349614, 14938851, 0),
    ("red", 10813481, 16711740, 16770797, 0),
    ("black", 2829099, 5658198, 8684676, 16777215),
    ("fluix", 1778500, 9002152, 14138348, 0),
];

/// CraftingMonitorRenderer and MonitorBakedModel's front: the lights in the monitor's colour (formed), and the job's
/// item with how many are left, on the block's front.
fn crafting_monitor_submit(game: &Game<'_>, state: horizon_world::registry::BlockStateId, entity: &ModBlockEntity, draw: &mut super::native_block_entity_draw::BlockEntityDraw) {
    let paint = nbt_string(&entity.nbt, "paint");
    let color = AE_COLORS.iter().find(|c| c.0 == paint).copied().unwrap_or(AE_COLORS[16]);
    let c = [0.5, 0.5, 0.5];
    draw.push();
    // The pose: +z out of the block's front, as AE2's rotateToFace leaves it.
    match game.native_block_state_property(state, "facing").unwrap_or("north") {
        "north" => draw.rotate_about([0.0, 1.0, 0.0], 180.0, c),
        "east" => draw.rotate_about([0.0, 1.0, 0.0], 90.0, c),
        "west" => draw.rotate_about([0.0, 1.0, 0.0], -90.0, c),
        "up" => draw.rotate_about([1.0, 0.0, 0.0], -90.0, c),
        "down" => draw.rotate_about([1.0, 0.0, 0.0], 90.0, c),
        _ => {}
    }
    let light = draw.light;
    if game.native_block_state_property(state, FORMED_KEY) == Some("true") {
        let powered = game.native_block_state_property(state, "powered") == Some("true");
        if powered {
            draw.light = 0xF0F0;
        }
        let (a, b, z) = (0.0, 1.0, 1.0005);
        for (sprite, tint) in [("ae2:gui/hz/monitor_light_bright", color.3), ("ae2:gui/hz/monitor_light_medium", color.2), ("ae2:gui/hz/monitor_light_dark", color.1)] {
            game.native_draw_quad(draw, sprite, [[a, b, z], [a, a, z], [b, a, z], [b, b, z]], [0.0, 0.0, 16.0, 16.0], tint);
        }
        draw.light = light;
    }
    let id = nbt_string(&entity.nbt, "show");
    if !id.is_empty() {
        draw.translate([0.5, 0.52, 1.001]);
        draw.push();
        monitor_text(game, draw, &nbt_string(&entity.nbt, "text"), -0.18, color.4);
        draw.pop();
        draw.translate([0.0, 0.0, 0.0005]);
        draw.scale([0.3, 0.3, 0.0001]);
        draw.light = 0xF0F0;
        game.native_draw_item(draw, &ItemStack::new(&icon_of(&id), 1), horizon_assets::item_model::DisplayContext::Gui, None, 0);
        draw.light = light;
    }
    draw.pop();
}

/// MolecularAssemblerRenderer.renderPowerLight (molecular_assembler_lights: twelve frames, three ticks each, on a cube
/// inside the shell, full bright) and CraftingFx: every four ticks of a craft, sparks that shrink into the middle.
fn assembler_effects(game: &Game<'_>, state: horizon_world::registry::BlockStateId, entity: &ModBlockEntity, draw: &mut super::native_block_entity_draw::BlockEntityDraw, working: bool) {
    let light = draw.light;
    draw.light = 0xF0F0;
    if game.native_block_state_property(state, "powered") == Some("true") {
        let frame = (draw.game_time.div_euclid(3).rem_euclid(12)) as f32;
        let v = |t: f32| (frame + t / 16.0) * 16.0 / 12.0;
        let uv = [2.6, v(2.6), 13.4, v(13.4)];
        let (a, b) = (2.0 / 16.0, 14.0 / 16.0);
        for corners in [
            [[b, b, a], [b, a, a], [a, a, a], [a, b, a]],
            [[a, b, b], [a, a, b], [b, a, b], [b, b, b]],
            [[a, b, a], [a, a, a], [a, a, b], [a, b, b]],
            [[b, b, b], [b, a, b], [b, a, a], [b, b, a]],
            [[a, b, a], [a, b, b], [b, b, b], [b, b, a]],
            [[a, a, b], [a, a, a], [b, a, a], [b, a, b]],
        ] {
            game.native_draw_quad(draw, "ae2:gui/hz/molecular_assembler_lights", corners, uv, 0xFFFFFF);
        }
    }
    if working {
        // Stateless: where each spark is follows from when it started, so nothing runs on the ticks between.
        let speed = nbt_i64(&entity.nbt, "speed").max(10);
        let count = (speed as f32 / 5.0).ceil() as u32;
        let now = draw.game_time as f32 + draw.partial;
        let (yaw, pitch) = (game.yaw.to_radians(), game.pitch.to_radians());
        let forward = [-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()];
        let right = [-yaw.cos(), 0.0, -yaw.sin()];
        let up = [
            right[1] * forward[2] - right[2] * forward[1],
            right[2] * forward[0] - right[0] * forward[2],
            right[0] * forward[1] - right[1] * forward[0],
        ];
        let seed = std::ptr::from_ref(entity) as usize as u64;
        let newest = draw.game_time.div_euclid(4);
        for step in 0..10 {
            let born = newest - step;
            for spark in 0..count {
                let mut bits = seed ^ (born as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(spark).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
                let mut next = || {
                    bits ^= bits << 13;
                    bits ^= bits >> 7;
                    bits ^= bits << 17;
                    (bits >> 40) as f32 / (1u64 << 24) as f32
                };
                let _ = next();
                let off = [next() - 0.5, next() - 0.5, next() - 0.5];
                let length = (off[0] * off[0] + off[1] * off[1] + off[2] * off[2]).sqrt().max(0.001);
                let life = (4.0 / (next() * 0.9 + 0.1) / 1.2).floor().max(1.0);
                let f = (now - (born * 4) as f32) / life;
                if !(0.0..1.0).contains(&f) {
                    continue;
                }
                let eased = (1.0 - (f - 1.0) * (f - 1.0)).sqrt();
                let size = 0.13 * (1.0 - eased);
                let at = off.map(|o| 0.5 + o / length * 0.35 * (1.0 - f));
                let corner = |r: f32, u: f32| [0, 1, 2].map(|i| at[i] + right[i] * r * size + up[i] * u * size);
                game.native_draw_quad(draw, "ae2:gui/hz/crafting_fx", [corner(-1.0, 1.0), corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0)], [0.0, 0.0, 16.0, 16.0], 0xFFE6FF);
            }
        }
    }
    draw.light = light;
}

/// Places the held block against `pos`'s `face`, as a block item does on a block that doesn't use the click.
fn place_held_against(game: &mut Game<'_>, pos: Pos, face: Pos) {
    let Some(stack) = game.held().cloned() else { return };
    let target = add(pos, face);
    if !matches!(block_id(game, target).as_str(), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air" | "minecraft:water" | "minecraft:short_grass") {
        return;
    }
    let Some(state) = game.pack.assets.registry.default_state(&stack.item) else { return };
    let Some(player) = game.native_acting_player() else { return };
    let dim = game.dim.id().to_owned();
    if !game.native_set_block(&dim, target, state, 3) {
        return;
    }
    if player.game_mode != 1 {
        let hand = game.held_mut();
        if let Some(h) = hand.as_mut() {
            h.count -= 1;
            if h.count == 0 {
                *hand = None;
            }
        }
    }
    placed(game, target);
}

// ---------------------------------------------------------------------------------------------------------
// AE2's ME chest terminal (ChestBlock.onActivated): a click on the chest's top opens the inserted cell's own
// terminal (its contents only), any other face the cell slot. Here it is the wireless terminal's screen with the
// chest's cell behind it.

/// A right-click on the chest's top face: its cell's terminal, or AE2's message when there's no cell to read.
fn chest_top_click(game: &mut Game<'_>, pos: Pos, face: Pos) -> bool {
    let facing = facing_dir(game.native_block_property(pos, "facing").as_deref());
    // The chest's top: up, unless it faces up or down (then the way its front's top points, north).
    let top = if facing[1] != 0 { [0, 0, -1] } else { [0, 1, 0] };
    if face != top || sneaking(game) {
        return false;
    }
    let Some(player) = game.native_acting_player() else { return true };
    let readable = game
        .native_with_mod_block_entity(pos, |e| e.items.first().cloned().flatten())
        .flatten()
        .is_some_and(|c| cell_type(&c.item).is_some());
    let online = SERVER.with_borrow(|s| s.grid_of.get(&pos).and_then(|g| s.grids.get(*g)).is_some_and(|g| g.online && g.active.contains(&pos)));
    if !readable || !online {
        game.native_action_bar(player.uuid, horizon_client::text::Text::legacy("ME Chest cannot read storage cell."));
        return true;
    }
    SERVER.with_borrow_mut(|s| {
        s.viewers.insert(player.uuid, pos);
        s.sent.remove(&player.uuid);
        s.wireless.insert(player.uuid);
        s.chest_terms.insert(player.uuid);
    });
    let _ = game.native_net_send_to_player(player.uuid, NET_WIRELESS, b"open");
    true
}

/// The chest's cell as a storage of its own, written back onto the cell afterwards (its id cleared, so a
/// network that has it mounted reads it again).
fn with_chest_cell<R>(
    game: &mut Game<'_>,
    pos: Pos,
    f: impl FnOnce(&mut NetworkStorage<Box<dyn super::port_ae2_grid::MeStorage>>) -> R,
) -> Option<R> {
    let mut stack = game.native_with_mod_block_entity(pos, |e| e.items.first().cloned().flatten()).flatten()?;
    let cell = cell_type(&stack.item)?;
    let inv = BasicCellInventory::new(cell, &cell_config(&stack), cell_fuzzy(&stack), cell_upgrades(&stack), &read_cell(&stack));
    let mut storage: NetworkStorage<Box<dyn super::port_ae2_grid::MeStorage>> = NetworkStorage::new();
    storage.mount(0, 0, Box::new(inv));
    let r = f(&mut storage);
    if let Some(mut inv) = storage.unmount(0)
        && let Some(inv) = inv.as_any().downcast_mut::<BasicCellInventory>()
        && let Some(contents) = inv.persist()
    {
        write_cell(&mut stack, &contents);
        cell_changed(&mut stack);
        game.native_with_mod_block_entity(pos, |e| {
            if let Some(slot) = e.items.first_mut() {
                *slot = Some(stack);
            }
        });
    }
    Some(r)
}

/// The storage a player's terminal screen works on: the network's, or an ME chest's cell.
fn with_viewer_storage<R>(
    game: &mut Game<'_>,
    uuid: u128,
    f: impl FnOnce(&mut NetworkStorage<Box<dyn super::port_ae2_grid::MeStorage>>) -> R,
) -> Option<R> {
    let (pos, chest) = SERVER.with_borrow(|s| (s.viewers.get(&uuid).copied(), s.chest_terms.contains(&uuid)));
    let pos = pos?;
    if chest {
        return with_chest_cell(game, pos, f);
    }
    let grid = SERVER.with_borrow(|s| s.grid_of.get(&pos).copied())?;
    Some(with_storage(game, grid, None, f))
}

/// The chest terminal's list: the cell's contents, most first.
fn chest_items(game: &mut Game<'_>, pos: Pos) -> Vec<(AeKey, i64)> {
    let mut items: Vec<(AeKey, i64)> = with_chest_cell(game, pos, |st| st.contents().iter().map(|(k, a)| (k.clone(), a)).filter(|(_, a)| *a > 0).collect())
        .unwrap_or_default();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.id().cmp(b.0.id())));
    items
}

/// A plane's model axes in the world (the blockstate's turn for its facing): which way its +x and +y point.
fn plane_axes(facing: &str) -> (Pos, Pos) {
    match facing {
        "south" => ([-1, 0, 0], [0, 1, 0]),
        "west" => ([0, 0, -1], [0, 1, 0]),
        "east" => ([0, 0, 1], [0, 1, 0]),
        "up" => ([1, 0, 0], [0, 0, 1]),
        "down" => ([1, 0, 0], [0, 0, -1]),
        _ => ([1, 0, 0], [0, 1, 0]),
    }
}

/// PlaneConnectionHelper.getConnections: each edge of the plate reaches the block's edge where the same plane
/// faces the same way beside it.
fn plane_links(game: &mut Game<'_>, pos: Pos, block: &str) {
    let facing = game.native_block_property(pos, "facing").unwrap_or_default();
    let (x, y) = plane_axes(&facing);
    let neg = |d: Pos| [-d[0], -d[1], -d[2]];
    for (key, d) in [("link_xp", x), ("link_xn", neg(x)), ("link_yp", y), ("link_yn", neg(y))] {
        let q = add(pos, d);
        let joined = block_id(game, q) == block && game.native_block_property(q, "facing").as_deref() == Some(facing.as_str());
        let want = if joined { "true" } else { "false" };
        if game.native_block_property(pos, key).is_some_and(|v| v != want) {
            game.native_set_block_property_with(pos, key, want, 2);
        }
    }
}

/// The cable in a bus, plane or tunnel's block (its block entity's `wire`: the cable block and the sides it
/// joins): that cable's own model, its centre and an arm to each.
fn part_cable_submit(game: &Game<'_>, state: horizon_world::registry::BlockStateId, entity: &ModBlockEntity, draw: &mut super::native_block_entity_draw::BlockEntityDraw) {
    let _ = state;
    let wire = nbt_string(&entity.nbt, "wire");
    let Some((block, sides)) = wire.split_once(' ') else { return };
    let arms: Vec<(&str, &str)> = sides.split(',').filter(|s| !s.is_empty()).map(|s| (s, "true")).collect();
    game.native_draw_block(draw, block, &arms);
}

/// A cable's arms: toward what it joins (AE2's rules, `links`): a part or the crystal resonance generator only
/// at its back, so a cable beside, under or in front of a part draws no arm to it (Horizon's cable tag would).
fn cable_sides(game: &mut Game<'_>, p: Pos) {
    let name = block_id(game, p);
    if cable(&name).is_none() {
        return;
    }
    let twin = is_twin(&game.native_block(p));
    let mut arms = 0u32;
    for (i, (d, key)) in SIDES.iter().zip(TWIN_SIDES).enumerate() {
        let q = add(p, *d);
        let other = block_id(game, q);
        let joined = is_network_block(&other) && links(game, p, &name, q, &other);
        if joined {
            arms |= 1 << i;
        }
        let want = if joined { "true" } else { "false" };
        if !twin && game.native_block_property(p, key).is_some_and(|v| v != want) {
            game.native_set_block_property_with(p, key, want, 2);
        }
    }
    if twin
        && let Some((_, now, facades)) = twin_masks(game, p)
        && now != arms
        && let Some(cable) = twin_cable(game, p)
    {
        place_twin(game, p, &cable, arms, facades);
    }
}

/// AE2's illuminated panels give light 15 while powered (AbstractReportingPart.getLightLevel): the panel becomes
/// its lit twin, facing kept, and back.
fn panel_light(game: &mut Game<'_>, p: Pos, on: bool) {
    let block = block_id(game, p);
    let Some(name) = ae2_name(&block).filter(|n| matches!(*n, "monitor" | "semi_dark_monitor" | "dark_monitor")) else { return };
    let want = if on { format!("ae2:hz_{name}_lit") } else { format!("ae2:{name}") };
    if block == want {
        return;
    }
    let facing = game.native_block_property(p, "facing").unwrap_or_else(|| "north".to_owned());
    let Some(state) = game.pack.assets.registry.default_state(&want) else { return };
    let dim = game.dim.id().to_owned();
    if game.native_set_block(&dim, p, state, 3) {
        game.native_set_block_property_with(p, "facing", &facing, 3);
        game.native_set_block_property_with(p, "powered", if on { "true" } else { "false" }, 3);
    }
}

/// Level.getSkyDarken: how much darker the sky is than full day (0 to 11), from the time and the weather.
fn sky_darken(game: &Game<'_>) -> i32 {
    let day = game.native_day_time().rem_euclid(24000) as f64;
    // DimensionType.timeOfDay.
    let frac = (day / 24000.0 - 0.25).rem_euclid(1.0);
    let f = 0.5 - (frac * std::f64::consts::PI).cos() / 2.0;
    let angle = (frac * 2.0 + f) / 3.0;
    let (rain, thunder) = game.native_weather();
    let mut d = 1.0 - ((angle * std::f64::consts::TAU).cos() * 2.0 + 0.5);
    d = d.clamp(0.0, 1.0);
    d = 1.0 - d;
    d *= 1.0 - f64::from(rain) * 5.0 / 16.0;
    d *= 1.0 - f64::from(thunder) * 5.0 / 16.0;
    d = 1.0 - d;
    (d * 11.0) as i32
}

/// LightDetectorBlockEntity: every 30 ticks the light where it stands (the brightest of block light and the
/// darkened sky), its signal that less 6; neighbours told when it changes.
fn light_detector_tick(game: &mut Game<'_>, pos: Pos, entity: &mut ModBlockEntity) {
    let n = nbt_i64(&entity.nbt, "check") + 1;
    if n <= 30 {
        set_nbt_i64(&mut entity.nbt, "check", n);
        return;
    }
    set_nbt_i64(&mut entity.nbt, "check", 0);
    let (block, sky) = game.native_light(pos);
    let light = i32::from(block).max(i32::from(sky) - sky_darken(game));
    let signal = if light > 0 { (light - 6).max(0) } else { 0 };
    let was = SERVER.with_borrow_mut(|s| s.light_signals.insert(pos, signal));
    if was != Some(signal) {
        SERVER.with_borrow_mut(|s| s.signal_updates.push(pos));
    }
}

/// Toggle buses: which join their front, from the redstone beside them.
fn toggle_tick(game: &mut Game<'_>, pos: Pos, entity: &ModBlockEntity) {
    let dim = game.dim.id().to_owned();
    let powered = game.native_neighbor_signal(&dim, pos) > 0;
    let on = if ae2_name(&entity.id) == Some("inverted_toggle_bus") { !powered } else { powered };
    SERVER.with_borrow_mut(|s| {
        if on {
            s.toggles_on.insert(pos);
        } else {
            s.toggles_on.remove(&pos);
        }
    });
}

fn is_creative_cell(item: &str) -> bool {
    matches!(ae2_name(item), Some("creative_item_cell" | "creative_fluid_cell"))
}

/// AE2's creative ME cell: every key its partition names, Integer.MAX_VALUE of it shown, taken or stored without
/// end; anything else refused.
struct CreativeCell {
    keys: Vec<AeKey>,
}

impl super::port_ae2_grid::MeStorage for CreativeCell {
    fn insert(&mut self, what: &AeKey, amount: i64, _mode: Actionable) -> i64 {
        if self.keys.contains(what) { amount } else { 0 }
    }
    fn extract(&mut self, what: &AeKey, amount: i64, _mode: Actionable) -> i64 {
        if self.keys.contains(what) { amount } else { 0 }
    }
    fn available_stacks(&self, out: &mut super::port_ae2_core::KeyCounter) {
        for k in &self.keys {
            out.add(k, i64::from(i32::MAX));
        }
    }
    fn is_preferred_for(&self, what: &AeKey) -> bool {
        self.keys.contains(what)
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The view cell slots of AE2's terminals (SlotSemantics.VIEW_CELL), after their own slots.
const VIEW_CELLS: usize = 5;

fn view_cell_slots(block: &str) -> Option<std::ops::Range<usize>> {
    let first = match ae2_name(block)? {
        "terminal" => 1,
        "crafting_terminal" => 11,
        "pattern_encoding_terminal" => 2,
        _ => return None,
    };
    Some(first..first + VIEW_CELLS)
}

/// ViewCellItem.createFilter: each view cell's partition lets only its keys show (an inverter card: all but
/// them; a fuzzy card: by damage, its fuzzy mode); none configured, everything.
fn view_filter(game: &mut Game<'_>, pos: Pos, block: &str, items: Vec<(AeKey, i64)>) -> Vec<(AeKey, i64)> {
    let Some(range) = view_cell_slots(block) else { return items };
    let cells: Vec<ItemStack> = game
        .native_with_mod_block_entity(pos, |e| range.clone().filter_map(|i| e.items.get(i).cloned().flatten()).collect())
        .unwrap_or_default();
    let mut lists: Vec<(Vec<AeKey>, bool, Option<FuzzyMode>)> = Vec::new();
    for cell in cells.iter().filter(|c| ae2_name(&c.item) == Some("view_cell")) {
        let keys = cell_config(cell);
        if keys.is_empty() {
            continue;
        }
        let ups = cell_upgrades(cell);
        lists.push((keys, !ups.inverter, ups.fuzzy.then(|| cell_fuzzy(cell))));
    }
    if lists.is_empty() {
        return items;
    }
    let listed = |key: &AeKey, keys: &[AeKey], fuzzy: Option<FuzzyMode>| match fuzzy {
        Some(mode) => keys.iter().any(|k| k.fuzzy_equals(key, mode)),
        None => keys.contains(key),
    };
    let any_positive = lists.iter().any(|(_, white, _)| *white);
    items
        .into_iter()
        .filter(|(key, _)| {
            // MergedPriorityList.isListed: a deny list wins; then any allow list, if there are any.
            if lists.iter().any(|(keys, white, f)| !*white && listed(key, keys, *f)) {
                return false;
            }
            !any_positive || lists.iter().any(|(keys, white, f)| *white && listed(key, keys, *f))
        })
        .collect()
}

/// What the terminal's view cells are (their configs), as a number that changes when they do.
fn view_sig(game: &mut Game<'_>, pos: Pos, block: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let Some(range) = view_cell_slots(block) else { return 0 };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    game.native_with_mod_block_entity(pos, |e| {
        for i in range {
            if let Some(Some(c)) = e.items.get(i) {
                c.item.hash(&mut h);
                workbench_cells(Some(c)).hash(&mut h);
                cell_cards(c).hash(&mut h);
            }
        }
    });
    h.finish()
}

// ---------------------------------------------------------------------------------------------------------
// AE2's sky stone tank (SkyStoneTankBlockEntity): sixteen buckets of one fluid, filled and drained by buckets and
// by anything that moves fluids, the fluid drawn inside its glass at its level.

const TANK_MB: i32 = 16_000;

/// `native_block_entity_fluids`: the tank, reached from every side.
pub fn block_entity_fluids(block: &str, fluids: &mut Option<horizon_client::containers::ModFluidTanks>) {
    if block == "ae2:sky_stone_tank" {
        *fluids = Some(horizon_client::containers::ModFluidTanks {
            tanks: vec![TANK_MB],
            faces: std::array::from_fn(|_| vec![0]),
            inputs: vec![0],
            outputs: vec![0],
        });
    }
}

/// The tank's fluid and amount, from its saved `fluids`.
fn tank_fluid(nbt: &Compound) -> Option<(String, i32)> {
    let Some(Tag::List(list)) = nbt.get(&java("fluids")) else { return None };
    let Some(Tag::Compound(t)) = list.first() else { return None };
    let fluid = match t.get(&java("Fluid")) {
        Some(Tag::String(s)) => s.to_string().ok()?,
        _ => return None,
    };
    let amount = match t.get(&java("Amount")) {
        Some(Tag::Int(n)) => *n,
        _ => 0,
    };
    (amount > 0).then_some((fluid, amount))
}

/// FluidStorageUtil.interactWithFluidStorage with a bucket: a full one empties into the tank when a whole bucket
/// fits, an empty one fills from it when it holds a bucket. True when the hand held a bucket.
fn tank_bucket(game: &mut Game<'_>, pos: Pos) -> bool {
    use horizon_core::geometry::Direction;
    let Some(held) = game.held().cloned() else { return false };
    let Some(player) = game.native_acting_player() else { return true };
    let creative = player.game_mode == 1;
    let full = match &*held.item {
        "minecraft:water_bucket" => Some("minecraft:water"),
        "minecraft:lava_bucket" => Some("minecraft:lava"),
        "minecraft:bucket" => None,
        _ => return false,
    };
    let swap = |game: &mut Game<'_>, to: &str| {
        if creative {
            return;
        }
        let hand = game.held_mut();
        if let Some(h) = hand.as_mut() {
            h.count -= 1;
            if h.count == 0 {
                *hand = None;
            }
        }
        if let Some(left) = game.native_give_stack(player.uuid, ItemStack::new(to, 1)) {
            *game.held_mut() = Some(left);
        }
    };
    match full {
        Some(fluid) => {
            // Only a whole bucket: what doesn't fit is taken back out.
            let left = game.native_fluid_insert(pos, Direction::Up, fluid, 1000);
            if left == 0 {
                swap(game, "minecraft:bucket");
            } else if left < 1000 {
                let dim = game.dim.id().to_owned();
                game.native_fluid_extract(&dim, pos, Direction::Up, 1000 - left);
            }
        }
        None => {
            let dim = game.dim.id().to_owned();
            let have = game.native_with_mod_block_entity(pos, |e| tank_fluid(&e.nbt)).flatten();
            if have.as_ref().is_some_and(|(_, n)| *n >= 1000)
                && let Some((fluid, 1000)) = game.native_fluid_extract(&dim, pos, Direction::Up, 1000)
            {
                swap(game, if fluid == "minecraft:lava" { "minecraft:lava_bucket" } else { "minecraft:water_bucket" });
            }
        }
    }
    game.native_block_entity_sync(pos);
    true
}

/// SkyStoneTankBlockEntityRenderer.drawFluidInTank: the fluid's still texture in a box inside the glass,
/// 0.0635 to 0.9365 across, from the floor up to its fill.
fn tank_submit(game: &Game<'_>, entity: &ModBlockEntity, draw: &mut super::native_block_entity_draw::BlockEntityDraw) {
    use super::native_block_entity_draw::CubeFace;
    let Some((fluid, amount)) = tank_fluid(&entity.nbt) else { return };
    let (sprite, tint) = if fluid == "minecraft:lava" { ("block/lava_still", 0xFFFFFF) } else { ("block/water_still", 0x3F76E4) };
    let fill = 0.0635 + 0.873 * (amount as f32 / TANK_MB as f32).clamp(0.0, 1.0);
    let face = Some(CubeFace { sprite, uv: None });
    let lo = 0.0635 * 16.0;
    let hi = 0.9365 * 16.0;
    if fluid == "minecraft:lava" {
        let light = draw.light;
        draw.light = 0xF0F0;
        game.native_draw_cube(draw, [lo, lo, lo], [hi, fill * 16.0, hi], [face, face, face, face, face, face], tint);
        draw.light = light;
    } else {
        game.native_draw_cube(draw, [lo, lo, lo], [hi, fill * 16.0, hi], [face, face, face, face, face, face], tint);
    }
}

// ---------------------------------------------------------------------------------------------------------
// AE2's pattern provider settings (PatternProviderLogic, PatternProviderScreen): blocking mode, lock crafting
// (LockCraftingMode NONE, UNTIL_PULSE, WHILE_HIGH, WHILE_LOW, UNTIL_RESULT), shown on the pattern access terminal,
// and the return inventory.

const PP_BLOCKING: i32 = 2;
const PP_LOCK: i32 = 3;
const PP_ACCESS: i32 = 4;
/// The priority tab (the screen's `openPriority` widget).
const PP_PRIORITY: [f32; 2] = [154.0, 0.0];
/// SettingToggleButton's icons for each LockCraftingMode: UNLOCKED, REDSTONE_PULSE, REDSTONE_LOW, REDSTONE_HIGH, ENTER.
const PP_LOCK_ICONS: [[f32; 2]; 5] = [[160.0, 0.0], [32.0, 0.0], [0.0, 0.0], [16.0, 0.0], [112.0, 0.0]];

/// The left toolbar, top down.
fn pp_toolbar() -> Vec<(i32, [f32; 2])> {
    [PP_BLOCKING, PP_LOCK, PP_ACCESS].iter().enumerate().map(|(i, b)| (*b, [-18.0, 8.0 + 20.0 * i as f32])).collect()
}

/// A number from a provider's saved settings, read without changing it.
fn provider_nbt(game: &Game<'_>, pos: Pos, key: &str) -> i64 {
    match game.world.block_entities.get(&(pos[0], pos[1], pos[2])) {
        Some(horizon_client::containers::BlockEntity::Mod(m)) => nbt_i64(&m.nbt, key),
        _ => 0,
    }
}

/// PatternProviderLogic.getCraftingLockedReason as a LockCraftingMode number.
fn lock_reason(mode: i64, high: bool, event: i64) -> i64 {
    if mode == 3 && !high {
        3
    } else if mode == 2 && high {
        2
    } else if event == 1 || event == 3 {
        1
    } else if event == 2 {
        4
    } else {
        0
    }
}

/// The provider's tick: redstone (updateRedstoneState), the return inventory into the network
/// (injectIntoNetwork, which may unlock it), the lock reason and the screen's data.
fn pattern_provider_tick(game: &mut Game<'_>, pos: Pos, grid: Option<usize>, entity: &mut ModBlockEntity) {
    entity.items.resize(18, None);
    let dim = game.dim.id().to_owned();
    let high = game.native_neighbor_signal(&dim, pos) > 0;
    let mut event = nbt_i64(&entity.nbt, "unlock_event");
    if (nbt_i64(&entity.nbt, "rs") != 0) != high {
        set_nbt_i64(&mut entity.nbt, "rs", i64::from(high));
        if event == 1 && high {
            event = 0;
        } else if event == 3 && !high {
            event = 1;
        }
    }
    if let Some(g) = grid {
        for slot in 9..18 {
            let Some(stack) = entity.items[slot].take() else { continue };
            let key = key_of(game, &stack);
            let stored = with_storage(game, g, None, |st| st.insert(&key, i64::from(stack.count), Actionable::Modulate));
            if stored > 0 && event == 2 {
                event = returned_to(entity, &key, stored);
            }
            let left = stack.count.saturating_sub(stored as u32);
            if left > 0 {
                entity.items[slot] = Some(ItemStack { count: left, ..stack });
            }
        }
    }
    set_nbt_i64(&mut entity.nbt, "unlock_event", event);
    let mode = nbt_i64(&entity.nbt, "lock_mode");
    let reason = lock_reason(mode, high, event);
    set_nbt_i64(&mut entity.nbt, "lock_reason", reason);
    entity.data.resize(4, 0);
    entity.data[0] = nbt_i64(&entity.nbt, "blocking") as i32;
    entity.data[1] = mode as i32;
    entity.data[2] = nbt_i64(&entity.nbt, "hide") as i32;
    entity.data[3] = reason as i32;
}

/// onStackReturnedToNetwork: what came back counts down the result it waits for; all of it unlocks.
fn returned_to(entity: &mut ModBlockEntity, key: &AeKey, amount: i64) -> i64 {
    let until = nbt_string(&entity.nbt, "lock_until");
    let (id, left) = until.rsplit_once(' ').unwrap_or((&until, "0"));
    if wire_id(key) != id {
        return 2;
    }
    let left = left.parse::<i64>().unwrap_or(0) - amount;
    if left <= 0 {
        entity.nbt.remove(&java("lock_until"));
        0
    } else {
        entity.nbt.insert(java("lock_until"), Tag::String(java(&format!("{id} {left}"))));
        2
    }
}

/// A machine's output taken back through the provider at `pos` (if it is one waiting for it).
fn provider_returned(game: &mut Game<'_>, pos: Pos, key: &AeKey, amount: i64) {
    if canon(&block_id(game, pos)) != "ae2:pattern_provider" || provider_nbt(game, pos, "unlock_event") != 2 {
        return;
    }
    game.native_with_mod_block_entity(pos, |e| {
        let event = returned_to(e, key, amount);
        set_nbt_i64(&mut e.nbt, "unlock_event", event);
        if event == 0 {
            let reason = lock_reason(nbt_i64(&e.nbt, "lock_mode"), nbt_i64(&e.nbt, "rs") != 0, 0);
            set_nbt_i64(&mut e.nbt, "lock_reason", reason);
        }
    });
}

/// onPushPatternSuccess: UNTIL_PULSE waits for the next pulse (a fall first if the signal is on), UNTIL_RESULT
/// for the pattern's primary output. The lock holds from this push on.
fn provider_pushed(game: &mut Game<'_>, pos: Pos, pattern: &Pattern) {
    let out = pattern.primary_output().clone();
    game.native_with_mod_block_entity(pos, |e| {
        let high = nbt_i64(&e.nbt, "rs") != 0;
        let mode = nbt_i64(&e.nbt, "lock_mode");
        e.nbt.remove(&java("lock_until"));
        let event = match mode {
            1 => if high { 3 } else { 1 },
            4 => {
                e.nbt.insert(java("lock_until"), Tag::String(java(&format!("{} {}", wire_id(&out.what), out.amount))));
                2
            }
            _ => 0,
        };
        set_nbt_i64(&mut e.nbt, "unlock_event", event);
        set_nbt_i64(&mut e.nbt, "lock_reason", lock_reason(mode, high, event));
    });
}

/// ItemMenuHost.drainPower for an open wireless terminal: every ten ticks, ten ticks of its drain (AE2's
/// `wirelessTerminalDrainMultiplier` 1 times the distance to the access point) from its charge; out of power or
/// out of range closes the terminal (creative players use none).
fn wireless_drain(game: &mut Game<'_>, player: u128, creative: bool, at: [f64; 3], slot: i32) {
    let (using, tick, wap) = SERVER.with_borrow(|s| (s.wireless.contains(&player) && !s.chest_terms.contains(&player), s.tick, s.viewers.get(&player).copied()));
    if !using || tick % 10 != 0 {
        return;
    }
    let Some(wap) = wap else { return };
    let Some(stack) = game.native_carried_stack(slot) else { return };
    if !is_wireless_terminal(&stack.item) || link_of(stack) != Some(wap) {
        return;
    }
    let range = game.native_with_mod_block_entity(wap, |e| wap_range(e)).unwrap_or(16.0);
    let distance = ((at[0] - wap[0] as f64 - 0.5).powi(2) + (at[1] - wap[1] as f64 - 0.5).powi(2) + (at[2] - wap[2] as f64 - 0.5).powi(2)).sqrt();
    let Some(stack) = game.native_carried_stack(slot) else { return };
    let powered = use_item_power(stack, 10.0 * distance, creative);
    if powered && distance <= range {
        return;
    }
    SERVER.with_borrow_mut(|s| {
        s.wireless.remove(&player);
        s.viewers.remove(&player);
    });
    let _ = game.native_net_send_to_player(player, NET_WIRELESS, b"close");
    let text = if powered { "Wireless Out Of Range." } else { "Device is low on power." };
    game.native_action_bar(player, horizon_client::text::Text::legacy(text));
}

// ---------------------------------------------------------------------------------------------------------
// AE2's charged staff (ChargedStaffItem.hurtEnemy): with more than 300 AE it spends 300 on a hit, sparks about
// the target (LightningPacket) and hurts it 6 magic damage on top (vanilla's invulnerable time keeps the larger
// of the two hits).

/// Mobs a charged staff hit, waiting for the damage that blow does.
static STAFF_HITS: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

/// `native_attack_entity`.
pub fn attack_entity(game: &mut Game<'_>, entity: i32, pos: [f64; 3]) {
    if game.held().map(|h| &*h.item) != Some("ae2:charged_staff") || held_power(game) <= 300.0 {
        return;
    }
    spend_held_power(game, 300.0);
    if let Ok(mut hits) = STAFF_HITS.lock() {
        hits.push(entity);
    }
    let spark = horizon_client::particles::ParticleOptions::simple(horizon_client::particles::ParticleType::ElectricSpark);
    for i in 0..12 {
        let f = f64::from(i) / 12.0;
        let a = f * std::f64::consts::TAU;
        game.particles.add(&spark, [pos[0] + 0.3 * a.cos(), pos[1] + 0.3 + f, pos[2] + 0.3 * a.sin()], [0.0; 3]);
    }
}

/// `native_living_incoming_damage`: the staff's 6 magic damage on the blow it landed.
pub fn incoming_damage(entity: i32, amount: &mut f32) {
    let hit = STAFF_HITS.lock().ok().is_some_and(|mut hits| match hits.iter().position(|e| *e == entity) {
        Some(i) => {
            hits.remove(i);
            true
        }
        None => false,
    });
    if hit {
        *amount = amount.max(6.0);
    }
}

/// AE2's charger recipes (data/ae2/recipes/charger): what each input becomes.
fn charger_result(item: &str) -> Option<&'static str> {
    match item {
        "ae2:certus_quartz_crystal" => Some("ae2:charged_certus_quartz_crystal"),
        "minecraft:book" => Some("ae2:guide"),
        "minecraft:compass" => Some("ae2:meteorite_compass"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------------------
// AE2's tiny TNT (TinyTNTBlock, TinyTNTPrimedEntity): lit by flint and steel or redstone, it hops and burns for 80
// ticks, then blows: 6 explosion damage to what is within 1.5 blocks and its own small crater (strength 2.3 less the
// squared distance and 0.11 of each block's resistance + 0.3). In water it goes out and drops as an item.

fn tiny_tnt_tick(game: &mut Game<'_>) {
    let dim = game.dim.id().to_owned();
    let primes = SERVER.with_borrow_mut(|s| std::mem::take(&mut s.tnt_primes));
    for pos in primes {
        if block_id(game, pos) != "ae2:tiny_tnt" {
            continue;
        }
        if let Some(air) = game.pack.assets.registry.default_state("minecraft:air") {
            game.native_set_block(&dim, pos, air, 3);
        }
        // TinyTNTPrimedEntity: a random sideways nudge, 0.2 up, a fuse of 80.
        let angle = {
            use horizon_core::random::RandomSource;
            f64::from(game.particles.random().next_float()) * std::f64::consts::TAU
        };
        let at = horizon_core::geometry::Vec3::new(f64::from(pos[0]) + 0.5, f64::from(pos[1]) + 0.5, f64::from(pos[2]) + 0.5);
        SERVER.with_borrow_mut(|s| {
            s.tiny_tnts.push(super::tnt::PrimedTnt {
                id: 0,
                position: at,
                previous: at,
                velocity: horizon_core::geometry::Vec3::new(-angle.sin() * 0.02, 0.2, -angle.cos() * 0.02),
                fuse: 80,
                on_ground: false,
                owner: None,
                guest: None,
            })
        });
        game.audio.play("minecraft:entity.tnt.primed", Some([at.x, at.y, at.z]), 1.0, 1.0);
    }
    let mut lit = SERVER.with_borrow_mut(|s| std::mem::take(&mut s.tiny_tnts));
    if lit.is_empty() {
        return;
    }
    let mut keep = Vec::new();
    for mut t in lit.drain(..) {
        let _ = t.tick(&game.world, game.collision, &game.pack.assets, game.res.facts());
        let at = [t.position.x, t.position.y, t.position.z];
        let cell = [at[0].floor() as i32, at[1].floor() as i32, at[2].floor() as i32];
        if block_id(game, cell) == "minecraft:water" {
            game.spawn_item(ItemStack::new("ae2:tiny_tnt", 1), at, [t.velocity.x, t.velocity.y, t.velocity.z], 10);
            continue;
        }
        if t.fuse <= 0 {
            tiny_tnt_explode(game, &dim, at);
        } else {
            let smoke = horizon_client::particles::ParticleOptions::simple(horizon_client::particles::ParticleType::Smoke);
            game.particles.add(&smoke, [at[0], at[1] + 0.25, at[2]], [0.0; 3]);
            keep.push(t);
        }
    }
    SERVER.with_borrow_mut(|s| s.tiny_tnts.extend(keep));
}

/// TinyTNTPrimedEntity.explode.
fn tiny_tnt_explode(game: &mut Game<'_>, dim: &str, at: [f64; 3]) {
    use horizon_client::mobs::{Damage, DamageKind};
    game.audio.play("minecraft:entity.generic.explode", Some(at), 4.0, 2.0);
    let boom = horizon_client::particles::ParticleOptions::simple(horizon_client::particles::ParticleType::Explosion);
    game.particles.add(&boom, at, [0.0; 3]);
    // What the blast hurts: mobs and the player in the 3x3x3 box round it, items (a singularity beside ender dust
    // entangles: AE2's SingularityEntity hears the blast).
    explosion(game, dim, at);
    let inside = |b: horizon_core::geometry::Aabb| {
        let (lo, hi) = (b.min_position(), b.max_position());
        hi.x > at[0] - 1.5 && lo.x < at[0] + 1.5 && hi.y > at[1] - 1.5 && lo.y < at[1] + 1.5 && hi.z > at[2] - 1.5 && lo.z < at[2] + 1.5
    };
    for m in game.mobs.iter_mut().filter(|m| m.alive() && inside(m.bounds())) {
        m.damage(6.0, Damage { kind: DamageKind::Explosion, attacker: None, from: Some(at) });
    }
    if game.player_view().is_some_and(|p| p.alive && inside(p.bounds)) {
        game.hurt_player_by("minecraft:explosion", 6.0);
    }
    // isTinyTntBlockDamageEnabled (on by default): the crater, from 0.25 lower.
    let c = [at[0], at[1] - 0.25, at[2]];
    let air = game.pack.assets.registry.default_state("minecraft:air");
    for x in (c[0] - 2.0) as i32..=(c[0] + 2.0) as i32 {
        for y in (c[1] - 2.0) as i32..=(c[1] + 2.0) as i32 {
            for z in (c[2] - 2.0) as i32..=(c[2] + 2.0) as i32 {
                let state = game.world.block(x, y, z);
                let facts = game.res.facts().get(state);
                if facts.name.ends_with(":air") {
                    continue;
                }
                let d = (f64::from(x) + 0.5 - c[0]).powi(2) + (f64::from(y) + 0.5 - c[1]).powi(2) + (f64::from(z) + 0.5 - c[2]).powi(2);
                let fluid = match &*facts.name {
                    "minecraft:water" | "minecraft:lava" => 100.0,
                    _ => 0.0,
                };
                let resistance = f64::from(facts.explosion_resistance).max(fluid);
                let strength = 2.3 - d - (resistance + 0.3) * 0.11;
                if strength <= 0.01 {
                    continue;
                }
                // Block.dropResources, then air.
                let drops = {
                    let items = game.res.items();
                    match items.block_loot_table(&game.pack.assets.registry, state) {
                        Some(table) => {
                            let mut random = horizon_core::random::LegacyRandom::new(((x as i64) << 24) ^ ((y as i64) << 12) ^ z as i64 ^ SERVER.with_borrow(|s| s.tick) as i64);
                            items.loot_table(&table, false, None, &mut random)
                        }
                        None => Vec::new(),
                    }
                };
                for d in drops {
                    game.spawn_item(d, [f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5], [0.0, 0.1, 0.0], 10);
                }
                if let Some(air) = air {
                    game.native_set_block(dim, [x, y, z], air, 3);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// AE2's `/ae2` command (appeng.server.Commands): `channelmode` shows or sets the channel mode (saved in AE2's
// config file in the game's config folder) and works every grid's channels out again.

const CHANNEL_MODES: [(&str, u32); 5] = [("infinite", 0), ("default", 1), ("x2", 2), ("x3", 3), ("x4", 4)];

fn channel_mode_file() -> Option<std::path::PathBuf> {
    Some(Game::native_config_dir().ok()?.join("ae2-channel-mode.txt"))
}

/// The saved channel mode, read once.
pub fn load_channel_mode() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let saved = channel_mode_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
        if let Some((_, f)) = CHANNEL_MODES.iter().find(|(n, _)| *n == saved.trim()) {
            super::port_ae2_grid::CHANNEL_MODE.store(*f, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

/// Chunk Logger: chunks log as they load and unload (`/ae2 chunklogger`).
static CHUNK_LOGGER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// `/ae2 tickmonitor`: how long AE2's CPUs take each tick goes to the log.
static TICK_MONITOR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `native_chunk_load` and `native_chunk_unload`, while the logger is on (ChunkLogger.onChunkLoadEvent).
pub fn chunk_logged(dimension: &str, chunk: [i32; 2], load: bool) {
    if CHUNK_LOGGER.load(std::sync::atomic::Ordering::Relaxed) {
        log::info!("{} chunk {} {} in {dimension}", if load { "Loaded" } else { "Unloaded" }, chunk[0], chunk[1]);
    }
}

/// `/ae2 ...` as typed: to the server, where AE2's state is; on the server itself it runs here.
pub fn ae2_command(game: &mut Game<'_>, args: &[&str]) -> bool {
    let line = format!("ae2 {}", args.join(" "));
    if game.native_net_send_to_server(NET_DEBUG, line.as_bytes()).is_err() {
        let level = game.native_server_players().into_iter().map(|p| p.permission).max().unwrap_or(4);
        let at = game.eye_position().unwrap_or([0.0; 3]);
        ae2_run(game, None, level, at, args);
    }
    true
}

/// `AECommand`: a player (None: the one here) runs a subcommand.
fn ae2_run(game: &mut Game<'_>, player: Option<u128>, level: u8, at: [f64; 3], args: &[&str]) {
    load_channel_mode();
    let say = |game: &mut Game<'_>, t: &str| match player {
        Some(uuid) => {
            game.native_tell(uuid, horizon_client::text::Text::legacy(t));
        }
        None => game.native_chat(t),
    };
    // Commands' level 4.
    if level < 4 {
        say(game, "You do not have adequate permissions to run this command.");
        return;
    }
    const USAGE: &str = "Commands provided by Applied Energistics 2 - use /ae2 list for a list, and /ae2 help _____ for help with a command.";
    use std::sync::atomic::Ordering::Relaxed;
    match args {
        [] => say(game, USAGE),
        ["list"] => say(game, "reloadconfig, chunklogger, spatial, channelmode, tickmonitor, compass, testmeteorites, setuptestworld"),
        ["help"] => say(game, USAGE),
        ["help", what] => {
            let text = match *what {
                "reloadconfig" => "/ae2 reloadconfig: reads AE2's configuration again",
                "chunklogger" => "/ae2 chunklogger: logs chunks as they load and unload, or stops",
                "spatial" => "/ae2 spatial info|tpback|givecell <id>|<id>",
                "channelmode" => "/ae2 channelmode [infinite|default|x2|x3|x4]",
                "tickmonitor" => "/ae2 tickmonitor <enable:true|false>",
                "compass" => "/ae2 compass: the meteorite compass's view of this region",
                "testmeteorites" => "/ae2 testmeteorites [force]: the meteorites around here",
                "setuptestworld" => "/ae2 setuptestworld [plot]: AE2's own test plots",
                _ => USAGE,
            };
            say(game, text);
        }
        ["reloadconfig"] => {
            let saved = channel_mode_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
            if let Some((_, f)) = CHANNEL_MODES.iter().find(|(n, _)| *n == saved.trim()) {
                super::port_ae2_grid::CHANNEL_MODE.store(*f, Relaxed);
            }
            say(game, "AE2 configuration reloaded");
        }
        ["chunklogger"] => {
            let on = !CHUNK_LOGGER.fetch_xor(true, Relaxed);
            say(game, if on { "Chunk Logging is now on" } else { "Chunk Logging is now off" });
        }
        ["tickmonitor"] => say(game, "/ae2 tickmonitor <enable:true|false>"),
        ["tickmonitor", value @ ("true" | "false")] => {
            TICK_MONITOR.store(*value == "true", Relaxed);
            say(game, &format!("Tick monitoring is now {}", if *value == "true" { "on" } else { "off" }));
        }
        // Spatial storage here lives in the cell itself: Horizon has no room for AE2's extra dimension of plots.
        ["spatial" | "spatial_info"] | ["spatial", "info"] => say(game, "Spatial cells hold what they store in the cell itself here: there are no plots to list."),
        ["spatial", "tpback"] => say(game, "You are not in the spatial storage level."),
        ["spatial", "givecell", _] | ["spatial", _] => say(game, "There is no spatial storage plot with that id."),
        ["channelmode"] => {
            let f = super::port_ae2_grid::channel_factor();
            let name = CHANNEL_MODES.iter().find(|(_, v)| *v == f).map_or("default", |(n, _)| n);
            say(game, &format!("Current channel mode: {name}"));
        }
        ["channelmode", mode] => {
            let Some((name, f)) = CHANNEL_MODES.iter().find(|(n, _)| n == mode).copied() else {
                say(game, &format!("Unknown channel mode: {mode}"));
                return;
            };
            super::port_ae2_grid::CHANNEL_MODE.store(f, Relaxed);
            if let Some(file) = channel_mode_file() {
                let _ = std::fs::write(file, name);
            }
            let grids = SERVER.with_borrow_mut(|s| {
                // PathingService.repath on every grid: the next pass works them out with the new capacity.
                s.mounted.clear();
                s.grids.len()
            });
            say(game, &format!("Channel mode set to {name}. Updated {grids} grids."));
        }
        ["compass"] | ["testmeteorites"] | ["testmeteorites", "force"] => {
            let dim = game.dim.id().to_owned();
            let spot = [at[0].floor() as i32, at[1].floor() as i32, at[2].floor() as i32];
            let lines = super::port_ae2_meteor::report(game, &dim, spot);
            if lines.is_empty() {
                say(game, "No meteorites in the regions around here.");
            }
            for line in lines {
                say(game, &line);
            }
        }
        ["setuptestworld", ..] => say(game, "AE2's test plots are for its own development and not part of this port."),
        _ => say(game, USAGE),
    }
}

// ---------------------------------------------------------------------------------------------------------
// AE2's meteorite compass (MeteoriteCompassItem, MeteoriteCompassBakedModel, CompassManager): the client asks the
// server about its chunk every second while it carries one; the pointer turns toward the answer (spins on the
// spot, turns slowly with no answer).

#[derive(Clone, Copy, Default)]
enum Compass {
    #[default]
    None,
    Spin,
    At(f64),
}

fn compass_ask(game: &mut Game<'_>) {
    if !game.inventory.iter().flatten().any(|s| &*s.item == "ae2:meteorite_compass") {
        return;
    }
    let due = CLIENT.with_borrow(|c| c.compass_asked.is_none_or(|t| t.elapsed().as_millis() >= 1000));
    let Some(eye) = game.eye_position() else { return };
    if !due {
        return;
    }
    let msg = format!("compass {} {}", (eye[0].floor() as i32) >> 4, (eye[2].floor() as i32) >> 4);
    if game.native_net_send_to_server(NET_UI, msg.as_bytes()).is_ok() {
        CLIENT.with_borrow_mut(|c| c.compass_asked = Some(std::time::Instant::now()));
    }
}

/// MeteoriteCompassBakedModel.getAnimatedRotation, drawn as the nearest of the sixteen pointer models.
fn compass_pointer(game: &Game<'_>, x: f32, y: f32, batch: &mut HudBatch) {
    use std::f64::consts::PI;
    let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let player = f64::from(game.yaw) / 180.0 * PI + PI;
    let angle = match CLIENT.with_borrow(|c| c.compass) {
        Compass::At(rad) => rad + player,
        Compass::Spin => (millis % 500) as f64 / 500.0 * 2.0 * PI,
        Compass::None => (millis % 3000) as f64 / 3000.0 * 2.0 * PI,
    };
    let i = ((angle / (PI / 8.0)).round() as i64).rem_euclid(16);
    game.native_item_model(batch, "ae2:meteorite_compass", &format!("ae2:hz_compass_pointer_{i}"), [x, y], 16.0);
}

// ---------------------------------------------------------------------------------------------------------
// AE2's matter cannon (MatterCannonItem): a one-type storage cell of ammo (loaded in an ME chest or drive). A shot
// takes 1 600 AE (a speed card adds a shot each, all from the same item), and its penetration is the ammo's weight
// times the shots: it hurts mobs (penetration / 20, rounded up), breaks blocks it beats (hardness x 9), and goes on
// through what it killed or broke at 60%. Paint balls dye the sheep they hit.

/// AE2's matter cannon ammo (data/ae2/recipes/matter_cannon, MatterCannonAmmo): an item or #tag and its weight.
const CANNON_AMMO: &[(&str, f32)] = &[
    ("ae2:matter_ball", 32.0), ("#c:aluminum_nuggets", 26.9815), ("#c:antimony_nuggets", 121.76),
    ("#c:barium_nuggets", 137.327), ("#c:beef_nuggets", 32.0), ("#c:beryllium_nuggets", 9.0122),
    ("#c:bismuth_nuggets", 208.9804), ("#c:boron_nuggets", 10.811), ("#c:bromine_nuggets", 79.904),
    ("#c:cadmium_nuggets", 112.411), ("#c:calcium_nuggets", 40.078), ("#c:carbon_nuggets", 12.0107),
    ("#c:cerium_nuggets", 140.116), ("#c:cesium_nuggets", 132.9055), ("#c:charcoal_nuggets", 12.0107),
    ("#c:chicken_nuggets", 32.0), ("#c:coal_nuggets", 12.0107), ("#c:cobalt_nuggets", 58.9332),
    ("#c:copper_nuggets", 63.546), ("#c:electrum_nuggets", 152.41737), ("#c:fish_nuggets", 32.0),
    ("#c:gallium_nuggets", 69.723), ("#c:germanium_nuggets", 72.64), ("#c:gold_nuggets", 196.96655),
    ("#c:indium_nuggets", 114.818), ("#c:invar_nuggets", 56.794468), ("#c:iodine_nuggets", 126.9045),
    ("#c:iridium_nuggets", 192.217), ("#c:iron_nuggets", 55.845), ("#c:krypton_nuggets", 83.8),
    ("#c:lanthanum_nuggets", 138.9055), ("#c:lead_nuggets", 207.2), ("#c:lithium_nuggets", 6.941),
    ("#c:magnesium_nuggets", 24.305), ("#c:manganese_nuggets", 54.938), ("#c:meat_nuggets", 32.0),
    ("#c:meatcooked_nuggets", 32.0), ("#c:meatraw_nuggets", 32.0), ("#c:nickel_nuggets", 58.6934),
    ("#c:niobium_nuggets", 92.9064), ("#c:osmium_nuggets", 190.23), ("#c:palladium_nuggets", 106.42),
    ("#c:phosphorus_nuggets", 30.9738), ("#c:platinum_nuggets", 195.078), ("#c:plutonium_nuggets", 244.0),
    ("#c:potassium_nuggets", 39.0983), ("#c:rhodium_nuggets", 102.9055), ("#c:rubidium_nuggets", 85.4678),
    ("#c:ruthenium_nuggets", 101.07), ("#c:scandium_nuggets", 44.9559), ("#c:sheep_nuggets", 32.0),
    ("#c:silicon_nuggets", 28.0855), ("#c:silver_nuggets", 107.8682), ("#c:sodium_nuggets", 22.9897),
    ("#c:strontium_nuggets", 87.62), ("#c:sulfur_nuggets", 32.065), ("#c:tantalum_nuggets", 180.9479),
    ("#c:technetium_nuggets", 98.0), ("#c:tellurium_nuggets", 127.6), ("#c:tin_nuggets", 118.71),
    ("#c:titanium_nuggets", 47.867), ("#c:tungsten_nuggets", 183.84), ("#c:uranium_nuggets", 238.0289),
    ("#c:vanadium_nuggets", 50.9415), ("#c:xenon_nuggets", 131.293), ("#c:yttrium_nuggets", 88.9059),
    ("#c:zinc_nuggets", 65.39), ("#c:zirconium_nuggets", 91.224),
];

fn is_paint_ball(id: &str) -> bool {
    id.starts_with("ae2:") && id.ends_with("_paint_ball")
}

/// Every item some ammo entry names (its tags expanded).
fn cannon_ammo_items(game: &Game<'_>) -> HashSet<String> {
    let items = game.res.items();
    let mut out = HashSet::new();
    for (entry, _) in CANNON_AMMO {
        match entry.strip_prefix('#') {
            Some(tag) => out.extend(items.item_tags.get(tag).into_iter().flatten().cloned()),
            None => {
                out.insert((*entry).to_owned());
            }
        }
    }
    out
}

/// MatterCannonItem.getPenetration: the ammo's weight (0 for paint balls and anything else).
fn cannon_weight(game: &Game<'_>, id: &str) -> f32 {
    let items = game.res.items();
    CANNON_AMMO
        .iter()
        .find(|(entry, _)| match entry.strip_prefix('#') {
            Some(tag) => items.item_tags.get(tag).is_some_and(|t| t.contains(id)),
            None => *entry == id,
        })
        .map_or(0.0, |(_, w)| *w)
}

/// MatterCannonItem.fireCannon. True when the use is taken.
fn fire_cannon(game: &mut Game<'_>, player: u128, creative: bool) -> bool {
    let Some(stack) = game.held().cloned() else { return false };
    let mut contents = read_cell(&stack);
    let Some(i) = contents.amounts.iter().position(|n| *n > 0) else {
        game.native_action_bar(player, horizon_client::text::Text::legacy("Ammo Depleted."));
        return true;
    };
    let ammo = contents.keys[i].clone();
    let speed = cell_cards(&stack).iter().filter(|c| c.as_str() == "ae2:speed_card").count() as i64;
    let mut shots = (1 + speed).min(contents.amounts[i]);
    let power = if creative { f64::INFINITY } else { portable_power(&stack) };
    if power < 1600.0 {
        return false;
    }
    shots = shots.min((power / 1600.0) as i64).max(1);
    // One item goes, whatever the shots (fireCannon extracts 1).
    contents.amounts[i] -= 1;
    contents.item_count -= 1;
    if contents.amounts[i] <= 0 {
        contents.keys.remove(i);
        contents.amounts.remove(i);
    }
    if let Some(held) = game.native_carried_stack(game.selected as i32) {
        write_cell(held, &contents);
        use_item_power(held, 1600.0 * shots as f64, creative);
    }
    let Some((eye, dir)) = game.aim() else { return true };
    let penetration = cannon_weight(game, ammo.id()) * shots as f32;
    if penetration <= 0.0 {
        if is_paint_ball(ammo.id()) {
            cannon_paint(game, eye, dir, ammo.id());
        }
        return true;
    }
    cannon_standard(game, eye, dir, penetration);
    true
}

/// The nearest live mob the ray (eye to 255 blocks) meets, its bounds grown by 0.3, and how far.
fn cannon_mob(game: &Game<'_>, eye: [f64; 3], dir: [f64; 3]) -> Option<(usize, f64)> {
    let mut best: Option<(usize, f64)> = None;
    for (i, m) in game.mobs.iter().enumerate().filter(|(_, m)| m.alive()) {
        let b = m.bounds();
        let (lo, hi) = (b.min_position(), b.max_position());
        let lo = [lo.x - 0.3, lo.y - 0.3, lo.z - 0.3];
        let hi = [hi.x + 0.3, hi.y + 0.3, hi.z + 0.3];
        let (mut t0, mut t1) = (0.0f64, 255.0f64);
        let mut hit = true;
        for a in 0..3 {
            if dir[a].abs() < 1e-9 {
                hit &= eye[a] >= lo[a] && eye[a] <= hi[a];
                continue;
            }
            let (mut ta, mut tb) = ((lo[a] - eye[a]) / dir[a], (hi[a] - eye[a]) / dir[a]);
            if ta > tb {
                std::mem::swap(&mut ta, &mut tb);
            }
            t0 = t0.max(ta);
            t1 = t1.min(tb);
        }
        if hit && t0 <= t1 && best.is_none_or(|b| t0 < b.1) {
            best = Some((i, t0));
        }
    }
    best
}

/// The first block with a collision shape on the ray (ClipContext.Block.COLLIDER, fluids ignored), and how far.
fn cannon_block(game: &Game<'_>, eye: [f64; 3], dir: [f64; 3]) -> Option<(Pos, f64)> {
    let mut t = 0.0;
    let mut last = [i32::MIN; 3];
    while t < 255.0 {
        let p = [(eye[0] + dir[0] * t).floor() as i32, (eye[1] + dir[1] * t).floor() as i32, (eye[2] + dir[2] * t).floor() as i32];
        if p != last {
            last = p;
            let state = game.world.block(p[0], p[1], p[2]);
            let facts = game.res.facts().get(state);
            let name = &*facts.name;
            if !name.ends_with(":air") && !matches!(name, "minecraft:water" | "minecraft:lava") && !is_paint(name) && (facts.full_collision || !facts.replaceable) {
                return Some((p, t));
            }
        }
        t += 0.05;
    }
    None
}

/// MatterCannonFX along the shot, to what it hit (32 blocks when nothing).
fn cannon_trail(game: &mut Game<'_>, eye: [f64; 3], dir: [f64; 3], length: f64) {
    let fx = horizon_client::particles::ParticleOptions::simple(horizon_client::particles::ParticleType::Crit);
    let mut t = 0.5;
    while t < length {
        game.particles.add(&fx, [eye[0] + dir[0] * t, eye[1] + dir[1] * t, eye[2] + dir[2] * t], [0.0; 3]);
        t += 0.6;
    }
}

fn cannon_standard(game: &mut Game<'_>, eye: [f64; 3], dir: [f64; 3], mut penetration: f32) {
    use horizon_client::mobs::{Damage, DamageKind};
    let dim = game.dim.id().to_owned();
    let mut destroyed = true;
    while penetration > 0.0 && destroyed {
        destroyed = false;
        let mob = cannon_mob(game, eye, dir);
        let block = cannon_block(game, eye, dir);
        let length = match (mob, block) {
            (Some((_, m)), Some((_, b))) => m.min(b),
            (Some((_, m)), None) => m,
            (None, Some((_, b))) => b,
            (None, None) => 32.0,
        };
        cannon_trail(game, eye, dir, length + 1.0);
        match (mob, block) {
            (Some((i, m)), b) if b.is_none_or(|(_, bt)| m <= bt) => {
                let dmg = (penetration / 20.0).ceil();
                penetration -= dmg;
                let at = eye;
                let mob = &mut game.mobs[i];
                mob.velocity.x -= dir[0] * 0.4;
                mob.velocity.z -= dir[2] * 0.4;
                mob.damage(dmg, Damage { kind: DamageKind::PlayerAttack, attacker: None, from: Some(at) });
                destroyed = !mob.alive();
            }
            (_, Some((pos, _))) => {
                let state = game.world.block(pos[0], pos[1], pos[2]);
                let hardness = game.res.facts().get(state).hardness * 9.0;
                if hardness >= 0.0 && penetration > hardness {
                    destroyed = true;
                    penetration = (penetration - hardness) * 0.6;
                    // Level.destroyBlock(pos, true): its drops, then air.
                    let drops = {
                        let items = game.res.items();
                        match items.block_loot_table(&game.pack.assets.registry, state) {
                            Some(table) => {
                                let mut random = horizon_core::random::LegacyRandom::new(((pos[0] as i64) << 24) ^ ((pos[1] as i64) << 12) ^ pos[2] as i64);
                                items.loot_table(&table, false, None, &mut random)
                            }
                            None => Vec::new(),
                        }
                    };
                    for d in drops {
                        game.spawn_item(d, [f64::from(pos[0]) + 0.5, f64::from(pos[1]) + 0.5, f64::from(pos[2]) + 0.5], [0.0, 0.1, 0.0], 10);
                    }
                    if let Some(air) = game.pack.assets.registry.default_state("minecraft:air") {
                        game.native_set_block(&dim, pos, air, 3);
                    }
                }
            }
            _ => {}
        }
    }
}

/// MatterCannonItem.shootPaintBalls: a sheep hit takes the ball's colour.
fn cannon_paint(game: &mut Game<'_>, eye: [f64; 3], dir: [f64; 3], ball: &str) {
    let mob = cannon_mob(game, eye, dir);
    let block = cannon_block(game, eye, dir);
    let length = match (mob, block) {
        (Some((_, m)), Some((_, b))) => m.min(b),
        (Some((_, m)), None) => m,
        (None, Some((_, b))) => b,
        (None, None) => 32.0,
    };
    cannon_trail(game, eye, dir, length + 1.0);
    if let Some((pos, t)) = block
        && mob.is_none_or(|(_, m)| t < m)
    {
        // The face hit: the cell the ray was in just before.
        let before = [(eye[0] + dir[0] * (t - 0.05)).floor() as i32, (eye[1] + dir[1] * (t - 0.05)).floor() as i32, (eye[2] + dir[2] * (t - 0.05)).floor() as i32];
        let face = [before[0] - pos[0], before[1] - pos[1], before[2] - pos[2]];
        if SIDES.contains(&face) {
            let hit = [eye[0] + dir[0] * t, eye[1] + dir[1] * t, eye[2] + dir[2] * t];
            add_splotch(game, add(pos, face), face, hit, ball);
        }
        return;
    }
    let Some((i, m)) = mob else { return };
    if block.is_some_and(|(_, b)| b < m) {
        return;
    }
    const DYES: [&str; 16] = ["white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray", "cyan", "purple", "blue", "brown", "green", "red", "black"];
    let color = ball.trim_start_matches("ae2:").trim_end_matches("_paint_ball").trim_end_matches("_lumen");
    if let Some(c) = DYES.iter().position(|d| *d == color)
        && game.mobs[i].kind.id.as_str() == "minecraft:sheep"
    {
        game.mobs[i].extra.color = c as u8;
    }
}

/// The middle button on a terminal's grid (AE2's MEStorageScreen: CREATIVE_DUPLICATE in creative mode).
pub fn terminal_middle_click(game: &Game<'_>, menu: Option<&str>, mouse: [f32; 2], button: i32, canceled: &mut bool) {
    if button != 2 {
        return;
    }
    let wireless = game.native_mod_screen() == Some("ae2") && CLIENT.with_borrow(|c| c.wireless_open) == 2;
    let terminal = matches!(menu, Some("ae2:terminal" | "ae2:crafting_terminal" | "ae2:pattern_encoding_terminal"));
    let origin = if wireless {
        Some(wireless_origin(game))
    } else if terminal {
        CLIENT.with_borrow(|c| c.term_origin)
    } else {
        None
    };
    let Some([ox, oy]) = origin else { return };
    let rows = CLIENT.with_borrow(|c| c.rows.max(3));
    let [gx, gy] = [mouse[0] - ox - TERM_GRID[0], mouse[1] - oy - TERM_GRID[1]];
    if gx < 0.0 || gy < 0.0 || gx >= TERM_COLS as f32 * 18.0 || gy >= rows as f32 * 18.0 {
        return;
    }
    let cell = CLIENT.with_borrow(|c| c.scroll * TERM_COLS) + (gy / 18.0) as usize * TERM_COLS + (gx / 18.0) as usize;
    if let Some(index) = CLIENT.with_borrow(|c| visible(c).get(cell).copied()) {
        *canceled = true;
        // On the cursor: a block terminal's through its menu, the wireless one's through its own cursor.
        CLIENT.with_borrow_mut(|c| {
            if wireless {
                c.wireless_out.push(format!("grid dup {index}"));
            } else {
                let offset = if menu == Some("ae2:pattern_encoding_terminal") { 8 } else { 0 };
                c.buttons_out.push((((index as i32) << 3) | CREATIVE_DUPLICATE) + offset);
            }
        });
    }
}

// ---------------------------------------------------------------------------------------------------------
// AE2's facades (FacadeItem, FacadePart, FacadeContainer): a plate of a block's faces, two pixels thick, on a side
// of a cable (one a side). Made by FacadeRecipe (data/ae2/recipe/hz_facade/*); its block is the item's `item`.

const FACE_NAMES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

/// The block a facade item is of.
fn facade_block(stack: &ItemStack) -> Option<String> {
    let Some(Tag::Compound(d)) = stack.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        return None;
    };
    match d.get(&java("item")) {
        Some(Tag::String(j)) => j.to_string().ok(),
        _ => None,
    }
}

/// A facade item of `block`, as the recipe makes it (its item model drawing the block's plate).
fn facade_stack(block: &str) -> ItemStack {
    let mut stack = ItemStack::new("ae2:facade", 1);
    set_custom(&mut stack, "item", Tag::String(java(block)));
    let (ns, path) = block.split_once(':').unwrap_or(("minecraft", block));
    let flat = if ns == "minecraft" { path.to_owned() } else { format!("{ns}_{path}") };
    let extra = stack.extra.get_or_insert_with(Default::default);
    extra.other.retain(|(k, _)| k != "minecraft:item_model");
    extra.other.push(("minecraft:item_model".to_owned(), Tag::String(java(&format!("ae2:hz_facade_{flat}")))));
    extra.other.sort_by(|a, b| a.0.cmp(&b.0));
    stack
}

/// The facades on a cable: side name to block.
fn cable_facades(nbt: &Compound) -> Vec<(String, String)> {
    match nbt.get(&java("facades")) {
        Some(Tag::Compound(c)) => c
            .iter()
            .filter_map(|(k, v)| Some((k.to_string().ok()?, match v { Tag::String(j) => j.to_string().ok()?, _ => return None })))
            .collect(),
        _ => Vec::new(),
    }
}

/// The six faces each facade block is drawn with (the content's facade table).
fn facade_faces(game: &Game<'_>, block: &str) -> Option<[String; 6]> {
    thread_local! {
        static TABLE: RefCell<Option<HashMap<String, [String; 6]>>> = const { RefCell::new(None) };
    }
    TABLE.with_borrow_mut(|t| {
        let table = t.get_or_insert_with(|| {
            game.native_data("data/ae2/hz/facades.json")
                .and_then(|b| serde_json::from_slice::<HashMap<String, Vec<String>>>(&b).ok())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(k, v)| Some((k, v.try_into().ok()?)))
                .collect()
        });
        table.get(block).cloned()
    })
}

/// FacadeItem.onItemUseFirst on a cable: the facade goes on the clicked side if that side has none. Sneaking with
/// an empty hand or a wrench takes the clicked side's facade off. True when the click was the facade's.
fn facade_click(game: &mut Game<'_>, pos: Pos, face: Pos) -> bool {
    let Some(side) = SIDES.iter().position(|d| *d == face).map(|i| side_name(SIDES[i])) else { return false };
    let held = game.held().cloned();
    let Some(player) = game.native_acting_player() else { return false };
    if let Some(h) = held.as_ref().filter(|h| &*h.item == "ae2:facade") {
        let Some(block) = facade_block(h) else { return false };
        if facade_faces(game, &block).is_none() {
            return false;
        }
        swap_cable(game, pos, true);
        let placed = game
            .native_with_mod_block_entity(pos, |e| {
                let mut c = match e.nbt.get(&java("facades")) {
                    Some(Tag::Compound(c)) => c.clone(),
                    _ => Compound::new(),
                };
                if c.get(&java(side)).is_some() {
                    return false;
                }
                c.insert(java(side), Tag::String(java(&block)));
                e.nbt.insert(java("facades"), Tag::Compound(c));
                true
            })
            .unwrap_or(false);
        if placed {
            if player.game_mode != 1
                && let Some(h) = game.native_carried_stack(game.selected as i32)
            {
                h.count -= 1;
            }
            refresh_twin(game, pos);
        }
        return true;
    }
    let wrench = held.as_ref().is_some_and(|h| matches!(&*h.item, "ae2:certus_quartz_wrench" | "ae2:nether_quartz_wrench"));
    if !sneaking(game) || !(held.is_none() || wrench) {
        return false;
    }
    let removed = game
        .native_with_mod_block_entity(pos, |e| {
            let Some(Tag::Compound(c)) = e.nbt.get_mut(&java("facades")) else { return None };
            let block = match c.remove(&java(side)) {
                Some(Tag::String(j)) => j.to_string().ok(),
                _ => None,
            };
            block
        })
        .flatten();
    let Some(block) = removed else { return false };
    refresh_twin(game, pos);
    if let Some(left) = game.native_give_stack(player.uuid, facade_stack(&block)) {
        game.spawn_item(left, [f64::from(pos[0]) + 0.5, f64::from(pos[1]) + 0.5, f64::from(pos[2]) + 0.5], [0.0, 0.1, 0.0], 10);
    }
    game.native_block_entity_sync(pos);
    true
}

fn side_name(d: Pos) -> &'static str {
    match d {
        [0, -1, 0] => "down",
        [0, 1, 0] => "up",
        [0, 0, -1] => "north",
        [0, 0, 1] => "south",
        [-1, 0, 0] => "west",
        _ => "east",
    }
}

/// FacadeBuilder: each facade a plate two pixels thick against its side, the block's own faces on it.
fn facade_submit(game: &Game<'_>, entity: &ModBlockEntity, draw: &mut super::native_block_entity_draw::BlockEntityDraw) {
    use super::native_block_entity_draw::CubeFace;
    for (side, block) in cable_facades(&entity.nbt) {
        let Some(faces) = facade_faces(game, &block) else { continue };
        let (from, to) = match side.as_str() {
            "down" => ([0.0, 0.0, 0.0], [16.0, 2.0, 16.0]),
            "up" => ([0.0, 14.0, 0.0], [16.0, 16.0, 16.0]),
            "north" => ([0.0, 0.0, 0.0], [16.0, 16.0, 2.0]),
            "south" => ([0.0, 0.0, 14.0], [16.0, 16.0, 16.0]),
            "west" => ([0.0, 0.0, 0.0], [2.0, 16.0, 16.0]),
            _ => ([14.0, 0.0, 0.0], [16.0, 16.0, 16.0]),
        };
        // Vanilla's sprites go by their bare name in the atlas.
        let names: Vec<&str> = faces.iter().map(|f| f.strip_prefix("minecraft:").unwrap_or(f)).collect();
        let face = |i: usize| Some(CubeFace { sprite: names[i], uv: None });
        game.native_draw_cube(draw, from, to, [face(0), face(1), face(2), face(3), face(4), face(5)], 0xFFFFFF);
    }
}

/// `native_destroy_block`: a cable broken drops its facades (FacadeContainer's drops).
pub fn destroyed(game: &mut Game<'_>, pos: Pos) {
    if cable(&block_id(game, pos)).is_none() {
        return;
    }
    let facades = game.native_with_mod_block_entity(pos, |e| cable_facades(&e.nbt)).unwrap_or_default();
    let creative = game.native_acting_player().is_some_and(|p| p.game_mode == 1);
    if creative {
        return;
    }
    if is_twin(&game.native_block(pos))
        && let Some(cable) = twin_cable(game, pos)
    {
        game.spawn_item(ItemStack::new(&format!("ae2:{cable}"), 1), [f64::from(pos[0]) + 0.5, f64::from(pos[1]) + 0.5, f64::from(pos[2]) + 0.5], [0.0, 0.1, 0.0], 10);
    }
    for (_, block) in facades {
        game.spawn_item(facade_stack(&block), [f64::from(pos[0]) + 0.5, f64::from(pos[1]) + 0.5, f64::from(pos[2]) + 0.5], [0.0, 0.1, 0.0], 10);
    }
}

/// A cable and its facade twin (the twin holds the cable, its lights and the facades in its block entity): to the
/// twin, or back to the plain cable, its arms and lights kept.
fn swap_cable(game: &mut Game<'_>, pos: Pos, to_twin: bool) {
    let raw = game.native_block(pos);
    let dim = game.dim.id().to_owned();
    if to_twin {
        if is_twin(&raw) {
            return;
        }
        let Some(name) = ae2_name(&raw).filter(|_| cable(&raw).is_some()).map(str::to_owned) else { return };
        let arms = TWIN_SIDES.iter().enumerate().filter(|(_, k)| game.native_block_property(pos, k).as_deref() == Some("true")).fold(0u32, |m, (i, _)| m | 1 << i);
        let lights = game.native_block_property(pos, "x_lights").unwrap_or_else(|| "0".to_owned());
        place_twin(game, pos, &name, arms, 0);
        game.native_with_mod_block_entity(pos, |e| {
            e.nbt.insert(java("cable"), Tag::String(java(&name)));
            e.nbt.insert(java("lights"), Tag::String(java(&lights)));
        });
        game.native_block_entity_sync(pos);
    } else {
        if !is_twin(&raw) {
            return;
        }
        let Some(name) = twin_cable(game, pos) else { return };
        let lights = game.native_with_mod_block_entity(pos, |e| nbt_string(&e.nbt, "lights")).unwrap_or_default();
        let Some(state) = game.pack.assets.registry.default_state(&format!("ae2:{name}")) else { return };
        if game.native_set_block(&dim, pos, state, 3) {
            if !lights.is_empty() && game.native_block_property(pos, "x_lights").is_some() {
                game.native_set_block_property_with(pos, "x_lights", &lights, 3);
            }
            cable_sides(game, pos);
        }
    }
}

/// The facade twins: `ae2:hz_fcb<core>_<n>`, colourless, sixteen looks to a block (its `rotation`). Look `n * 16 +
/// rotation` holds the joined arms (bits 0 to 5, `TWIN_SIDES` order) and the facades (bits 6 to 11), and so the
/// twin's collision (CableBusContainer's boxes); the cable it is, its lights and the facades are its block entity's.
fn is_twin(block: &str) -> bool {
    block.starts_with("ae2:hz_fcb")
}

const TWIN_SIDES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

/// The twin's core for a cable kind (CableCoreType: glass 6..10, covered and smart 5..11, dense 3..13).
fn twin_core(cable: &str) -> usize {
    if cable.contains("dense") {
        2
    } else if cable.contains("glass") {
        0
    } else {
        1
    }
}

/// The twin at `pos`: its core, joined arms and facades.
fn twin_masks(game: &Game<'_>, pos: Pos) -> Option<(usize, u32, u32)> {
    let raw = game.native_block(pos);
    let (core, n) = raw.strip_prefix("ae2:hz_fcb")?.split_once('_')?;
    let rotation: u32 = game.native_block_property(pos, "rotation")?.parse().ok()?;
    let look = n.parse::<u32>().ok()? * 16 + rotation;
    Some((core.parse().ok()?, look & 63, look >> 6))
}

/// The cable a twin holds (`fluix_glass_cable`).
fn twin_cable(game: &Game<'_>, pos: Pos) -> Option<String> {
    match game.world.block_entities.get(&(pos[0], pos[1], pos[2])) {
        Some(horizon_client::containers::BlockEntity::Mod(m)) => Some(nbt_string(&m.nbt, "cable")).filter(|c| !c.is_empty()),
        _ => None,
    }
}

/// The block at `pos` as the network sees it: a twin is the cable it holds (`ae2:hz_fcable_<cable>`, which
/// `ae2_name` reads as the cable).
fn block_id(game: &Game<'_>, pos: Pos) -> String {
    let raw = game.native_block(pos);
    if is_twin(&raw)
        && let Some(cable) = twin_cable(game, pos)
    {
        return format!("ae2:hz_fcable_{cable}");
    }
    raw
}

/// The facades on a twin's block entity as `TWIN_SIDES` bits.
fn facade_mask(nbt: &Compound) -> u32 {
    cable_facades(nbt).iter().filter_map(|(side, _)| TWIN_SIDES.iter().position(|k| k == side)).fold(0, |m, i| m | 1 << i)
}

/// Puts `cable`'s twin with `arms` and `facades` at `pos`, its block entity's data kept (another twin block makes
/// a new block entity).
fn place_twin(game: &mut Game<'_>, pos: Pos, cable: &str, arms: u32, facades: u32) {
    let look = (facades << 6) | arms;
    let want = format!("ae2:hz_fcb{}_{}", twin_core(cable), look / 16);
    let rotation = (look % 16).to_string();
    let raw = game.native_block(pos);
    let kept = game.native_with_mod_block_entity(pos, |e| e.nbt.clone()).filter(|_| is_twin(&raw));
    let dim = game.dim.id().to_owned();
    if raw != want {
        let Some(state) = game.pack.assets.registry.default_state(&want) else { return };
        if !game.native_set_block(&dim, pos, state, 3) {
            return;
        }
    }
    if game.native_block_property(pos, "rotation").as_deref() != Some(rotation.as_str()) {
        game.native_set_block_property_with(pos, "rotation", &rotation, 3);
    }
    if let Some(nbt) = kept {
        game.native_with_mod_block_entity(pos, |e| e.nbt = nbt);
    }
    game.native_with_mod_block_entity(pos, |e| e.nbt.insert(java("cable"), Tag::String(java(cable))));
    game.native_block_entity_sync(pos);
}

/// A twin's facades changed: the twin for them, or the plain cable once none is left.
fn refresh_twin(game: &mut Game<'_>, pos: Pos) {
    let Some((_, arms, _)) = twin_masks(game, pos) else { return };
    let Some(cable) = twin_cable(game, pos) else { return };
    let facades = game.native_with_mod_block_entity(pos, |e| facade_mask(&e.nbt)).unwrap_or(0);
    if facades == 0 {
        swap_cable(game, pos, false);
    } else {
        place_twin(game, pos, &cable, arms, facades);
    }
}

/// The cable a twin holds, drawn as its block (its arms and lights as the plain cable's state would be).
fn twin_submit(game: &Game<'_>, block: &str, state: horizon_world::registry::BlockStateId, entity: &ModBlockEntity, draw: &mut super::native_block_entity_draw::BlockEntityDraw) {
    let cable = nbt_string(&entity.nbt, "cable");
    if cable.is_empty() {
        return;
    }
    let Some(n) = block.strip_prefix("ae2:hz_fcb").and_then(|r| r.split_once('_')).and_then(|(_, n)| n.parse::<u32>().ok()) else { return };
    let rotation = game.pack.assets.registry.state(state).and_then(|s| s.properties.get("rotation").and_then(|r| r.parse::<u32>().ok())).unwrap_or(0);
    let arms = (n * 16 + rotation) & 63;
    let lights = nbt_string(&entity.nbt, "lights");
    let mut props: Vec<(&str, &str)> = TWIN_SIDES.iter().enumerate().map(|(i, k)| (*k, if arms >> i & 1 == 1 { "true" } else { "false" })).collect();
    if cable.contains("smart") {
        props.push(("x_lights", if lights.is_empty() { "0" } else { lights.as_str() }));
    }
    game.native_draw_block(draw, &format!("ae2:{cable}"), &props);
}

// ---------------------------------------------------------------------------------------------------------
// AE2's paint splotches (PaintSplotchesBlock, PaintSplotchesBlockEntity, Splotch, PaintSplotchesBakedModel): the
// matter cannon's paint balls leave splotches on the faces they hit, kept by a paint block in front of the face
// (up to 21, the oldest going first), lumen paint lighting it (12 for one, 15 for two or more).

fn is_paint(block: &str) -> bool {
    matches!(block, "ae2:paint" | "ae2:hz_paint_lit1" | "ae2:hz_paint_lit2")
}

/// AEColor's medium and white variants, by AEColor order (the paint ball's colour).
const AE_COLOR_NAMES: [&str; 16] = ["white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray", "cyan", "purple", "blue", "brown", "green", "red", "black"];
const AE_MEDIUM: [u32; 16] = [14408667, 16428612, 12069560, 8563943, 16318282, 12320593, 16299479, 10526880, 13487565, 5352134, 10769358, 5327615, 12031615, 6349614, 16711740, 5658198];
const AE_WHITE: [u32; 16] = [16448250, 16047811, 12949704, 14219007, 16777192, 15202263, 16244459, 13224393, 15724527, 11460084, 13083596, 14542591, 14734024, 14938851, 16770797, 8684676];

/// The splotches: (side of the paint block toward the painted face, x, y in 0..15, colour, lumen).
fn splotches(nbt: &Compound) -> Vec<(Pos, i32, i32, usize, bool)> {
    nbt_string(nbt, "dots")
        .split(';')
        .filter_map(|d| {
            let f: Vec<i32> = d.split(',').filter_map(|n| n.parse().ok()).collect();
            let [sx, sy, sz, x, y, c, l] = f.as_slice() else { return None };
            Some(([*sx, *sy, *sz], *x, *y, (*c).clamp(0, 15) as usize, *l != 0))
        })
        .collect()
}

fn write_splotches(nbt: &mut Compound, dots: &[(Pos, i32, i32, usize, bool)]) {
    let text = dots.iter().map(|(s, x, y, c, l)| format!("{},{},{},{x},{y},{c},{}", s[0], s[1], s[2], u8::from(*l))).collect::<Vec<_>>().join(";");
    nbt.insert(java("dots"), Tag::String(java(&text)));
}

/// PaintSplotchesBlockEntity.addBlot at `at` (in front of the face `face` of the block hit, `hit` the point).
fn add_splotch(game: &mut Game<'_>, at: Pos, face: Pos, hit: [f64; 3], ball: &str) {
    let here = block_id(game, at);
    if !is_paint(&here) {
        let id = game.world.block(at[0], at[1], at[2]);
        let replaceable = here.ends_with(":air") || game.res.facts().get(id).replaceable;
        if !replaceable {
            return;
        }
        let dim = game.dim.id().to_owned();
        let Some(state) = game.pack.assets.registry.default_state("ae2:paint") else { return };
        game.native_set_block(&dim, at, state, 3);
    }
    let color = ball.trim_start_matches("ae2:").trim_end_matches("_paint_ball");
    let lumen = color.ends_with("_lumen");
    let color = color.trim_end_matches("_lumen");
    let Some(c) = AE_COLOR_NAMES.iter().position(|n| *n == color) else { return };
    // The side toward the painted block, and the hit point across that face (Splotch's constructor).
    let side = [-face[0], -face[1], -face[2]];
    let local = [hit[0] - f64::from(at[0]), hit[1] - f64::from(at[1]), hit[2] - f64::from(at[2])];
    let (x, y) = if side[2] != 0 { (local[0], local[1]) } else if side[1] != 0 { (local[0], local[2]) } else { (local[1], local[2]) };
    let (x, y) = (((x * 15.0) as i32).clamp(0, 15), ((y * 15.0) as i32).clamp(0, 15));
    let lumens = game
        .native_with_mod_block_entity(at, |e| {
            let mut dots = splotches(&e.nbt);
            if dots.len() > 20 {
                dots.remove(0);
            }
            dots.push((side, x, y, c, lumen));
            write_splotches(&mut e.nbt, &dots);
            dots.iter().filter(|d| d.4).count()
        })
        .unwrap_or(0);
    paint_light(game, at, lumens);
    game.native_block_entity_sync(at);
}

/// PaintSplotchesBlockEntity.updateData's light: the paint block for none, one or two lumen splotches.
fn paint_light(game: &mut Game<'_>, at: Pos, lumens: usize) {
    let want = ["ae2:paint", "ae2:hz_paint_lit1", "ae2:hz_paint_lit2"][lumens.min(2)];
    if block_id(game, at) == want {
        return;
    }
    let nbt = game.native_with_mod_block_entity(at, |e| e.nbt.clone()).unwrap_or_default();
    let dim = game.dim.id().to_owned();
    let Some(state) = game.pack.assets.registry.default_state(want) else { return };
    game.native_set_block(&dim, at, state, 3);
    game.native_with_mod_block_entity(at, |e| e.nbt = nbt);
}

/// PaintSplotchesBlockEntity.cleanSide: a snowball from the applicator takes a face's splotches off.
fn clean_splotches(game: &mut Game<'_>, at: Pos, side: Pos) -> bool {
    if !is_paint(&block_id(game, at)) {
        return false;
    }
    let left = game
        .native_with_mod_block_entity(at, |e| {
            let mut dots = splotches(&e.nbt);
            let before = dots.len();
            dots.retain(|d| d.0 != side);
            write_splotches(&mut e.nbt, &dots);
            (before != dots.len()).then_some((dots.len(), dots.iter().filter(|d| d.4).count()))
        })
        .flatten();
    let Some((count, lumens)) = left else { return false };
    if count == 0 {
        let dim = game.dim.id().to_owned();
        if let Some(air) = game.pack.assets.registry.default_state("minecraft:air") {
            game.native_set_block(&dim, at, air, 3);
        }
    } else {
        paint_light(game, at, lumens);
        game.native_block_entity_sync(at);
    }
    true
}

/// PaintSplotchesBakedModel: each splotch a fifth of a block square on its face, just off it, one of the three
/// paint textures by its seed, tinted the colour (lumen paint its white variant, full bright).
fn paint_submit(game: &Game<'_>, entity: &ModBlockEntity, draw: &mut super::native_block_entity_draw::BlockEntityDraw) {
    use super::native_block_entity_draw::CubeFace;
    let mut offset = 0.001f32;
    for (side, x, y, c, lumen) in splotches(&entity.nbt) {
        let (px, py) = ((x as f32 / 15.0).clamp(0.1, 0.9), (y as f32 / 15.0).clamp(0.1, 0.9));
        let o = offset;
        offset += 0.001;
        let t = 0.01;
        let (lo, hi) = match side {
            [0, 1, 0] => ([px - 0.1, 1.0 - o - t, py - 0.1], [px + 0.1, 1.0 - o, py + 0.1]),
            [0, -1, 0] => ([px - 0.1, o, py - 0.1], [px + 0.1, o + t, py + 0.1]),
            [1, 0, 0] => ([1.0 - o - t, px - 0.1, py - 0.1], [1.0 - o, px + 0.1, py + 0.1]),
            [-1, 0, 0] => ([o, px - 0.1, py - 0.1], [o + t, px + 0.1, py + 0.1]),
            [0, 0, 1] => ([px - 0.1, py - 0.1, 1.0 - o - t], [px + 0.1, py + 0.1, 1.0 - o]),
            _ => ([px - 0.1, py - 0.1, o], [px + 0.1, py + 0.1, o + t]),
        };
        let side_index = SIDES.iter().position(|s| *s == side).unwrap_or(0) as i32;
        let seed = ((x | (y << 4)) + (side_index | ((c as i32) << 3) | if lumen { 128 } else { 0 })).unsigned_abs() as usize;
        let sprite = ["ae2:block/paint1", "ae2:block/paint2", "ae2:block/paint3"][seed % 3];
        let face = Some(CubeFace { sprite, uv: Some([0.0, 0.0, 16.0, 16.0]) });
        let tint = if lumen { AE_WHITE[c] } else { AE_MEDIUM[c] };
        let light = draw.light;
        if lumen {
            draw.light = 0xF0F0;
        }
        game.native_draw_cube(draw, lo.map(|v| v * 16.0), hi.map(|v| v * 16.0), [face, face, face, face, face, face], tint);
        draw.light = light;
    }
}

/// `native_use_item_on`: a block placed where paint is replaces it (the paint block is replaceable in AE2).
pub fn clear_paint_for(game: &mut Game<'_>, stack: &ItemStack, pos: Pos, face: Pos) {
    let at = add(pos, face);
    if !is_paint(&block_id(game, at)) || game.pack.assets.registry.default_state(&stack.item).is_none() {
        return;
    }
    let dim = game.dim.id().to_owned();
    if let Some(air) = game.pack.assets.registry.default_state("minecraft:air") {
        game.native_set_block(&dim, at, air, 3);
    }
}

// ---------------------------------------------------------------------------------------------------------
// AE2's wireless crafting terminal (WirelessCraftingTerminalItem, CraftingTermMenu): the wireless terminal with a
// crafting grid kept on the item. The screen has no real slots, so the cursor is the server's: each click on the
// player's inventory, the grid or the result is worked out in the player's inventory tick, as a menu would.

/// The terminal's crafting grid (nine stacks, from its `ae2:craft_grid`).
fn craft_grid(terminal: &ItemStack) -> Vec<Option<ItemStack>> {
    let Some(Tag::Compound(d)) = terminal.extra.as_ref().and_then(|e| e.other.iter().find(|(k, _)| k == CELL_DATA)).map(|(_, t)| t) else {
        return vec![None; 9];
    };
    let Some(Tag::List(list)) = d.get(&java("ae2:craft_grid")) else { return vec![None; 9] };
    let mut out: Vec<Option<ItemStack>> = list
        .iter()
        .map(|t| match t {
            Tag::String(j) => {
                let text = j.to_string().ok()?;
                let (key, n) = text.rsplit_once('\t')?;
                Some(stack_of(&key_from_string(key)?, n.parse().ok()?))
            }
            _ => None,
        })
        .collect();
    out.resize(9, None);
    out
}

fn set_craft_grid(game: &Game<'_>, terminal: &mut ItemStack, grid: &[Option<ItemStack>]) {
    let list: Vec<Tag> = grid
        .iter()
        .map(|s| Tag::String(java(&s.as_ref().map_or(String::new(), |s| format!("{}\t{}", key_to_string(&key_of(game, s)), s.count)))))
        .collect();
    set_custom(terminal, "ae2:craft_grid", Tag::List(list));
}

/// A vanilla slot click: pick up all (half on the right button), put down all (one on the right), merge or swap.
fn click_slot(game: &Game<'_>, slot: &mut Option<ItemStack>, cursor: &mut Option<ItemStack>, right: bool) {
    let same = |a: &ItemStack, b: &ItemStack| key_of(game, a) == key_of(game, b);
    match (slot.take(), cursor.take()) {
        (None, None) => {}
        (Some(mut s), None) => {
            if right && s.count > 1 {
                let half = s.count.div_ceil(2);
                let mut taken = s.clone();
                taken.count = half;
                s.count -= half;
                *slot = Some(s);
                *cursor = Some(taken);
            } else {
                *cursor = Some(s);
            }
        }
        (None, Some(mut c)) => {
            if right && c.count > 1 {
                let mut one = c.clone();
                one.count = 1;
                c.count -= 1;
                *slot = Some(one);
                *cursor = Some(c);
            } else {
                *slot = Some(c);
            }
        }
        (Some(mut s), Some(mut c)) if same(&s, &c) => {
            let most = max_stack(game, &s.item);
            let moved = if right { 1.min(c.count) } else { c.count }.min(most.saturating_sub(s.count));
            s.count += moved;
            c.count -= moved;
            *slot = Some(s);
            *cursor = (c.count > 0).then_some(c);
        }
        (Some(s), Some(c)) => {
            *slot = Some(c);
            *cursor = Some(s);
        }
    }
}

/// The clicks the wireless crafting terminal's screen sent, worked out now that the player's inventory is here.
fn wcraft_ops(game: &mut Game<'_>, player: u128) {
    let ops: Vec<String> = SERVER.with_borrow_mut(|s| {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut s.wcraft_ops).into_iter().partition(|(p, _)| *p == player);
        s.wcraft_ops = rest;
        mine.into_iter().map(|(_, o)| o).collect()
    });
    if ops.is_empty() {
        return;
    }
    let Some(term_slot) = SERVER.with_borrow(|s| s.wcraft_slot.get(&player).copied()) else { return };
    let term_slot = term_slot as usize;
    let mut cursor = SERVER.with_borrow_mut(|s| s.wcursor.remove(&player));
    for op in ops {
        let w: Vec<&str> = op.split(' ').collect();
        let terminal = game.inventory.get(term_slot).cloned().flatten().filter(|t| matches!(&*t.item, "ae2:wireless_terminal" | "ae2:wireless_crafting_terminal"));
        let Some(mut terminal) = terminal else { break };
        let crafting = &*terminal.item == "ae2:wireless_crafting_terminal";
        let mut grid = craft_grid(&terminal);
        match w.as_slice() {
            // MEStorageMenu's network clicks: PICKUP_OR_SET_DOWN (a stack, or half with the right button, onto the
            // empty cursor), SHIFT_CLICK (a stack into the inventory), a craftable entry's request.
            ["take", i, right, shift] => {
                let Ok(i) = i.parse::<usize>() else { continue };
                let Some((key, shown)) = SERVER.with_borrow(|s| s.sent.get(&player).and_then(|v| v.keys.get(i).cloned())) else { continue };
                if shown == 0 {
                    let amount = if *shift == "1" { 64 } else if *right == "1" { 16 } else { 1 };
                    if let Some(net) = SERVER.with_borrow(|s| s.viewers.get(&player).and_then(|p| s.grid_of.get(p)).copied()) {
                        request_craft(game, net, player, &key, amount);
                    }
                    continue;
                }
                if key.key_type() != super::port_ae2_core::KeyType::Items {
                    continue;
                }
                let most = i64::from(max_stack(game, key.id()));
                if *shift == "1" {
                    let got = with_viewer_storage(game, player, |st| st.extract(&key, most, Actionable::Modulate)).unwrap_or(0);
                    if got > 0 && let Some(left) = game.native_give_stack(player, stack_of(&key, got as u32)) {
                        let back = key_of(game, &left);
                        with_viewer_storage(game, player, |st| st.insert(&back, i64::from(left.count), Actionable::Modulate));
                    }
                } else if cursor.is_none() {
                    let have = with_viewer_storage(game, player, |st| st.extract(&key, most, Actionable::Simulate)).unwrap_or(0);
                    let want = if *right == "1" { (have + 1) / 2 } else { have };
                    let got = with_viewer_storage(game, player, |st| st.extract(&key, want, Actionable::Modulate)).unwrap_or(0);
                    if got > 0 {
                        cursor = Some(stack_of(&key, got as u32));
                    }
                }
            }
            // The "+" of a JEI recipe (CraftingTermMenu's transfer): the grid back to the network (what it can't take,
            // to the player), then one of each ingredient from it, the first alternative it has.
            ["fill", cells @ ..] if crafting => {
                for slot in grid.iter_mut() {
                    let Some(st) = slot.take() else { continue };
                    let key = key_of(game, &st);
                    let stored = with_viewer_storage(game, player, |n| n.insert(&key, i64::from(st.count), Actionable::Modulate)).unwrap_or(0);
                    let left = st.count.saturating_sub(stored as u32);
                    if left > 0 {
                        let _ = game.native_give_stack(player, ItemStack { count: left, ..st });
                    }
                }
                for spec in cells {
                    let Some((c, ids)) = spec.split_once('=') else { continue };
                    let Some(c) = c.parse::<usize>().ok().filter(|c| *c < 9) else { continue };
                    for id in ids.split(',') {
                        let key = key_of(game, &ItemStack::new(id, 1));
                        if with_viewer_storage(game, player, |n| n.extract(&key, 1, Actionable::Modulate)).unwrap_or(0) > 0 {
                            grid[c] = Some(stack_of(&key, 1));
                            break;
                        }
                        // Then the player's inventory, as the terminal's own grid does.
                        if game.native_take_items(id, 1) == 1 {
                            grid[c] = Some(ItemStack::new(id, 1));
                            break;
                        }
                    }
                }
            }
            // CREATIVE_DUPLICATE: a full stack on the empty cursor.
            ["dup", i] => {
                let Ok(i) = i.parse::<usize>() else { continue };
                let key = SERVER.with_borrow(|s| s.sent.get(&player).and_then(|v| v.keys.get(i).map(|(k, _)| k.clone())));
                if cursor.is_none()
                    && creative_player(game, player)
                    && let Some(key) = key.filter(|k| k.key_type() == super::port_ae2_core::KeyType::Items)
                {
                    cursor = Some(stack_of(&key, max_stack(game, key.id())));
                }
            }
            ["inv", slot, right] => {
                let Ok(i) = slot.parse::<usize>() else { continue };
                if i == term_slot || i >= game.inventory.len() {
                    continue;
                }
                let mut s = game.inventory[i].take();
                click_slot(game, &mut s, &mut cursor, *right == "1");
                game.inventory[i] = s;
            }
            ["cell", cell, right, shift] => {
                let Ok(i) = cell.parse::<usize>() else { continue };
                if i >= 9 {
                    continue;
                }
                if *shift == "1" {
                    // Shift: the stack to the network (what it doesn't take stays).
                    if let Some(st) = grid[i].take() {
                        let key = key_of(game, &st);
                        let stored = with_viewer_storage(game, player, |n| n.insert(&key, i64::from(st.count), Actionable::Modulate)).unwrap_or(0);
                        let left = st.count.saturating_sub(stored as u32);
                        grid[i] = (left > 0).then(|| ItemStack { count: left, ..st });
                    }
                } else {
                    click_slot(game, &mut grid[i], &mut cursor, *right == "1");
                }
            }
            ["result", shift] => {
                // CraftingTermSlot: as many times as asked (shift: until the inventory is full or it can't),
                // the ingredients from the network first, then the grid.
                let times = if *shift == "1" { 64 } else { 1 };
                for _ in 0..times {
                    let Some((name, result)) = game.res.recipes().find(&grid, 3).map(|(n, r)| (n.to_owned(), r)) else { break };
                    if *shift != "1" {
                        match &cursor {
                            Some(c) if key_of(game, c) != key_of(game, &result) || c.count + result.count > max_stack(game, &c.item) => break,
                            _ => {}
                        }
                    }
                    let remainders = game.res.recipes().remainders(&name, &grid, 3);
                    for (i, cell) in grid.iter_mut().enumerate() {
                        let Some(st) = cell.as_mut() else { continue };
                        let key = key_of(game, &ItemStack { count: 1, ..st.clone() });
                        let from_net = with_viewer_storage(game, player, |n| n.extract(&key, 1, Actionable::Modulate)).unwrap_or(0);
                        if from_net == 0 {
                            st.count -= 1;
                            if st.count == 0 {
                                *cell = None;
                            }
                        }
                        if let Some(Some(rem)) = remainders.get(i) {
                            if cell.is_none() {
                                *cell = Some(rem.clone());
                            } else if let Some(left) = game.native_give_stack(player, rem.clone()) {
                                let k = key_of(game, &left);
                                with_viewer_storage(game, player, |n| n.insert(&k, i64::from(left.count), Actionable::Modulate));
                            }
                        }
                    }
                    if *shift == "1" {
                        if let Some(left) = game.native_give_stack(player, result) {
                            let k = key_of(game, &left);
                            with_viewer_storage(game, player, |n| n.insert(&k, i64::from(left.count), Actionable::Modulate));
                            break;
                        }
                    } else {
                        match cursor.as_mut() {
                            Some(c) => c.count += result.count,
                            None => cursor = Some(result),
                        }
                    }
                }
            }
            ["stash"] | ["stashp"] => {
                for cell in grid.iter_mut() {
                    let Some(st) = cell.take() else { continue };
                    let st = if w[0] == "stashp" {
                        match game.native_give_stack(player, st) {
                            Some(left) => left,
                            None => continue,
                        }
                    } else {
                        st
                    };
                    let key = key_of(game, &st);
                    let stored = with_viewer_storage(game, player, |n| n.insert(&key, i64::from(st.count), Actionable::Modulate)).unwrap_or(0);
                    let left = st.count.saturating_sub(stored as u32);
                    *cell = (left > 0).then(|| ItemStack { count: left, ..st });
                }
            }
            // The cursor into the network: all of it, or one with the right button.
            ["drop"] | ["drop", _] => {
                if let Some(c) = cursor.take() {
                    let key = key_of(game, &c);
                    let n = if w.get(1) == Some(&"1") { 1 } else { c.count };
                    let stored = with_viewer_storage(game, player, |net| net.insert(&key, i64::from(n), Actionable::Modulate)).unwrap_or(0);
                    let left = c.count.saturating_sub(stored as u32);
                    cursor = (left > 0).then(|| ItemStack { count: left, ..c });
                }
            }
            ["close"] => {
                if let Some(c) = cursor.take()
                    && let Some(left) = game.native_give_stack(player, c)
                {
                    let k = key_of(game, &left);
                    with_viewer_storage(game, player, |n| n.insert(&k, i64::from(left.count), Actionable::Modulate));
                }
                SERVER.with_borrow_mut(|s| s.wcraft_slot.remove(&player));
            }
            _ => {}
        }
        if crafting {
            set_craft_grid(game, &mut terminal, &grid);
            if let Some(t) = game.inventory.get_mut(term_slot) {
                *t = Some(terminal);
            }
        }
    }
    let msg = match &cursor {
        Some(c) => format!("cursor {} {}", wire_id(&key_of(game, c)), c.count),
        None => "cursor - 0".to_owned(),
    };
    let _ = game.native_net_send_to_player(player, NET_WIRELESS, msg.as_bytes());
    if let Some(c) = cursor {
        SERVER.with_borrow_mut(|s| s.wcursor.insert(player, c));
    }
}

/// crafting_terminal.json's clearCraftingGrid and clearToPlayerInv: half-size buttons at left 92 and 102, 156 up from
/// the bottom (14 below the rows' foot).
const STASH_X: [f32; 2] = [92.0, 102.0];
const STASH_Y: f32 = 14.0;
const CT_STASH: i32 = (1 << 27) | 1;
const CT_STASH_PLAYER: i32 = (1 << 27) | 2;

/// The stash button under `at` (from the panel's top left) with `rows` network rows above.
fn stash_button(at: [f32; 2], rows: usize) -> Option<i32> {
    let y = 17.0 + 18.0 * rows.max(3) as f32 + STASH_Y;
    if !(y..y + 8.0).contains(&at[1]) {
        return None;
    }
    if (STASH_X[0]..STASH_X[0] + 8.0).contains(&at[0]) {
        Some(CT_STASH)
    } else if (STASH_X[1]..STASH_X[1] + 8.0).contains(&at[0]) {
        Some(CT_STASH_PLAYER)
    } else {
        None
    }
}

/// A carried meteorite compass, five times a second: its item model becomes the compass with its needle toward the
/// nearest meteorite as the player faces (MeteoriteCompassBakedModel.getAnimatedRotation), spinning on the spot and
/// turning slowly with none in reach.
fn compass_tick(game: &mut Game<'_>, slot: i32, player: u128, yaw: f32, at: [f64; 3]) {
    use std::f64::consts::PI;
    let tick = SERVER.with_borrow(|s| s.tick);
    if tick % 4 != 0 || game.native_carried_stack(slot).is_none_or(|s| &*s.item != "ae2:meteorite_compass") {
        return;
    }
    let chunk = [(at[0].floor() as i32) >> 4, (at[2].floor() as i32) >> 4];
    let dim = game.dim.id().to_owned();
    let found = super::port_ae2_meteor::nearest(game, &dim, chunk, 174);
    let player_rot = f64::from(yaw) / 180.0 * PI + PI;
    let angle = match found {
        Some(c) if c == chunk => (tick % 10) as f64 / 10.0 * 2.0 * PI,
        Some(c) => {
            let (up, side) = (f64::from(c[1] - chunk[1]), f64::from(c[0] - chunk[0]));
            (-up).atan2(side) - PI / 2.0 + player_rot
        }
        None => (tick % 60) as f64 / 60.0 * 2.0 * PI,
    };
    let i = ((angle / (PI / 8.0)).round() as i64).rem_euclid(16);
    let model = format!("ae2:hz_compass_full_{i}");
    let _ = player;
    let Some(stack) = game.native_carried_stack(slot) else { return };
    let extra = stack.extra.get_or_insert_with(Default::default);
    let now = extra.other.iter().find(|(k, _)| k == "minecraft:item_model").and_then(|(_, t)| match t {
        Tag::String(j) => j.to_string().ok(),
        _ => None,
    });
    if now.as_deref() == Some(model.as_str()) {
        return;
    }
    extra.other.retain(|(k, _)| k != "minecraft:item_model");
    extra.other.push(("minecraft:item_model".to_owned(), Tag::String(java(&model))));
    extra.other.sort_by(|a, b| a.0.cmp(&b.0));
}

// ---------------------------------------------------------------------------------------------------------
// AE2's JEI ghost ingredients (GhostIngredientHandler, JEI's GhostIngredientDragManager): an item dragged from
// JEI's list or a recipe onto a config slot (bus, interface, level emitter, cell workbench, pattern encoder) sets
// it, as a click there holding it would, without the item.

/// The key JEI's panel hears over the pressed item, so the frame learns what it was (no real key has the name).
const GHOST_KEY: &str = "key.keyboard.ae2.ghost";

#[derive(Default)]
struct GhostDrag {
    /// A left press on JEI's panel, held back until the frame knows what it was on.
    probe: Option<[f32; 2]>,
    /// Where that press was.
    pressed: [f32; 2],
    /// The item being dragged (id, components) and where it was picked up.
    drag: Option<(String, String, [f32; 2])>,
    /// A let-go that came before the frame knew what the press was on (a press and release in one frame).
    released: Option<[f32; 2]>,
    /// A click the panel should still have, where it was (let go where it was pressed).
    replay: Option<[f32; 2]>,
}

thread_local! {
    static GHOST_DRAG: RefCell<GhostDrag> = RefCell::default();
    /// The AE2 screen last drawn, its window's top left and when.
    static GHOST_SCREEN: RefCell<Option<(String, [f32; 2], std::time::Instant)>> = const { RefCell::new(None) };
}

fn ghost_seen(block: &str, origin: [f32; 2]) {
    GHOST_SCREEN.with_borrow_mut(|s| *s = Some((block.to_owned(), origin, std::time::Instant::now())));
}

/// The AE2 screen open now (drawn this last fifth of a second).
fn ghost_screen() -> Option<(String, [f32; 2])> {
    GHOST_SCREEN.with_borrow(|s| {
        s.as_ref().filter(|(_, _, t)| t.elapsed().as_millis() < 200).map(|(b, o, _)| (b.clone(), *o))
    })
}

/// The open screen's config slots (from the window's top left, 16 square) and the menu button a click there is.
fn ghost_targets(block: &str) -> Vec<([f32; 4], i32)> {
    let grid = |n: usize, y: f32| -> Vec<([f32; 4], i32)> {
        (0..n).map(|i| ([8.0 + 18.0 * (i % 9) as f32, y + 18.0 * (i / 9) as f32, 16.0, 16.0], GHOST + ((i as i32) << 3))).collect()
    };
    match block {
        // Only the slots its capacity cards unlock (Slot.isActive): the screen's first data value says how many.
        "ae2:import_bus" | "ae2:export_bus" | "ae2:storage_bus" | "ae2:formation_plane" => {
            let unlocked = CLIENT.with_borrow(|c| c.screen_data.first().copied().unwrap_or(0)).clamp(0, 63) as usize;
            grid(unlocked, 29.0)
        }
        "ae2:cell_workbench" => grid(63, 29.0),
        "ae2:level_emitter" => vec![([137.0, 40.0, 16.0, 16.0], GHOST)],
        "ae2:pattern_encoding_terminal" => {
            let (extra, mode, scroll) = CLIENT.with_borrow(|c| (c.encoder_extra, c.encoder_mode, c.enc_scroll));
            (0..mode_cells(mode))
                .map(|i| {
                    let [x, y] = ghost_cell_mode(i, mode);
                    ([x, y + extra, 16.0, 16.0], GHOST + ((enc_visible(i, mode, scroll) as i32) << 3))
                })
                .collect()
        }
        b if canon(b) == "ae2:interface" => {
            (0..9).map(|i| ([8.0 + 18.0 * i as f32, 53.0, 16.0, 16.0], GHOST + ((i as i32) << 3))).collect()
        }
        _ => Vec::new(),
    }
}

fn ghost_target_at(at: [f32; 2]) -> Option<i32> {
    let (block, [ox, oy]) = ghost_screen()?;
    let [x, y] = [at[0] - ox, at[1] - oy];
    ghost_targets(&block).into_iter().find(|(r, _)| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3]).map(|(_, id)| id)
}

/// `native_screen_mouse_button_pressed_pre`: a left press on JEI's panel (right of the window) over a screen with
/// config slots waits for the frame to learn whether it picked up an item.
pub fn ghost_press(game: &Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, button: i32, canceled: &mut bool) {
    if button != 0 || game.carried.is_some() || !game.native_on("jei") {
        return;
    }
    let Some((block, _)) = ghost_screen() else { return };
    if ghost_targets(&block).is_empty() || screen.mouse[0] < screen.rect[0] + screen.rect[2] + panel_clearance(game) + 8.0 {
        return;
    }
    *canceled = true;
    GHOST_DRAG.with_borrow_mut(|g| g.probe = Some(screen.mouse));
}

/// Once a frame: what the terminal settings ask of the frame: the grid cleared after closing, JEI's search kept in
/// step with the terminal's (or the terminal's taken from JEI's).
fn terminal_frame(game: &mut Game<'_>) {
    if matches!(game.screen, super::Screen::Playing) {
        CLIENT.with_borrow_mut(|c| {
            c.show_jobs = false;
            c.status_block = false;
            c.settings_open = false;
            c.dialog = None;
        });
    }
    if game.native_screen_height().is_none() {
        CLIENT.with_borrow_mut(|c| {
            if c.toolbelt_at.take().is_some() {
                c.ui_out.push("toolbeltdone".to_owned());
            }
        });
    }
    super::native_hooks::port_ae2_screen::set_hidden(game, overlay_shown());
    // The toolbelt lies beyond the window, where a press isn't the screen's: it goes as the slot's button by hand.
    for button in CLIENT.with_borrow_mut(|c| std::mem::take(&mut c.toolbelt_presses)) {
        if let Some(container) = game.remote.as_ref().and_then(|r| r.menu.as_ref()).map(|(w, _)| *w) {
            game.send(horizon_protocol::packets::play::Serverbound::ContainerButtonClick(horizon_protocol::packets::play::ContainerButtonClick { container, button }));
        }
    }
    if CLIENT.with_borrow_mut(|c| std::mem::take(&mut c.relayout)) {
        super::native_hooks::port_ae2_screen::relayout(game);
    }
    let title = game.native_screen_title();
    let crafting = matches!(title.as_deref(), Some("ME Crafting Terminal"));
    let open = crafting || matches!(title.as_deref(), Some("ME Terminal" | "Terminal" | "ME Pattern Encoding Terminal"));
    let (was_crafting, was_open, s) = CLIENT.with_borrow_mut(|c| {
        let s = settings(c);
        (std::mem::replace(&mut c.crafting_term_open, crafting), std::mem::replace(&mut c.term_open, open), s)
    });
    if was_crafting && !crafting && s[5] {
        CLIENT.with_borrow_mut(|c| c.ui_out.push("cleargrid".to_owned()));
    }
    if !open {
        return;
    }
    use super::native_panel::port_ae2_panel as panel;
    if !was_open && s[6] && s[8] {
        panel::set_search(game, "");
    }
    if s[6] {
        // The terminal's list follows JEI's box.
        let text = panel::search_text(game);
        CLIENT.with_borrow_mut(|c| {
            if c.search != text {
                c.search = text;
                c.view.take();
                c.scroll = 0;
            }
            c.search_focus = false;
        });
    } else if s[7] {
        // syncWithExternalSearch: "when either is changed".
        let (mine, last) = CLIENT.with_borrow(|c| (c.search.clone(), c.synced_search.clone()));
        let theirs = panel::search_text(game);
        if mine != last {
            panel::set_search(game, &mine);
            CLIENT.with_borrow_mut(|c| c.synced_search = mine);
        } else if theirs != last {
            CLIENT.with_borrow_mut(|c| {
                c.search = theirs.clone();
                c.synced_search = theirs;
                c.view.take();
                c.scroll = 0;
            });
        }
    }
}

/// `native_frame`: the held-back press asks JEI's panel what it was on (a `Key` event over the item, the pointer
/// put back where it was pressed); not an item (the page arrows, the search box), the panel has the press as it was.
/// A click let go in place is the panel's too.
pub fn ghost_frame(game: &mut Game<'_>) {
    wireless_jei_frame(game);
    terminal_frame(game);
    let (probe, replay) = GHOST_DRAG.with_borrow_mut(|g| (g.probe.take(), g.replay.take()));
    if let Some(at) = replay {
        press_panel_at(game, at);
    }
    let Some(at) = probe else { return };
    GHOST_DRAG.with_borrow_mut(|g| g.pressed = at);
    // A press off the search box takes its focus (its keys would otherwise keep the panel from hearing ours).
    if game.mod_browser_typing() {
        game.mod_browser_closed();
    }
    let cursor = game.cursor;
    game.cursor = (at[0], at[1]);
    game.native_panel_key(GHOST_KEY);
    game.cursor = cursor;
    if GHOST_DRAG.with_borrow(|g| g.drag.is_none()) {
        GHOST_DRAG.with_borrow_mut(|g| g.released = None);
        press_panel_at(game, at);
    } else if let Some(to) = GHOST_DRAG.with_borrow_mut(|g| g.released.take()) {
        ghost_drop(to);
    }
}

/// The panel's own press at `at`.
fn press_panel_at(game: &mut Game<'_>, at: [f32; 2]) {
    let cursor = game.cursor;
    game.cursor = (at[0], at[1]);
    game.mod_browser_press(false);
    game.cursor = cursor;
}

/// `native_panel_event`: the item under the held-back press.
pub fn ghost_panel_event(event: &crate::game::native_hooks::NativePanelEvent) {
    if let crate::game::native_hooks::NativePanelEvent::Key { key, stack } = event
        && key == GHOST_KEY
    {
        GHOST_DRAG.with_borrow_mut(|g| g.drag = Some((stack.item.clone(), stack.components.clone(), g.pressed)));
    }
}

/// `native_screen_mouse_button_released_pre`: dropped on a config slot, the server sets it; let go where it was
/// picked up, it was a click (JEI's recipes, or its give in cheat mode).
pub fn ghost_release(game: &Game<'_>, canceled: &mut bool) {
    let at = [game.cursor.0, game.cursor.1];
    let (pending, dragging) = GHOST_DRAG.with_borrow_mut(|g| {
        if g.probe.is_some() {
            g.released = Some(at);
        }
        (g.probe.is_some(), g.drag.is_some())
    });
    if pending || dragging {
        *canceled = true;
    }
    if dragging {
        ghost_drop(at);
    }
}

/// The dragged item let go at `at`.
fn ghost_drop(at: [f32; 2]) {
    let Some((item, components, from)) = GHOST_DRAG.with_borrow_mut(|g| g.drag.take()) else { return };
    if let Some(id) = ghost_target_at(at) {
        log::info!("ae2: ghost {item} dropped on button {id}");
        CLIENT.with_borrow_mut(|c| c.ui_out.push(format!("ghost {id} {item} {components}")));
    } else if (at[0] - from[0]).abs() < 4.0 && (at[1] - from[1]).abs() < 4.0 {
        GHOST_DRAG.with_borrow_mut(|g| g.replay = Some(from));
    }
}

/// `native_screen_render_post`: the config slots it may go in (GhostIngredientDrag.drawTargets) and the item at the
/// pointer.
pub fn ghost_draw(game: &Game<'_>, batch: &mut HudBatch, mouse: [f32; 2]) {
    let Some((item, _, _)) = GHOST_DRAG.with_borrow(|g| g.drag.clone()) else { return };
    let Some((block, [ox, oy])) = ghost_screen() else {
        GHOST_DRAG.with_borrow_mut(|g| g.drag = None);
        return;
    };
    let hovered = ghost_target_at(mouse);
    for (r, id) in ghost_targets(&block) {
        let colour = if hovered == Some(id) { 0x4000FF00 } else { 0x8822BB33 };
        game.native_fill(batch, [ox + r[0], oy + r[1], r[2], r[3]], colour);
    }
    game.native_item(batch, &item, [mouse[0] - 8.0, mouse[1] - 8.0], 16.0);
}

thread_local! {
    /// Who is clicking the menu button being handled, and their game mode.
    static CLICKER: std::cell::Cell<Option<(u128, i32)>> = const { std::cell::Cell::new(None) };
}

/// `lan_station_button`: the player a menu button is from (Horizon swaps no player in, and has its player list out of
/// the game meanwhile).
pub fn button_clicker(uuid: u128, game_mode: i32) {
    CLICKER.set(Some((uuid, game_mode)));
}

/// The menu button is done.
pub fn button_done() {
    CLICKER.set(None);
}

/// The player a menu button on the block at `pos` is from: its clicker when they have it open, else a viewer.
fn menu_player(pos: Pos) -> Option<u128> {
    SERVER.with_borrow(|s| {
        CLICKER.get().map(|(u, _)| u).filter(|u| s.viewers.get(u) == Some(&pos)).or_else(|| s.viewers.iter().find(|(_, p)| **p == pos).map(|(u, _)| *u))
    })
}

/// Whether player `uuid` plays in creative: the acting player's mode, a menu button's clicker's, a LAN guest's, or
/// (the window's own player, on a path with no list of players) the game's own.
fn creative_player(game: &Game<'_>, uuid: u128) -> bool {
    let acting = game.native_acting_player().filter(|p| p.uuid == uuid).map(|p| p.game_mode);
    let clicker = CLICKER.get().filter(|(u, _)| *u == uuid).map(|(_, m)| m);
    let listed = game.native_server_players().into_iter().find(|p| p.uuid == uuid).map(|p| p.game_mode);
    let owner = game.serving.as_ref().is_some_and(|s| s.owner == uuid);
    acting.or(clicker).or(listed).map_or(owner && game.creative, |m| m == 1)
}

/// The cable with facades as older builds kept it: the cable's own twin on the beacon's template, its arms and
/// lights as `x_` properties.
fn is_legacy_twin(block: &str) -> bool {
    block.starts_with("ae2:hz_fcable_")
}

/// Older builds' cables with facades become the shaped twins: the same cable, arms, lights and facades.
fn migrate_legacy_twins(game: &mut Game<'_>) {
    for (pos, nbt) in SERVER.with_borrow_mut(|s| std::mem::take(&mut s.legacy_twins)) {
        let raw = game.native_block(pos);
        let Some(cable) = raw.strip_prefix("ae2:hz_fcable_").map(str::to_owned) else { continue };
        let arms = TWIN_SIDES
            .iter()
            .enumerate()
            .filter(|(_, k)| game.native_block_property(pos, &format!("x_{k}")).as_deref() == Some("true"))
            .fold(0u32, |m, (i, _)| m | 1 << i);
        let lights = game.native_block_property(pos, "x_lights").unwrap_or_else(|| "0".to_owned());
        let facades = facade_mask(&nbt);
        if facades == 0 {
            if let Some(state) = game.pack.assets.registry.default_state(&format!("ae2:{cable}")) {
                let dim = game.dim.id().to_owned();
                game.native_set_block(&dim, pos, state, 3);
                cable_sides(game, pos);
            }
            continue;
        }
        place_twin(game, pos, &cable, arms, facades);
        game.native_with_mod_block_entity(pos, |e| {
            if let Some(f) = nbt.get(&java("facades")) {
                e.nbt.insert(java("facades"), f.clone());
            }
            e.nbt.insert(java("cable"), Tag::String(java(&cable)));
            e.nbt.insert(java("lights"), Tag::String(java(&lights)));
        });
        game.native_block_entity_sync(pos);
    }
}

/// `PartHostTooltips`: on a cable with facades, the facade the crosshair meets (its stack's name, the facade's
/// block as its icon), else the cable (its item).
pub fn part_host_name(game: &Game<'_>, pos: Pos, block: &str, hit: [f64; 3]) -> Option<(String, String, String)> {
    if !is_twin(block) {
        return None;
    }
    let (cable, facades) = match game.world.block_entities.get(&(pos[0], pos[1], pos[2])) {
        Some(horizon_client::containers::BlockEntity::Mod(m)) => (nbt_string(&m.nbt, "cable"), cable_facades(&m.nbt)),
        _ => return None,
    };
    let at = [hit[0] - f64::from(pos[0]), hit[1] - f64::from(pos[1]), hit[2] - f64::from(pos[2])];
    let near = |v: f64, edge: f64| (v - edge).abs() <= 2.0 / 16.0 + 1e-4;
    let on = |side: &str| match side {
        "down" => near(at[1], 0.0),
        "up" => near(at[1], 1.0),
        "north" => near(at[2], 0.0),
        "south" => near(at[2], 1.0),
        "west" => near(at[0], 0.0),
        _ => near(at[0], 1.0),
    };
    if let Some((_, facade)) = facades.iter().find(|(side, _)| on(side)) {
        let name = game.stack_tooltip(&facade_stack(facade)).first().map(|(t, _)| t.plain()).unwrap_or_default();
        return Some((facade.clone(), name, "ae2".to_owned()));
    }
    let item = format!("ae2:{cable}");
    let name = game.stack_tooltip(&ItemStack::new(&item, 1)).first().map(|(t, _)| t.plain()).unwrap_or_default();
    Some((item, name, "ae2".to_owned()))
}

// ---------------------------------------------------------------------------------------------------------
// JEI on the wireless terminals: AE2's are ordinary container screens, where JEI always shows; here they are mod
// screens, which Horizon's panel skips. While the panel draws or takes input, the game is told an inventory screen is
// open (only then), its width such that the panel lies right of the window and its side column. And keys over a
// terminal's items (JEIPlugin's ClickableIngredient): JEI's and the guide's look them up, as over its own list.

#[derive(Default)]
struct WirelessJei {
    press: Option<([f32; 2], bool)>,
    unfocus: bool,
    typing: bool,
    typed: String,
    backspaces: u32,
    keys: Vec<String>,
    lookups: Vec<(String, String)>,
    was_open: bool,
}

thread_local! {
    static WIRELESS_JEI: RefCell<WirelessJei> = RefCell::default();
    static PANEL_SWAP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The window's width as the panel is told it: centred like the real one, reaching past its side column.
const WIRELESS_PANEL_WIDTH: f32 = 259.0;

fn wireless_open(game: &Game<'_>) -> bool {
    game.native_mod_screen() == Some("ae2") && CLIENT.with_borrow(|c| c.wireless_open) == 2
}

/// Where the panel starts on the wireless screen (its gap past the window).
fn wireless_panel_left(game: &Game<'_>) -> f32 {
    let (w, _) = game.gui_size();
    ((w - WIRELESS_PANEL_WIDTH) / 2.0).floor() + WIRELESS_PANEL_WIDTH + 8.0
}

/// `native_screen_width`, while the panel works for the wireless screen.
pub fn panel_screen_width() -> Option<f32> {
    PANEL_SWAP.get().then_some(WIRELESS_PANEL_WIDTH)
}

fn with_panel<R>(game: &mut Game<'_>, f: impl FnOnce(&mut Game<'_>) -> R) -> R {
    let screen = game.screen;
    game.screen = super::Screen::Inventory;
    PANEL_SWAP.set(true);
    let out = f(game);
    PANEL_SWAP.set(false);
    game.screen = screen;
    out
}

/// FinishedJobToast: top right for 2.5 seconds, when no terminal is open and the player carries a wireless terminal
/// (PendingCraftingJobs.hasNotificationEnablingItem).
pub fn job_toast_draw(game: &Game<'_>, batch: &mut HudBatch) {
    let Some((id, amount, shown)) = CLIENT.with_borrow(|c| c.job_toast.clone()) else { return };
    let since = match shown {
        Some(at) => at.elapsed().as_secs_f32(),
        None => {
            let terminal = CLIENT.with_borrow(|c| c.term_origin.is_some() && c.wireless_open == 0) && game.native_screen_height().is_some();
            let carries = game.inventory.iter().flatten().any(|s| is_wireless_terminal(&s.item));
            if terminal || !carries {
                CLIENT.with_borrow_mut(|c| c.job_toast = None);
                return;
            }
            CLIENT.with_borrow_mut(|c| c.job_toast = Some((id.clone(), amount, Some(std::time::Instant::now()))));
            0.0
        }
    };
    if since >= 2.5 {
        CLIENT.with_borrow_mut(|c| c.job_toast = None);
        return;
    }
    let (w, _) = game.gui_size();
    // Slides in and out over a quarter of a second, as toasts do.
    let slide = (since / 0.25).min((2.5 - since) / 0.25).clamp(0.0, 1.0);
    let x = w - 160.0 * slide;
    if !game.native_sprite(batch, "gui/sprites/toast/recipe", [0.0, 0.0, 160.0, 32.0], [x, 0.0, 160.0, 32.0]) {
        horizon_client::toasts::background(&game.gui.atlas, batch, x, 0.0, 160.0);
    }
    game.native_text(batch, "Auto-Crafting Complete", [x + 30.0, 7.0], 0x500050, false);
    let text = format!("{} {}", key_amount(&id, amount, super::port_ae2_core::AmountFormat::Slot), key_words(game, &id));
    game.native_text(batch, &text, [x + 30.0, 18.0], 0x000000, false);
    draw_key(game, batch, &id, [x + 8.0, 8.0], 16.0);
}

/// `hud_layers`, once the frame's screen has drawn: the panel, its tooltips on top.
pub fn wireless_jei_draw(game: &mut Game<'_>, batch: &mut HudBatch) {
    if !wireless_open(game) {
        return;
    }
    // Over the window the panel has nothing to show (and the inventory screen it is told of has other slots there).
    let cursor = game.cursor;
    if cursor.0 < wireless_panel_left(game) {
        game.cursor = (-10000.0, -10000.0);
    }
    with_panel(game, |g| g.draw_mod_browser(batch));
    game.cursor = cursor;
    let typing = with_panel(game, |g| g.mod_browser_typing());
    WIRELESS_JEI.with_borrow_mut(|j| j.typing = typing);
}

/// A click on the wireless screen: true when it was on the panel. Anywhere else takes the focus off its search box.
fn wireless_jei_click(game: &Game<'_>, mouse: [f32; 2], button: i32) -> bool {
    if mouse[0] < wireless_panel_left(game) {
        WIRELESS_JEI.with_borrow_mut(|j| j.unfocus = true);
        return false;
    }
    if button == 0 || button == 1 {
        WIRELESS_JEI.with_borrow_mut(|j| j.press = Some((mouse, button == 1)));
        CLIENT.with_borrow_mut(|c| c.search_focus = false);
    }
    true
}

/// A key on the wireless screen or a terminal: the panel's search box takes it, or it looks up the item under the
/// pointer. True when it was the search box's.
pub fn jei_key(game: &Game<'_>, menu: Option<&str>, mouse: [f32; 2], key: &str) -> bool {
    let wireless = wireless_open(game);
    if wireless && WIRELESS_JEI.with_borrow(|j| j.typing) {
        if key == "key.keyboard.backspace" {
            WIRELESS_JEI.with_borrow_mut(|j| j.backspaces += 1);
        }
        return key != "key.keyboard.escape";
    }
    if !game.native_on("jei") || CLIENT.with_borrow(|c| c.search_focus || c.dialog.is_some()) {
        return false;
    }
    if wireless && mouse[0] >= wireless_panel_left(game) {
        WIRELESS_JEI.with_borrow_mut(|j| j.keys.push(key.to_owned()));
    } else if let Some(item) = network_item_under(game, menu, mouse) {
        WIRELESS_JEI.with_borrow_mut(|j| j.lookups.push((key.to_owned(), item)));
    }
    false
}

/// A character on the wireless screen: the panel's search box's, when it has focus.
pub fn jei_char(c: char) -> bool {
    WIRELESS_JEI.with_borrow_mut(|j| {
        if j.typing && !c.is_control() {
            j.typed.push(c);
            return true;
        }
        false
    })
}

/// What the frame has to do for the panel, with the game to hand.
fn wireless_jei_frame(game: &mut Game<'_>) {
    let open = wireless_open(game);
    let (press, unfocus, typed, backspaces, keys, lookups, was_open) = WIRELESS_JEI.with_borrow_mut(|j| {
        let was = std::mem::replace(&mut j.was_open, open);
        (j.press.take(), std::mem::take(&mut j.unfocus), std::mem::take(&mut j.typed), std::mem::take(&mut j.backspaces), std::mem::take(&mut j.keys), std::mem::take(&mut j.lookups), was)
    });
    for (key, item) in lookups {
        let stack = crate::game::native_hooks::NativeStack { item, count: 1, components: String::new() };
        game.native_panel_event("jei", &crate::game::native_hooks::NativePanelEvent::Key { key, stack });
    }
    if was_open && !open {
        // The search box lets go, as the game's own close does.
        with_panel(game, |g| g.mod_browser_closed());
        WIRELESS_JEI.with_borrow_mut(|j| j.typing = false);
    }
    if !open {
        return;
    }
    if unfocus {
        with_panel(game, |g| g.mod_browser_closed());
    }
    if let Some((at, right)) = press {
        let cursor = game.cursor;
        game.cursor = (at[0], at[1]);
        with_panel(game, |g| g.mod_browser_press(right));
        game.cursor = cursor;
    }
    if !typed.is_empty() {
        with_panel(game, |g| g.mod_browser_text(Some(&typed), false));
    }
    for _ in 0..backspaces {
        with_panel(game, |g| g.mod_browser_text(None, true));
    }
    for key in keys {
        with_panel(game, |g| g.native_panel_key(&key));
    }
}

/// The wireless crafting terminal is open (a recipe's "+" fills its grid).
pub fn wireless_crafting_open() -> bool {
    CLIENT.with_borrow(|c| c.wireless_open == 2 && c.wireless_craft)
}

/// The network item under the pointer on an open terminal (JEIPlugin's ClickableIngredient).
fn network_item_under(game: &Game<'_>, menu: Option<&str>, mouse: [f32; 2]) -> Option<String> {
    let terminal = wireless_open(game) || matches!(menu, Some("ae2:terminal" | "ae2:crafting_terminal" | "ae2:pattern_encoding_terminal"));
    if !terminal {
        return None;
    }
    let [ox, oy] = CLIENT.with_borrow(|c| c.term_origin)?;
    let rows = CLIENT.with_borrow(|c| c.rows.max(3));
    let [gx, gy] = [mouse[0] - ox - TERM_GRID[0], mouse[1] - oy - TERM_GRID[1]];
    if gx < 0.0 || gy < 0.0 || gx >= TERM_COLS as f32 * 18.0 || gy >= rows as f32 * 18.0 {
        return None;
    }
    let cell = CLIENT.with_borrow(|c| c.scroll * TERM_COLS) + (gy / 18.0) as usize * TERM_COLS + (gx / 18.0) as usize;
    CLIENT.with_borrow(|c| visible(c).get(cell).and_then(|&i| c.items.get(i)).map(|(id, _)| icon_of(id)))
}

/// How far past its width the open screen reaches (AE2's upgrade and view cell columns), which JEI's panel keeps clear
/// (`AEGuiContainerHandler.getGuiExtraAreas`).
pub fn panel_clearance(game: &Game<'_>) -> f32 {
    if CLIENT.with_borrow(|c| c.wireless_open) == 2 {
        return 0.0;
    }
    let Some((block, _)) = ghost_screen() else { return 0.0 };
    let mut shown = None;
    screen(game, &block, &mut shown);
    let Some(s) = shown else { return 0.0 };
    let toolbelt = CLIENT.with_borrow(|c| c.toolbelt_at.map_or(0.0, |_| c.toolbelt_right + 6.0 - s.width));
    s.slots.iter().flatten().filter(|at| at[0] >= s.width).map(|at| at[0] + 18.0 + 6.0 - s.width).fold(toolbelt, f32::max)
}
