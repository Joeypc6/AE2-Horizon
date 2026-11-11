ae2_optional::if_kit_jei_items!("ae2", {
// Where every item comes from: JEI's "Recipes" page also shows what isn't crafted, as Just Enough Resources does.
// Loot tables give the blocks, mobs, chests and other sources of an item, with exact chances worked out from the
// tables themselves. The world generator's own data gives where blocks generate and how much of each ore there is.
// All of it is read once, on a thread of its own, the first time a screen with the panel opens.
use crate::game::Game;
use crate::game::jei::Stack;
use crate::game::hz_kit_window_ae2::{Card, MobDrop, Tab};
// The card helpers mods call as kit_collection's (they are the window's now).
#[allow(unused_imports)] // Mods call them; JEI alone does not.
pub(in crate::game) use crate::game::hz_kit_window_ae2::{source, tab, wrap, wrap_text};
use serde_json::Value;
pub(in crate::game) use crate::game::hz_kit_math_ae2::percent;
use crate::game::hz_kit_math_ae2::{average, range, height_shape, tries, vein_blocks, solid, title_case, trim_count};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::cell::RefCell;
use std::sync::Arc;

const SILK: u8 = 1;
const SHEARS: u8 = 2;
const PLAYER: u8 = 4;
const FORTUNE: u8 = 8;
const LOOTING: u8 = 16;
/// Depends on something this page can't show (the mob's variant, the weather, ...).
const MAYBE: u8 = 32;
/// Depends on the block's state (a grown crop): an `alternatives` entry goes on to the next child.
const STATE: u8 = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Hand,
    Silk,
    Shears,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Chest,
    Archaeology,
    Fishing,
    Barter,
    Shearing,
    Other,
}

#[derive(Clone, Debug)]
enum Source {
    Block(String),
    Mob(String),
    Table(Group, String),
}

#[derive(Clone, Debug)]
struct Drop {
    source: Source,
    chance: f64,
    low: u32,
    high: u32,
    flags: u8,
}

/// How much of one ore a dimension's biomes generate.
#[derive(Clone, Debug)]
struct Ore {
    dimension: usize,
    /// None: every biome of the dimension.
    biomes: Option<Vec<String>>,
    per_chunk: f64,
    /// Its part of all blocks from `low` to `high`.
    share: f64,
    /// Its percent of each layer's blocks, from the world's bottom up.
    layers: Arc<Vec<f32>>,
    low: i32,
    high: i32,
    peak: i32,
}

#[derive(Default)]
struct Natural {
    /// Dimensions whose terrain has it.
    terrain: BTreeSet<usize>,
    /// Dimension → the biomes a feature places it in.
    biomes: BTreeMap<usize, BTreeSet<String>>,
    structures: BTreeSet<String>,
}

#[derive(Default)]
struct Index {
    drops: HashMap<String, Vec<Drop>>,
    natural: HashMap<String, Natural>,
    ores: HashMap<String, Vec<Ore>>,
    /// Each dimension's number of biomes.
    biome_counts: [usize; 3],
    /// A mob's category and the biomes it spawns in, by dimension.
    spawns: HashMap<String, (String, [BTreeSet<String>; 3])>,
}

pub(in crate::game) const DIMENSIONS: [&str; 3] = ["Overworld", "The Nether", "The End"];
/// Each dimension's lowest block and height.
const WORLD: [(i32, i32); 3] = [(-64, 384), (0, 256), (0, 256)];

enum State {
    Idle,
    Building(std::thread::JoinHandle<Index>),
    Ready(Arc<Index>),
}

thread_local! {
    static STATE_NOW: RefCell<State> = const { RefCell::new(State::Idle) };
    /// The villager trades.
    static TRADES: RefCell<Option<Arc<Vec<Trade>>>> = const { RefCell::new(None) };
    /// The tags loot tables name: expanded on the game's thread.
    static TAGS: RefCell<HashMap<String, Vec<String>>> = RefCell::new(HashMap::new());
}

/// Once a frame: starts reading when a screen first opens, so a page never waits for it in play.
pub(in crate::game) fn frame(game: &Game<'_>) {
    if matches!(game.screen, crate::game::Screen::Playing) {
        return;
    }
    STATE_NOW.with_borrow_mut(|s| {
        if matches!(s, State::Idle) {
            let root = game.options.resources.clone();
            let tables = game.res.items().hz_kit_tables_ae2();
            let spawned = std::thread::Builder::new().name("jei-collection".into()).spawn(move || build(&root, &tables));
            match spawned {
                Ok(handle) => *s = State::Building(handle),
                Err(_) => *s = State::Ready(Arc::new(Index::default())),
            }
        }
    });
}

/// The index, waiting for the thread if it isn't done.
fn index() -> Option<Arc<Index>> {
    STATE_NOW.with_borrow_mut(|s| {
        if let State::Building(_) = s {
            let State::Building(handle) = std::mem::replace(s, State::Idle) else { unreachable!() };
            *s = State::Ready(Arc::new(handle.join().unwrap_or_default()));
        }
        match s {
            State::Ready(index) => Some(index.clone()),
            _ => None,
        }
    })
}

// ---------------------------------------------------------------------------------------------------------
// Loot tables.

#[derive(Clone, Copy)]
struct Test {
    pass: bool,
    chance: f64,
    flags: u8,
}

const YES: Test = Test { pass: true, chance: 1.0, flags: 0 };

/// Whether a loot condition's tool predicate matches the tool a source is read for.
fn tool_matches(v: &Value, tool: Tool) -> bool {
    let predicate = v.get("predicate").unwrap_or(&Value::Null);
    let text = predicate.to_string();
    match tool {
        Tool::Hand => false,
        Tool::Silk => text.contains("silk_touch"),
        Tool::Shears => text.contains("shears"),
    }
}

/// One loot condition, tested for the source's tool.
fn condition(v: &Value, tool: Tool) -> Test {
    // A named predicate: the tools are all this page needs of them.
    if let Value::String(name) = v {
        return match (name.contains("silk_touch"), name.contains("shear")) {
            (true, _) => Test { pass: tool == Tool::Silk, ..YES },
            (_, true) => Test { pass: tool == Tool::Shears, ..YES },
            _ => Test { flags: MAYBE, ..YES },
        };
    }
    let kind = v.get("type").or_else(|| v.get("condition")).and_then(Value::as_str).unwrap_or("").trim_start_matches("minecraft:");
    let list = |key: &str| v.get(key).and_then(Value::as_array).map_or(&[][..], |a| a.as_slice());
    match kind {
        "random_chance" => Test { chance: v.get("chance").map_or(1.0, average), ..YES },
        "random_chance_with_enchanted_bonus" => {
            Test { chance: v.get("unenchanted_chance").map_or(1.0, average), flags: LOOTING, ..YES }
        }
        "table_bonus" => Test {
            chance: v.get("chances").and_then(Value::as_array).and_then(|c| c.first()).map_or(1.0, average),
            flags: FORTUNE,
            ..YES
        },
        "killed_by_player" => Test { flags: PLAYER, ..YES },
        "match_tool" => Test { pass: tool_matches(v, tool), ..YES },
        "survives_explosion" => YES,
        "block_state_property" | "match_block" => Test { flags: STATE, ..YES },
        "inverted" => {
            let t = v.get("term").map_or(YES, |t| condition(t, tool));
            if t.flags & (MAYBE | STATE) != 0 { t } else { Test { pass: !t.pass, chance: 1.0, flags: t.flags } }
        }
        "any_of" => {
            let mut out = Test { pass: false, chance: 0.0, flags: 0 };
            for t in list("terms").iter().map(|t| condition(t, tool)) {
                if t.pass {
                    out.pass = true;
                    out.chance = out.chance.max(t.chance);
                    out.flags |= t.flags;
                }
            }
            out
        }
        "all_of" => all(list("terms"), tool),
        _ => Test { flags: MAYBE, ..YES },
    }
}

/// All of a list of conditions (an `all_of`, or an entry's conditions).
fn all(list: &[Value], tool: Tool) -> Test {
    let mut out = YES;
    for t in list.iter().map(|t| condition(t, tool)) {
        out.pass &= t.pass;
        out.chance *= t.chance;
        out.flags |= t.flags;
    }
    out
}

/// An entry's or pool's conditions, tested together.
fn conditions(v: &Value, tool: Tool) -> Test {
    let mut out = v.get("conditions").and_then(Value::as_array).map_or(YES, |l| all(l, tool));
    match v.get("condition") {
        Some(Value::Array(l)) => {
            let t = all(l, tool);
            out = Test { pass: out.pass && t.pass, chance: out.chance * t.chance, flags: out.flags | t.flags };
        }
        Some(c) => {
            let t = condition(c, tool);
            out = Test { pass: out.pass && t.pass, chance: out.chance * t.chance, flags: out.flags | t.flags };
        }
        None => {}
    }
    out
}

#[derive(Clone, Debug)]
struct Got {
    item: String,
    chance: f64,
    low: u32,
    high: u32,
    flags: u8,
}

/// What `functions` make of one stack's count.
fn count_of(v: &Value, got: &mut Got) {
    let one = v.get("modifier").filter(|m| m.is_object());
    let lists = [v.get("functions"), v.get("modifier")];
    let list = lists.iter().flatten().filter_map(|l| l.as_array()).flatten().chain(one);
    for f in list {
        match f.get("type").or_else(|| f.get("function")).and_then(Value::as_str).unwrap_or("").trim_start_matches("minecraft:") {
            "set_count" => {
                let (low, high, chance) = f.get("count").map_or((1, 1, 1.0), range);
                if f.get("add").and_then(Value::as_bool) == Some(true) {
                    got.high += high;
                } else if f.get("conditions").is_some() || f.get("condition").is_some() {
                    // Only in some states (more candles): the count ranges over both.
                    got.low = got.low.min(low);
                    got.high = got.high.max(high);
                } else {
                    got.low = low;
                    got.high = high;
                    got.chance *= chance;
                }
            }
            "enchanted_count_increase" | "looting_enchant" => got.flags |= LOOTING,
            "apply_bonus" => got.flags |= FORTUNE,
            _ => {}
        }
    }
}

/// The stacks an entry gives when it is picked; false when its conditions fail.
fn entry(v: &Value, tables: &horizon_world::loot::LootTables, tool: Tool, depth: u32, out: &mut Vec<Got>) -> bool {
    let test = conditions(v, tool);
    if !test.pass {
        return false;
    }
    let start = out.len();
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("").trim_start_matches("minecraft:");
    let children = v.get("children").and_then(Value::as_array).map_or(&[][..], |c| c.as_slice());
    match kind {
        "item" => {
            if let Some(name) = v.get("name").and_then(Value::as_str) {
                let mut got = Got { item: name.to_owned(), chance: 1.0, low: 1, high: 1, flags: 0 };
                count_of(v, &mut got);
                out.push(got);
            }
        }
        // A tag's items are expanded later, on the game's thread.
        "tag" => {
            // The tag is `name` in the loot table format; some tables (and loot hooks) write it as `items`.
            if let Some(name) = v.get("name").or_else(|| v.get("items")).and_then(Value::as_str) {
                let name = name.trim_start_matches('#');
                let mut got = Got { item: format!("#{name}"), chance: 1.0, low: 1, high: 1, flags: 0 };
                count_of(v, &mut got);
                out.push(got);
            }
        }
        "loot_table" if depth < 6 => {
            let nested = v.get("value").or_else(|| v.get("name"));
            let table = match nested {
                Some(Value::String(id)) => tables.get(id),
                other => other,
            };
            if let Some(table) = table {
                out.extend(table_items(table, tables, tool, depth + 1));
            }
        }
        "group" => {
            for c in children {
                entry(c, tables, tool, depth, out);
            }
        }
        "sequence" => {
            for c in children {
                if !entry(c, tables, tool, depth, out) {
                    break;
                }
            }
        }
        "alternatives" => {
            for c in children {
                let open = conditions(c, tool).flags & (MAYBE | STATE) != 0;
                if entry(c, tables, tool, depth, out) && !open {
                    break;
                }
            }
        }
        _ => {}
    }
    for got in &mut out[start..] {
        got.chance *= test.chance;
        got.flags |= test.flags & !STATE;
    }
    true
}

/// Everything a table can give and its chance per roll of the whole table.
fn table_items(table: &Value, tables: &horizon_world::loot::LootTables, tool: Tool, depth: u32) -> Vec<Got> {
    let mut merged: BTreeMap<String, Got> = BTreeMap::new();
    for pool in table.get("pools").and_then(Value::as_array).into_iter().flatten() {
        let test = conditions(pool, tool);
        if !test.pass {
            continue;
        }
        let rolls = pool.get("rolls").map_or(1.0, average);
        let (_, most, _) = pool.get("rolls").map_or((1, 1, 1.0), range);
        let mut picks: Vec<(f64, Vec<Got>)> = Vec::new();
        let mut total = 0.0;
        for e in pool.get("entries").and_then(Value::as_array).into_iter().flatten() {
            let mut got = Vec::new();
            if entry(e, tables, tool, depth, &mut got) {
                let weight = e.get("weight").and_then(Value::as_f64).unwrap_or(1.0);
                total += weight;
                picks.push((weight, got));
            }
        }
        if total <= 0.0 {
            continue;
        }
        let mut per_roll: BTreeMap<String, Got> = BTreeMap::new();
        for (weight, got) in picks {
            for g in got {
                let p = g.chance * weight / total;
                let e = per_roll.entry(g.item.clone()).or_insert(Got { chance: 0.0, ..g.clone() });
                e.chance += p;
                e.low = e.low.min(g.low);
                e.high = e.high.max(g.high);
                e.flags |= g.flags;
            }
        }
        for (id, mut g) in per_roll {
            let chance = (1.0 - (1.0 - g.chance.min(1.0)).powf(rolls)) * test.chance;
            g.high = g.high.saturating_mul(most.max(1));
            g.flags |= test.flags & !STATE;
            count_of(pool, &mut g);
            let m = merged.entry(id).or_insert(Got { chance: 0.0, low: 0, high: 0, ..g.clone() });
            m.chance = 1.0 - (1.0 - m.chance) * (1.0 - chance);
            m.low = if m.high == 0 { g.low } else { m.low.min(g.low) };
            m.high += g.high;
            m.flags |= g.flags;
        }
    }
    merged.into_values().collect()
}

/// Indexes every loot table: what each block, mob, chest and other source drops.
fn loot(root_tables: &horizon_world::loot::LootTables, index: &mut Index) {
    let mut add = |item: String, drop: Drop| index.drops.entry(item).or_default().push(drop);
    for id in root_tables.hz_kit_ids_ae2() {
        let Some(table) = root_tables.get(&id) else { continue };
        let Some((ns, path)) = id.split_once(':') else { continue };
        let (top, rest) = path.split_once('/').unwrap_or((path, ""));
        match top {
            "blocks" => {
                if rest.contains('/') {
                    continue;
                }
                let hand = table_items(table, root_tables, Tool::Hand, 0);
                let block = format!("{ns}:{rest}");
                let source = || Source::Block(block.clone());
                for g in &hand {
                    add(g.item.clone(), Drop { source: source(), chance: g.chance, low: g.low, high: g.high, flags: g.flags });
                }
                for (tool, flag) in [(Tool::Silk, SILK), (Tool::Shears, SHEARS)] {
                    for g in table_items(table, root_tables, tool, 0) {
                        if !hand.iter().any(|h| h.item == g.item) {
                            add(g.item.clone(), Drop { source: source(), chance: g.chance, low: g.low, high: g.high, flags: g.flags | flag });
                        }
                    }
                }
            }
            "entities" => {
                let mob = rest.split('/').next().unwrap_or(rest);
                let source = Source::Mob(format!("{ns}:{mob}"));
                for g in table_items(table, root_tables, Tool::Hand, 0) {
                    add(g.item.clone(), Drop { source: source.clone(), chance: g.chance, low: g.low, high: g.high, flags: g.flags });
                }
            }
            "equipment" | "shearing_block" => {}
            _ => {
                let group = match top {
                    "chests" | "pots" | "dispensers" | "spawners" => Group::Chest,
                    "archaeology" => Group::Archaeology,
                    "shearing" => Group::Shearing,
                    _ if path.starts_with("gameplay/fishing") => Group::Fishing,
                    _ if path == "gameplay/piglin_bartering" => Group::Barter,
                    _ => Group::Other,
                };
                for g in table_items(table, root_tables, Tool::Hand, 0) {
                    add(g.item.clone(), Drop { source: Source::Table(group.clone(), id.clone()), chance: g.chance, low: g.low, high: g.high, flags: g.flags });
                }
            }
        }
    }
    // Each source once per item: a mob's table and its variants' say the same.
    for list in index.drops.values_mut() {
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut kept: Vec<Drop> = Vec::new();
        for d in list.drain(..) {
            let key = format!("{:?}", d.source);
            match seen.get(&key) {
                Some(&i) => {
                    if d.chance > kept[i].chance {
                        kept[i] = d;
                    }
                }
                None => {
                    seen.insert(key, kept.len());
                    kept.push(d);
                }
            }
        }
        *list = kept;
    }
}

// ---------------------------------------------------------------------------------------------------------
// The world generator's data.

/// One of the worldgen bundle's files: each entry is a JSON text.
fn bundle(root: &std::path::Path, name: &str) -> HashMap<String, Value> {
    let path = root.join("worldgen").join(name);
    let map: HashMap<String, Value> = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    map.into_iter()
        .map(|(k, v)| {
            let v = match &v {
                Value::String(text) => serde_json::from_str(text).unwrap_or(v),
                _ => v,
            };
            (k, v)
        })
        .collect()
}

/// A block id a generator's JSON names, without what only tests for blocks.
fn walk_blocks(v: &Value, maps: &[&HashMap<String, Value>], seen: &mut HashSet<String>, out: &mut BTreeSet<String>) {
    const TESTS: [&str; 15] = [
        "predicate", "target", "replaceable", "can_place_on", "can_grow_through", "can_replace", "placement", "biome", "tag",
        "if_true", "cannot_replace", "invalid_blocks", "valid_base_block", "replaceable_blocks", "density",
    ];
    match v {
        Value::String(s) => {
            if !s.contains(':') || s.starts_with('#') {
                return;
            }
            let mut followed = false;
            for (i, map) in maps.iter().enumerate() {
                if let Some(next) = map.get(s) {
                    followed = true;
                    if seen.insert(format!("{i} {s}")) {
                        walk_blocks(next, maps, seen, out);
                    }
                }
            }
            if !followed {
                out.insert(s.clone());
            }
        }
        Value::Array(a) => a.iter().for_each(|x| walk_blocks(x, maps, seen, out)),
        Value::Object(o) => {
            if o.contains_key("predicate_type") {
                return;
            }
            for (k, x) in o {
                if k != "type" && !TESTS.contains(&k.as_str()) {
                    walk_blocks(x, maps, seen, out);
                }
            }
        }
        _ => {}
    }
}

/// Indexes world generation: where each ore and other feature is placed.
fn worldgen(root: &std::path::Path, index: &mut Index) {
    let (features, placed, biomes, providers, rules, settings) = (
        bundle(root, "features.json"),
        bundle(root, "placed_features.json"),
        bundle(root, "biomes.json"),
        bundle(root, "state_providers.json"),
        bundle(root, "rules.json"),
        bundle(root, "settings.json"),
    );
    let listed = |name: &str| -> BTreeSet<String> {
        let path = root.join("worldgen/biome_parameters").join(name);
        let v: Value = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        v.get("biomes").and_then(Value::as_array).into_iter().flatten().filter_map(|b| b.get("biome")?.as_str().map(str::to_owned)).collect()
    };
    let end: BTreeSet<String> =
        ["the_end", "end_highlands", "end_midlands", "small_end_islands", "end_barrens"].into_iter().map(|b| format!("minecraft:{b}")).collect();
    let dims = [listed("overworld.json"), listed("nether.json"), end];
    for (i, d) in dims.iter().enumerate() {
        index.biome_counts[i] = d.len();
    }
    for (d, list) in dims.iter().enumerate() {
        for biome in list {
            let Some(b) = biomes.get(biome) else { continue };
            let Some(by_category) = b.get("attributes").and_then(|a| a.get("minecraft:gameplay/natural_mob_spawns")).and_then(|s| s.get("argument")).and_then(|a| a.get("spawns_by_category")).and_then(Value::as_object) else {
                continue;
            };
            for (category, list) in by_category {
                for mob in list.as_array().into_iter().flatten().filter_map(|s| s.get("type")?.as_str()) {
                    let e = index.spawns.entry(mob.to_owned()).or_insert_with(|| (category.clone(), Default::default()));
                    e.1[d].insert(biome.clone());
                }
            }
        }
    }
    // Ore: (dimension, biomes it isn't everywhere in) → the blocks per chunk at each Y.
    let mut ore_density: BTreeMap<(String, usize, Option<Vec<String>>), Vec<f64>> = BTreeMap::new();
    // Each placed feature's biomes in each dimension.
    let mut feature_biomes: BTreeMap<String, [BTreeSet<String>; 3]> = BTreeMap::new();
    for (d, list) in dims.iter().enumerate() {
        for biome in list {
            let Some(b) = biomes.get(biome) else { continue };
            for step in b.get("features").and_then(Value::as_array).into_iter().flatten() {
                for f in step.as_array().into_iter().flatten().filter_map(Value::as_str) {
                    feature_biomes.entry(f.to_owned()).or_default()[d].insert(biome.clone());
                }
            }
        }
    }
    let maps: [&HashMap<String, Value>; 3] = [&placed, &features, &providers];
    for (id, per_dim) in &feature_biomes {
        let Some(p) = placed.get(id) else { continue };
        let mut blocks = BTreeSet::new();
        let mut seen = HashSet::new();
        walk_blocks(p, &maps, &mut seen, &mut blocks);
        for block in blocks {
            let n = index.natural.entry(block).or_default();
            for (d, list) in per_dim.iter().enumerate() {
                if !list.is_empty() {
                    n.biomes.entry(d).or_default().extend(list.iter().cloned());
                }
            }
        }
        let Some(config) = p.get("feature").and_then(Value::as_str).and_then(|f| features.get(f)) else { continue };
        let kind = config.get("type").and_then(Value::as_str).unwrap_or("");
        if kind != "minecraft:ore" && kind != "minecraft:scattered_ore" {
            continue;
        }
        let placement = p.get("placement").and_then(Value::as_array).map_or(&[][..], |a| a.as_slice());
        let size = config.get("size").and_then(Value::as_f64).unwrap_or(1.0);
        let targets = config.get("targets").and_then(Value::as_array).map_or(&[][..], |a| a.as_slice());
        let states: Vec<&str> = targets.iter().filter_map(|t| t.get("state").and_then(Value::as_str)).collect();
        let layered = states.iter().any(|s| s.contains("deepslate_")) && states.len() > 1;
        for (d, list) in per_dim.iter().enumerate() {
            if list.is_empty() {
                continue;
            }
            let world = WORLD[d];
            let shape = height_shape(placement, world);
            let blocks_per_chunk = tries(placement) * vein_blocks(size);
            let scope = (list.len() != dims[d].len()).then(|| list.iter().cloned().collect::<Vec<_>>());
            for state in &states {
                let deep = state.contains("deepslate_");
                let e = ore_density.entry(((*state).to_owned(), d, scope.clone())).or_insert_with(|| vec![0.0; world.1 as usize]);
                for (i, chance) in shape.iter().enumerate() {
                    let y = world.0 + i as i32;
                    // Stone gives way to deepslate from Y 8 down to Y 0.
                    let stone = (f64::from(y) / 8.0).clamp(0.0, 1.0);
                    let share = if !layered { 1.0 } else if deep { 1.0 - stone } else { stone };
                    e[i] += chance * blocks_per_chunk * share * solid(d, y);
                }
            }
        }
    }
    for ((state, d, scope), density) in ore_density {
        let total: f64 = density.iter().sum();
        let world = WORLD[d];
        // The heights holding most of it.
        let Some(peak) = density.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|p| p.0) else { continue };
        let most = density[peak] * 0.1;
        let Some(first) = density.iter().position(|v| *v > most) else { continue };
        let last = density.iter().rposition(|v| *v > most).unwrap_or(first);
        index.ores.entry(state).or_default().push(Ore {
            dimension: d,
            biomes: scope,
            per_chunk: total,
            share: density[first..=last].iter().sum::<f64>() / (256.0 * (last - first + 1) as f64),
            layers: Arc::new(density.iter().map(|v| (*v / 256.0 * 100.0) as f32).collect()),
            low: world.0 + first as i32,
            high: world.0 + last as i32,
            peak: world.0 + peak as i32,
        });
    }
    // Terrain: what each dimension's noise settings and surface rules place.
    let rule_maps: [&HashMap<String, Value>; 1] = [&rules];
    for (d, name) in ["minecraft:overworld", "minecraft:nether", "minecraft:end"].into_iter().enumerate() {
        let mut blocks = BTreeSet::new();
        if let Some(s) = settings.get(name) {
            for key in ["default_block", "default_fluid"] {
                if let Some(b) = s.get(key).and_then(Value::as_str) {
                    blocks.insert(b.to_owned());
                }
            }
        }
        if let Some(r) = rules.get(name) {
            let mut seen = HashSet::new();
            walk_blocks(r, &rule_maps, &mut seen, &mut blocks);
        }
        for block in blocks {
            index.natural.entry(block).or_default().terrain.insert(d);
        }
    }
    // Structures: each template's palette, named by its folder.
    let mut stack = vec![root.join("worldgen/structure/minecraft")];
    let base = stack[0].clone();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(name) = path.strip_prefix(&base).ok().and_then(|p| p.components().next()).map(|c| c.as_os_str().to_string_lossy().to_string()) else {
                continue;
            };
            if name.ends_with(".nbt") {
                continue;
            }
            let label = title_case(&name);
            for block in palette(&path) {
                index.natural.entry(block).or_default().structures.insert(label.clone());
            }
        }
    }
    // Structures the game builds in code, not from templates.
    for (name, blocks) in CODE_STRUCTURES {
        for block in *blocks {
            index.natural.entry(format!("minecraft:{block}")).or_default().structures.insert((*name).to_owned());
        }
    }
    for (block, n) in &mut index.natural {
        if matches!(block.as_str(), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air" | "minecraft:jigsaw" | "minecraft:structure_void" | "minecraft:structure_block") {
            n.structures.clear();
            n.biomes.clear();
            n.terrain.clear();
        }
    }
}

const CODE_STRUCTURES: &[(&str, &[&str])] = &[
    ("Mineshaft", &["rail", "oak_planks", "oak_fence", "cobweb", "torch", "dark_oak_planks", "dark_oak_fence"]),
    ("Stronghold", &["stone_bricks", "mossy_stone_bricks", "cracked_stone_bricks", "infested_stone_bricks", "iron_bars", "iron_door", "end_portal_frame", "oak_door", "smooth_stone_slab", "bookshelf", "cobweb", "torch", "ladder", "oak_fence"]),
    ("Ocean Monument", &["prismarine", "prismarine_bricks", "dark_prismarine", "sea_lantern", "gold_block", "wet_sponge", "sponge"]),
    ("Nether Fortress", &["nether_bricks", "nether_brick_fence", "nether_brick_stairs", "soul_sand", "nether_wart"]),
    ("Desert Pyramid", &["sandstone", "chiseled_sandstone", "cut_sandstone", "sandstone_stairs", "sandstone_slab", "orange_terracotta", "blue_terracotta", "tnt", "stone_pressure_plate", "suspicious_sand"]),
    ("Jungle Temple", &["cobblestone", "mossy_cobblestone", "chiseled_stone_bricks", "cobblestone_stairs", "dispenser", "tripwire_hook", "redstone_wire", "sticky_piston", "repeater", "lever", "vine"]),
    ("Swamp Hut", &["spruce_planks", "oak_log", "spruce_stairs", "oak_fence", "crafting_table", "cauldron", "potted_red_mushroom"]),
    ("End Ship", &["purpur_block", "purpur_stairs", "purpur_slab", "end_stone_bricks", "magenta_stained_glass", "brewing_stand", "dragon_head"]),
];

/// A template's blocks.
fn palette(path: &std::path::Path) -> Vec<String> {
    use std::io::Read;
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let mut raw = Vec::new();
    if flate2::read::GzDecoder::new(bytes.as_slice()).read_to_end(&mut raw).is_err() {
        return Vec::new();
    }
    let Ok((named, _)) = horizon_world::nbt::read_named(&raw, horizon_world::nbt::Limits::templates()) else { return Vec::new() };
    let mut out = Vec::new();
    let mut read = |list: &[horizon_world::nbt::Tag]| {
        for state in list {
            if let Some(horizon_world::nbt::Tag::String(name)) = state.get("id").or_else(|| state.get("Name")) {
                out.push(name.to_string_lossy());
            }
        }
    };
    match named.tag.get("palette") {
        Some(horizon_world::nbt::Tag::List(list)) => read(list),
        _ => {
            if let Some(horizon_world::nbt::Tag::List(lists)) = named.tag.get("palettes") {
                for list in lists {
                    if let horizon_world::nbt::Tag::List(list) = list {
                        read(list);
                    }
                }
            }
        }
    }
    out
}

/// The whole index, built on the collection thread.
fn build(root: &std::path::Path, tables: &horizon_world::loot::LootTables) -> Index {
    let started = std::time::Instant::now();
    let mut index = Index::default();
    loot(tables, &mut index);
    worldgen(root, &mut index);
    let in_structures = index.natural.values().filter(|n| !n.structures.is_empty()).count();
    log::info!("jei: {} items' sources, {} natural blocks ({in_structures} in structures), {} ores read in {:?}", index.drops.len(), index.natural.len(), index.ores.len(), started.elapsed());
    index
}

// ---------------------------------------------------------------------------------------------------------
// Pages.

/// A drop's count range as JER writes it ("x1-3").
fn amount(d: &Drop) -> String {
    if d.low == d.high { format!("x{}", d.high) } else { format!("x{}-{}", d.low, d.high) }
}

/// What a drop needs or gets besides: Silk Touch, shears, Looting, Fortune.
fn extras(flags: u8) -> String {
    let mut out = String::new();
    for (bit, text) in [
        (SILK, ", Silk Touch"),
        (SHEARS, ", shears"),
        (PLAYER, ", player kill"),
        (FORTUNE, ", more with Fortune"),
        (LOOTING, ", more with Looting"),
        (MAYBE, ", sometimes"),
    ] {
        if flags & bit != 0 {
            out.push_str(text);
        }
    }
    out
}

/// An item's name, or a block's that has no item (a crop).
fn label(game: &Game<'_>, id: &str) -> String {
    if game.res.items().get(id).is_some() {
        return game.pack.names.display_item(id).to_string();
    }
    let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
    game.translate(&format!("block.{ns}.{path}"), &title_case(path))
}

/// A mob's name in the game's language.
fn entity_name(game: &Game<'_>, id: &str) -> String {
    let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
    game.translate(&format!("entity.{ns}.{path}"), &title_case(path))
}

/// A biome's name in the game's language.
fn biome_name(game: &Game<'_>, id: &str) -> String {
    let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
    game.translate(&format!("biome.{ns}.{path}"), &title_case(path))
}

/// A list of biomes as a card line: "All biomes" when it is every one of the dimension.
fn biome_list(game: &Game<'_>, biomes: impl Iterator<Item = String>, of: usize) -> String {
    let list: Vec<String> = biomes.collect();
    if list.len() >= of && of > 0 {
        return "All biomes".to_owned();
    }
    let names: Vec<String> = list.iter().take(6).map(|b| biome_name(game, b)).collect();
    let more = list.len().saturating_sub(names.len());
    if more > 0 { format!("{} and {more} more", names.join(", ")) } else { names.join(", ") }
}

/// A tag's items, expanded on the game's thread and kept.
fn tag_items(game: &Game<'_>, tag: &str) -> Vec<String> {
    if let Some(list) = TAGS.with_borrow(|t| t.get(tag).cloned()) {
        return list;
    }
    let list = game.native_recipe_ingredient(&Value::String(tag.to_owned())).map_or_else(Vec::new, |i| i.order.iter().map(ToString::to_string).collect::<Vec<_>>());
    TAGS.with_borrow_mut(|t| t.insert(tag.to_owned(), list.clone()));
    list
}

/// Every drop of `item`, its tags' entries too.
fn drops_of(game: &Game<'_>, index: &Index, item: &str) -> Vec<Drop> {
    let mut out = index.drops.get(item).cloned().unwrap_or_default();
    for (key, list) in &index.drops {
        if key.starts_with('#') && tag_items(game, key).iter().any(|i| i == item) {
            out.extend(list.iter().cloned());
        }
    }
    out
}

/// What a source gives, as a list: item, its drop.
fn given_by(game: &Game<'_>, index: &Index, wanted: impl Fn(&Source) -> bool) -> Vec<(String, Drop)> {
    let mut list = Vec::new();
    for (item, drops) in &index.drops {
        for d in drops.iter().filter(|d| wanted(&d.source)) {
            if item.starts_with('#') {
                list.extend(tag_items(game, item).into_iter().map(|i| (i, d.clone())));
            } else {
                list.push((item.clone(), d.clone()));
            }
        }
    }
    list.sort_by(|a, b| b.1.chance.total_cmp(&a.1.chance).then_with(|| a.0.cmp(&b.0)));
    list
}

/// World Gen cards for a block: each placement with its heights, chances and drops.
fn ore_cards(game: &Game<'_>, index: &Index, block: &str, cards: &mut Vec<Card>) {
    let Some(ores) = index.ores.get(block) else { return };
    let mut ores = ores.clone();
    ores.sort_by(|a, b| a.dimension.cmp(&b.dimension).then_with(|| a.biomes.is_some().cmp(&b.biomes.is_some())));
    let drops: Vec<String> = {
        let mut list: Vec<String> = given_by(game, index, |s| matches!(s, Source::Block(b) if b == block))
            .into_iter()
            .filter(|(_, d)| d.flags & (SILK | SHEARS) == 0)
            .map(|(i, _)| i)
            .collect();
        // Once each, in chance order: an item named both on its own and through a tag can be apart in the list.
        let mut seen = HashSet::new();
        list.retain(|i| seen.insert(i.clone()));
        list
    };
    for ore in ores {
        let scope = match &ore.biomes {
            None => "All biomes".to_owned(),
            Some(b) => biome_list(game, b.iter().cloned(), index.biome_counts[ore.dimension]),
        };
        let scope = wrap(game, &scope).into_iter().next().unwrap_or_default();
        let (bottom, _) = WORLD[ore.dimension];
        let summary = vec![format!("{} of blocks, Y {} to {}", percent(ore.share), ore.low, ore.high), format!("Most at Y {}, about {} a chunk", ore.peak, trim_count(ore.per_chunk))];
        cards.push(Card::Ore {
            dimension: ore.dimension,
            name: DIMENSIONS[ore.dimension].to_owned(),
            block: block.to_owned(),
            scope,
            bottom,
            share: ore.layers.clone(),
            summary,
            drops: drops.clone(),
        });
    }
}

/// What a mob says of itself on its card.
fn mob_info(game: &Game<'_>, index: &Index, mob: &str) -> Vec<String> {
    let mut info = Vec::new();
    let mut category = "";
    match index.spawns.get(mob) {
        Some((c, by_dim)) => {
            category = c.as_str();
            let mut parts = Vec::new();
            for (d, biomes) in by_dim.iter().enumerate() {
                if !biomes.is_empty() {
                    let list = biome_list(game, biomes.iter().cloned(), index.biome_counts[d]);
                    parts.push(if list == "All biomes" { DIMENSIONS[d].to_owned() } else { list });
                }
            }
            info.extend(wrap(game, &format!("Spawn Biome: {}", parts.join(", "))).into_iter().take(2));
        }
        None => info.push("Spawn Biome: not natural".to_owned()),
    }
    match category {
        "monster" => info.push("Light Level: 0".to_owned()),
        "creature" => info.push("Light Level: 9 or more".to_owned()),
        _ => {}
    }
    info.push(format!("Experience: {}", experience(mob, category)));
    info
}

/// `getBaseExperienceReward` of a kill, for the mobs that differ from their kind.
fn experience(mob: &str, category: &str) -> &'static str {
    match mob.trim_start_matches("minecraft:") {
        "ender_dragon" => "500 (12000 the first time)",
        "wither" => "50",
        "ravager" | "piglin_brute" => "20",
        "blaze" | "guardian" | "elder_guardian" | "evoker" | "illusioner" | "breeze" => "10",
        "slime" | "magma_cube" => "by size",
        "villager" | "wandering_trader" | "iron_golem" | "snow_golem" | "allay" | "bat" => "0",
        "zombie" | "husk" | "drowned" | "skeleton" | "stray" | "bogged" | "creeper" | "spider" | "cave_spider" | "enderman" | "ghast" | "hoglin" | "zoglin" | "phantom" | "piglin" | "pillager" | "shulker"
        | "silverfish" | "endermite" | "vex" | "vindicator" | "warden" | "witch" | "wither_skeleton" | "zombie_villager" | "zombified_piglin" => "5",
        _ if category == "monster" => "5",
        _ => "1-3",
    }
}

/// A Mob Drops card: the mob, its spawn info and every drop of its table.
fn mob_card(game: &Game<'_>, index: &Index, mob: &str, wanted: &str) -> Card {
    let list = given_by(game, index, |s| matches!(s, Source::Mob(m) if m == mob));
    let mut seen = HashSet::new();
    let mut drops = Vec::new();
    for (item, d) in &list {
        if seen.insert(item.clone()) {
            drops.push(MobDrop { item: item.clone(), count: d.high.max(1), line: format!("Drop chance: {} {}{}", percent(d.chance), amount(d), extras(d.flags)) });
        }
    }
    Card::Mob { mob: mob.to_owned(), name: entity_name(game, mob), info: mob_info(game, index, mob), drops, wanted: wanted.to_owned() }
}

const WORK_STATIONS: [(&str, &str); 13] = [
    ("armorer", "blast_furnace"),
    ("butcher", "smoker"),
    ("cartographer", "cartography_table"),
    ("cleric", "brewing_stand"),
    ("farmer", "composter"),
    ("fisherman", "barrel"),
    ("fletcher", "fletching_table"),
    ("leatherworker", "cauldron"),
    ("librarian", "lectern"),
    ("mason", "stonecutter"),
    ("shepherd", "loom"),
    ("toolsmith", "smithing_table"),
    ("weaponsmith", "grindstone"),
];

/// One villager trade.
struct Trade {
    who: String,
    level: Option<u32>,
    wants: Vec<Stack>,
    gives: Stack,
}

/// The villager trades, read once from the game's stations.
fn trades(game: &Game<'_>) -> Arc<Vec<Trade>> {
    if let Some(t) = TRADES.with_borrow(Clone::clone) {
        return t;
    }
    let stack = |v: &Value| Some(Stack { item: v.get("id")?.as_str()?.to_owned(), count: v.get("count").and_then(Value::as_u64).unwrap_or(1) as u32, components: String::new() });
    let mut list: Vec<Trade> = Vec::new();
    for (id, trade) in &game.res.stations().trades.trades {
        let path = id.split_once(':').map_or(id.as_str(), |p| p.1);
        let mut parts = path.split('/');
        let who = parts.next().unwrap_or(path).to_owned();
        let level = parts.next().and_then(|l| l.parse().ok());
        let Some(gives) = trade.get("gives").and_then(stack) else { continue };
        let mut wants: Vec<Stack> = trade.get("wants").and_then(stack).into_iter().collect();
        wants.extend(trade.get("additional_wants").and_then(stack));
        list.push(Trade { who, level, wants, gives });
    }
    list.sort_by(|a, b| a.who.cmp(&b.who).then_with(|| a.level.cmp(&b.level)).then_with(|| a.gives.item.cmp(&b.gives.item)));
    let list = Arc::new(list);
    TRADES.with_borrow_mut(|t| *t = Some(list.clone()));
    list
}

/// One trade as a card: what the villager wants and gives.
fn trade_card(t: &Trade) -> Card {
    let wandering = t.who == "wandering_trader";
    let profession = horizon_client::mobs::render::PROFESSIONS.iter().position(|p| *p == t.who).map(|p| p as u8);
    let station = WORK_STATIONS.iter().find(|(w, _)| *w == t.who).map(|(_, b)| format!("minecraft:{b}"));
    Card::Trade {
        who: title_case(&t.who),
        station,
        level: t.level,
        profession: if wandering { None } else { profession },
        mob: if wandering { "minecraft:wandering_trader".to_owned() } else { "minecraft:villager".to_owned() },
        wants: t.wants.clone(),
        gives: t.gives.clone(),
    }
}

/// The tabs of how `item` is got.
pub(in crate::game) fn got_tabs(game: &Game<'_>, item: &str) -> Vec<Tab> {
    let mut tabs = Vec::new();
    let Some(index) = index() else { return tabs };
    let mut drops = drops_of(game, &index, item);
    drops.sort_by(|a, b| b.chance.total_cmp(&a.chance).then_with(|| format!("{:?}", a.source).cmp(&format!("{:?}", b.source))));

    // World generation: ores' graphs (this block's, or that of the block that drops it), and where it generates.
    let mut world = Vec::new();
    ore_cards(game, &index, item, &mut world);
    let mut seen_blocks = HashSet::new();
    for d in &drops {
        if let Source::Block(block) = &d.source
            && block != item
            && seen_blocks.insert(block.clone())
        {
            ore_cards(game, &index, block, &mut world);
        }
    }
    if let Some(n) = index.natural.get(item) {
        for d in &n.terrain {
            world.push(Card::World { dimension: *d, name: DIMENSIONS[*d].to_owned(), lines: wrap(game, "Part of the terrain") });
        }
        let graphed: Vec<usize> = world.iter().filter_map(|c| if let Card::Ore { dimension, block, .. } = c { (block == item).then_some(*dimension) } else { None }).collect();
        for (d, biomes) in n.biomes.iter().filter(|(d, _)| !graphed.contains(d)) {
            let list = biome_list(game, biomes.iter().cloned(), index.biome_counts[*d]);
            world.push(Card::World { dimension: *d, name: DIMENSIONS[*d].to_owned(), lines: wrap(game, &list) });
        }
        if !n.structures.is_empty() {
            let list: Vec<&str> = n.structures.iter().map(String::as_str).collect();
            world.push(Card::Text(wrap(game, &format!("In structures: {}", list.join(", ")))));
        }
    }
    if !world.is_empty() {
        tabs.push(tab("minecraft:grass_block", "World Gen", &[], world));
    }

    // Broken blocks.
    let mut cards = Vec::new();
    for d in &drops {
        if let Source::Block(block) = &d.source {
            let name = label(game, block);
            let from = if game.res.items().get(block).is_some() { block.as_str() } else { item };
            cards.push(source(game, from, item, d.high, &format!("{name}: {} {}{}", percent(d.chance), amount(d), extras(d.flags))));
        }
    }
    if !cards.is_empty() {
        tabs.push(tab("minecraft:diamond_pickaxe", "Block Drops", &["minecraft:diamond_pickaxe"], cards));
    }

    // Mobs.
    // One card per mob: its table's drops, and what its code drops besides (a creeper's discs) on the same card.
    let mut cards = Vec::new();
    let mut mobs = HashSet::new();
    for d in &drops {
        if let Source::Mob(mob) = &d.source
            && game.res.mob_types().contains_key(mob.as_str())
            && mobs.insert(mob.clone())
        {
            cards.push(mob_card(game, &index, mob, item));
        }
    }
    for (_, mob, note) in CODE_DROPS.iter().filter(|(i, _, _)| *i == item) {
        let card = cards.iter_mut().find(|c| matches!(c, Card::Mob { mob: m, .. } if m == mob));
        if let Some(Card::Mob { drops, .. }) = card {
            match drops.iter_mut().find(|x| x.item == item) {
                Some(x) => x.line = format!("{} ({note})", x.line),
                None => drops.push(MobDrop { item: item.to_owned(), count: 1, line: (*note).to_owned() }),
            }
            continue;
        }
        cards.push(Card::Mob {
            mob: (*mob).to_owned(),
            name: entity_name(game, mob),
            info: mob_info(game, &index, mob),
            drops: vec![MobDrop { item: item.to_owned(), count: 1, line: (*note).to_owned() }],
            wanted: item.to_owned(),
        });
    }
    if !cards.is_empty() {
        tabs.push(tab("minecraft:zombie_head", "Mob Drops", &[], cards));
    }

    // Everything else with a loot table.
    for (group, icon, title) in [
        (Group::Chest, "minecraft:chest", "Chest Loot"),
        (Group::Archaeology, "minecraft:brush", "Archaeology"),
        (Group::Fishing, "minecraft:fishing_rod", "Fishing"),
        (Group::Barter, "minecraft:gold_ingot", "Piglin Bartering"),
        (Group::Shearing, "minecraft:shears", "Shearing"),
        (Group::Other, "minecraft:knowledge_book", "Other Sources"),
    ] {
        let mut cards = Vec::new();
        for d in &drops {
            if let Source::Table(g, id) = &d.source
                && *g == group
            {
                let path = id.split_once(':').map_or(id.as_str(), |p| p.1);
                let path = ["gameplay/", "chests/", "archaeology/", "shearing/"].iter().find_map(|p| path.strip_prefix(p)).unwrap_or(path);
                cards.push(source(game, icon, item, d.high, &format!("{}: {} {}{}", title_case(path), percent(d.chance), amount(d), extras(d.flags))));
            }
        }
        if !cards.is_empty() {
            let catalysts: &[&str] = if group == Group::Other { &[] } else { std::slice::from_ref(&icon) };
            tabs.push(tab(icon, title, catalysts, cards));
        }
    }

    // Villager trades that give it.
    let cards: Vec<Card> = trades(game).iter().filter(|t| t.gives.item == item).map(trade_card).collect();
    if !cards.is_empty() {
        tabs.push(tab("minecraft:emerald", "Villager Trades", &[], cards));
    }

    // What a mod's world generation gives.
    tabs.extend(crate::game::hz_kit_layer_ae2::provider_sources(game, item));

    // What no table says.
    if let Some(note) = SPECIAL.iter().find(|(i, _)| *i == item).map(|(_, n)| *n).or_else(|| crate::game::hz_kit_layer_ae2::provider_note(item)) {
        tabs.push(tab("minecraft:book", "How To Get It", &[], vec![Card::Text(wrap(game, note))]));
    } else if item.ends_with("_spawn_egg") {
        tabs.push(tab("minecraft:book", "How To Get It", &[], vec![Card::Text(wrap(game, "Creative mode only."))]));
    }
    tabs
}

/// An item's uses from here: what a spawn egg's mob drops, and the trades that want it.
pub(in crate::game) fn use_tabs(game: &Game<'_>, item: &str) -> Vec<Tab> {
    let mut tabs = Vec::new();
    let Some(index) = index() else { return tabs };
    if let Some(mob) = item.strip_suffix("_spawn_egg")
        && game.res.mob_types().contains_key(mob)
    {
        tabs.push(tab("minecraft:zombie_head", "Mob Drops", &[], vec![mob_card(game, &index, mob, "")]));
    }
    let cards: Vec<Card> = trades(game).iter().filter(|t| t.wants.iter().any(|w| w.item == item)).map(trade_card).collect();
    if !cards.is_empty() {
        tabs.push(tab("minecraft:emerald", "Villager Trades", &[], cards));
    }
    tabs
}

const CODE_DROPS: &[(&str, &str, &str)] = &[
    ("minecraft:nether_star", "minecraft:wither", "Wither: 100% x1"),
    ("minecraft:music_disc_11", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_13", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_blocks", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_cat", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_chirp", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_far", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_mall", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_mellohi", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_stal", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_strad", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_wait", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
    ("minecraft:music_disc_ward", "minecraft:creeper", "Creeper killed by a skeleton or bogged"),
];

const SPECIAL: &[(&str, &str)] = &[
    ("minecraft:blue_egg", "Laid by cold chickens."),
    ("minecraft:brown_egg", "Laid by warm chickens."),
    ("minecraft:budding_amethyst", "Lines amethyst geodes; breaks to nothing, so it can't be collected in survival."),
    ("minecraft:chorus_plant", "Grows on end stone in the End Highlands; breaks into chorus fruit, so only creative mode holds it."),
    ("minecraft:dirt_path", "Use a shovel on dirt or grass; Silk Touch keeps it."),
    ("minecraft:farmland", "Use a hoe on dirt or grass; Silk Touch keeps it."),
    ("minecraft:frogspawn", "Laid by frogs on water; can't be collected in survival."),
    ("minecraft:flow_pottery_sherd", "From decorated pots in Trial Chambers."),
    ("minecraft:guster_pottery_sherd", "From decorated pots in Trial Chambers."),
    ("minecraft:scrape_pottery_sherd", "From decorated pots in Trial Chambers."),
    ("minecraft:infested_chiseled_stone_bricks", "Generated in strongholds; turns to a silverfish when broken, so only creative mode holds it."),
    ("minecraft:infested_cobblestone", "Generated in igloos and woodland mansions; can't be collected in survival."),
    ("minecraft:infested_cracked_stone_bricks", "Generated in strongholds; can't be collected in survival."),
    ("minecraft:infested_mossy_stone_bricks", "Generated in strongholds; can't be collected in survival."),
    ("minecraft:infested_stone_bricks", "Generated in strongholds; can't be collected in survival."),
    ("minecraft:infested_stone", "Generated in mountains; can't be collected in survival."),
    ("minecraft:infested_deepslate", "Generated in deep mountains; can't be collected in survival."),
    ("minecraft:reinforced_deepslate", "Around the Ancient City's center; can't be collected in survival."),
    ("minecraft:spawner", "Dungeons, strongholds, mineshafts, fortresses and bastions; can't be collected in survival."),
    ("minecraft:trial_spawner", "Trial Chambers; can't be collected in survival."),
    ("minecraft:vault", "Trial Chambers; can't be collected in survival."),
    ("minecraft:sulfur_cube_bucket", "Use a water bucket on a sulfur cube."),
    ("minecraft:suspicious_gravel", "Trail ruins and cold ocean ruins; only creative mode holds it. Use a brush for its loot."),
    ("minecraft:suspicious_sand", "Desert wells, pyramids and warm ocean ruins; only creative mode holds it. Use a brush for its loot."),
    ("minecraft:water_bucket", "Use a bucket on a water source."),
    ("minecraft:lava_bucket", "Use a bucket on a lava source."),
    ("minecraft:milk_bucket", "Use a bucket on a cow, goat or mooshroom."),
    ("minecraft:powder_snow_bucket", "Use a bucket on powder snow."),
    ("minecraft:cod_bucket", "Use a water bucket on a cod."),
    ("minecraft:salmon_bucket", "Use a water bucket on a salmon."),
    ("minecraft:pufferfish_bucket", "Use a water bucket on a pufferfish."),
    ("minecraft:tropical_fish_bucket", "Use a water bucket on a tropical fish."),
    ("minecraft:axolotl_bucket", "Use a water bucket on an axolotl."),
    ("minecraft:tadpole_bucket", "Use a water bucket on a tadpole."),
    ("minecraft:glass_bottle", "Crafted, or dropped by witches."),
    ("minecraft:honey_bottle", "Use a glass bottle on a full bee nest or beehive."),
    ("minecraft:dragon_breath", "Use a glass bottle in the Ender Dragon's breath."),
    ("minecraft:dragon_egg", "Appears on the exit portal after the first Ender Dragon kill."),
    ("minecraft:elytra", "In an item frame on End Ships, in End Cities."),
    ("minecraft:ominous_bottle", "Dropped by raid captains; found in ominous trial vaults."),
    ("minecraft:experience_bottle", "Clerics sell them; also found in chests."),
    ("minecraft:filled_map", "Use an empty map; cartographers sell explorer maps."),
    ("minecraft:written_book", "Sign a book and quill."),
    ("minecraft:firework_star", "Crafted with gunpowder and dye."),
    ("minecraft:player_head", "Dropped by a player killed by a charged creeper."),
    ("minecraft:command_block", "Creative mode only, with /give."),
    ("minecraft:chain_command_block", "Creative mode only, with /give."),
    ("minecraft:repeating_command_block", "Creative mode only, with /give."),
    ("minecraft:command_block_minecart", "Creative mode only, with /give."),
    ("minecraft:barrier", "Creative mode only, with /give."),
    ("minecraft:light", "Creative mode only, with /give."),
    ("minecraft:structure_block", "Creative mode only, with /give."),
    ("minecraft:structure_void", "Creative mode only, with /give."),
    ("minecraft:jigsaw", "Creative mode only, with /give."),
    ("minecraft:debug_stick", "Creative mode only, with /give."),
    ("minecraft:knowledge_book", "Creative mode only, with /give."),
    ("minecraft:test_block", "Creative mode only, with /give."),
    ("minecraft:test_instance_block", "Creative mode only, with /give."),
];
});
