// AE2's meteorites (appeng.worldgen.meteorite, AE2 15.4.12 for MC 1.20.1): the `ae2:meteorite` structure set
// (random_spread, spacing 32, separation 8, salt 124895654), MeteoriteStructure's start (findGenerationPoint,
// generatePieces) and MeteoritePlacer, run once in each chunk the piece's box covers, as vanilla's structure step
// does. Horizon doesn't run AE2's structure type, so a chunk that was just generated (`native_chunk_load`) asks for
// the starts whose box (the meteor's chunk +-64 blocks, so +-4 chunks) covers it. A worker thread works each start
// out from the noise generator (heights and noise biomes, as `ChunkGenerator.getBaseHeight` and
// `BiomeSource.getNoiseBiome` give them), and the level's next ticks place the start's part clipped to the chunk,
// with the chunk's own decoration random. No `//!` comments: module payloads can't hold inner attributes.

use super::Game;
use horizon_client::streaming::ChunkGenerator;
use horizon_client::vanillagen::VanillaTerrain;
use horizon_core::biome::{BiomeZoom, obfuscate_seed};
use horizon_core::position::BlockPos;
use horizon_core::random::{RandomSource, XoroshiroRandom};
use horizon_world::registry::{BlockRegistry, BlockStateId};
use horizon_world::worldgen::biome_temperature::{BiomeTemperatureSettings, TemperatureModifier};
use horizon_world::worldgen::generator::GeneratorWorker;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

const ID: &str = "ae2";
/// `data/ae2/worldgen/structure_set/meteorite.json`: random_spread, linear.
const SPACING: i32 = 32;
const SEPARATION: i32 = 8;
const SALT: i32 = 124895654;
/// MeteoriteStructurePiece.createBoundingBox: the meteor's chunk +-64 blocks, so the chunks +-4 around it.
const REACH: i32 = 4;
/// `ChunkGenerator.getSeaLevel()` (and `WorldGenRegion.getSeaLevel()`) in the overworld presets; AE2 1.20.1's
/// `Biome.getHeightAdjustedTemperature` cools above y 80 (sea level + 17).
const SEA_LEVEL: i32 = 63;
/// The meteorite is the first structure (index 0) of GenerationStep.Decoration.TOP_LAYER_MODIFICATION (10).
const STEP: i32 = 10;
const STEP_INDEX: i32 = 0;
/// `WorldGenRegion.setBlock` writes the proto chunk straight: no neighbour or shape updates, no `onPlace`
/// (no fluid or falling ticks), no block entity side effects. Players still get every write.
const WORLDGEN_FLAGS: i32 =
    horizon_world::level::flags::UPDATE_CLIENTS | horizon_world::level::flags::UPDATE_SKIP_ALL_SIDEEFFECTS;
/// Chunks placed per level tick: a chunk near the crater writes a few thousand blocks.
const CHUNKS_PER_TICK: usize = 2;
/// Chunks kept waiting (for a start being worked out, or to be loaded again) at most.
const MOST_PENDING: usize = 8192;

/// Vanilla's `WorldgenRandom`: `BitRandomSource`'s draws over `next(bits)` of a `LegacyRandomSource` (Java's
/// `java.util.Random` LCG) or, for the decoration random, a `XoroshiroRandomSource`.
enum Bits {
    Legacy(u64),
    Xoroshiro(XoroshiroRandom),
}
struct Random(Bits);
impl Random {
    const MULTIPLIER: u64 = 0x5_DEEC_E66D;
    const MASK: u64 = (1 << 48) - 1;
    /// `new WorldgenRandom(new LegacyRandomSource(seed))`.
    fn legacy(seed: i64) -> Self {
        let mut random = Random(Bits::Legacy(0));
        random.set_seed(seed);
        random
    }
    /// `new WorldgenRandom(new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()))`: every use reseeds it.
    fn xoroshiro() -> Self {
        Random(Bits::Xoroshiro(XoroshiroRandom::new(0)))
    }
    fn set_seed(&mut self, seed: i64) {
        match &mut self.0 {
            // LegacyRandomSource.setSeed: Java's scramble.
            Bits::Legacy(state) => *state = (seed as u64 ^ Self::MULTIPLIER) & Self::MASK,
            // XoroshiroRandomSource.setSeed: RandomSupport.upgradeSeedTo128bit.
            Bits::Xoroshiro(source) => *source = XoroshiroRandom::new(seed),
        }
    }
    /// `WorldgenRandom.next(bits)`.
    fn next(&mut self, bits: u32) -> i32 {
        match &mut self.0 {
            Bits::Legacy(state) => {
                *state = state.wrapping_mul(Self::MULTIPLIER).wrapping_add(0xB) & Self::MASK;
                (*state >> (48 - bits)) as i32
            }
            Bits::Xoroshiro(source) => ((source.next_long() as u64) >> (64 - bits)) as i32,
        }
    }
    fn next_int(&mut self, bound: i32) -> i32 {
        if bound & (bound - 1) == 0 {
            return ((i64::from(bound) * i64::from(self.next(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let value = bits % bound;
            if bits.wrapping_sub(value).wrapping_add(bound - 1) >= 0 {
                return value;
            }
        }
    }
    fn next_long(&mut self) -> i64 {
        let high = i64::from(self.next(32)) << 32;
        high.wrapping_add(i64::from(self.next(32)))
    }
    fn next_boolean(&mut self) -> bool {
        self.next(1) != 0
    }
    /// `next(24) * 5.9604645E-8F` (2^-24).
    fn next_float(&mut self) -> f32 {
        self.next(24) as f32 * (1.0 / 16_777_216.0)
    }
    /// `((long) next(26) << 27) + next(27)` times `(double) 1.110223E-16F`, which is 2^-53.
    fn next_double(&mut self) -> f64 {
        let bits = (i64::from(self.next(26)) << 27) + i64::from(self.next(27));
        bits as f64 * (1.0 / 9_007_199_254_740_992.0)
    }
    fn set_large_feature_seed(&mut self, seed: i64, x: i32, z: i32) {
        self.set_seed(seed);
        let a = self.next_long();
        let b = self.next_long();
        self.set_seed(i64::from(x).wrapping_mul(a) ^ i64::from(z).wrapping_mul(b) ^ seed);
    }
    fn set_large_feature_with_salt(&mut self, seed: i64, x: i32, z: i32, salt: i32) {
        self.set_seed(
            i64::from(x)
                .wrapping_mul(341_873_128_712)
                .wrapping_add(i64::from(z).wrapping_mul(132_897_987_541))
                .wrapping_add(seed)
                .wrapping_add(i64::from(salt)),
        );
    }
    fn set_decoration_seed(&mut self, seed: i64, x: i32, z: i32) -> i64 {
        self.set_seed(seed);
        let a = self.next_long() | 1;
        let b = self.next_long() | 1;
        let mixed = i64::from(x).wrapping_mul(a).wrapping_add(i64::from(z).wrapping_mul(b)) ^ seed;
        self.set_seed(mixed);
        mixed
    }
    fn set_feature_seed(&mut self, seed: i64, index: i32, step: i32) {
        self.set_seed(seed.wrapping_add(i64::from(index)).wrapping_add(i64::from(step.wrapping_mul(10000))));
    }
}

/// RandomSpreadStructurePlacement.getPotentialStructureChunk: the one chunk of region `(rx, rz)` that may start a
/// meteorite.
fn start_chunk(seed: i64, rx: i32, rz: i32) -> [i32; 2] {
    let mut random = Random::legacy(0);
    random.set_large_feature_with_salt(seed, rx, rz, SALT);
    let x = random.next_int(SPACING - SEPARATION);
    let z = random.next_int(SPACING - SEPARATION);
    [rx * SPACING + x, rz * SPACING + z]
}

/// MeteoriteStructure.findGenerationPoint's own random: one nextBoolean, false for no meteorite.
fn generation_point(seed: i64, start: [i32; 2]) -> bool {
    let mut random = Random::legacy(0);
    random.set_large_feature_seed(seed, start[0], start[1]);
    random.next_boolean()
}

/// The regions whose start chunk is within reach of `chunk` (so the start's box covers it), with that chunk.
fn starts_near(seed: i64, chunk: [i32; 2]) -> Vec<((i32, i32), [i32; 2])> {
    let mut near = Vec::new();
    for rx in (chunk[0] - REACH).div_euclid(SPACING)..=(chunk[0] + REACH).div_euclid(SPACING) {
        for rz in (chunk[1] - REACH).div_euclid(SPACING)..=(chunk[1] + REACH).div_euclid(SPACING) {
            let start = start_chunk(seed, rx, rz);
            if (start[0] - chunk[0]).abs() <= REACH && (start[1] - chunk[1]).abs() <= REACH {
                near.push(((rx, rz), start));
            }
        }
    }
    near
}

/// CraterType, with its filler block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Crater {
    None,
    Normal,
    Lava,
    Obsidian,
    Water,
    Snow,
    Ice,
}
impl Crater {
    fn filler(self) -> &'static str {
        match self {
            Crater::None | Crater::Normal => "minecraft:air",
            Crater::Lava => "minecraft:lava",
            Crater::Obsidian => "minecraft:obsidian",
            Crater::Water => "minecraft:water",
            Crater::Snow => "minecraft:snow_block",
            Crater::Ice => "minecraft:ice",
        }
    }
}

/// FalloutMode (NONE is never chosen): which Fallout the placer gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FalloutMode {
    Default,
    Sand,
    Terracotta,
    IceSnow,
}

/// PlacedMeteoriteSettings, and the block FalloutCopy copies (from the biome at the meteor).
#[derive(Clone, Debug)]
struct Meteor {
    pos: [i32; 3],
    radius: f32,
    crater: Crater,
    fallout: FalloutMode,
    pure: bool,
    lake: bool,
    copied: &'static str,
}

/// The biome tags AE2 reads (Fabric's convention v1 names), each with its nested tags expanded.
struct Tags {
    has_meteorites: HashSet<String>,
    ocean: HashSet<String>,
    desert: HashSet<String>,
    beach: HashSet<String>,
    mesa: HashSet<String>,
    snowy: HashSet<String>,
    icy: HashSet<String>,
    plains: HashSet<String>,
    forest: HashSet<String>,
}
impl Tags {
    fn new(game: &Game<'_>) -> Self {
        let tag = |name: &str| game.tag_entries("worldgen/biome", name).into_iter().collect::<HashSet<String>>();
        Tags {
            has_meteorites: tag("ae2:has_meteorites"),
            ocean: tag("c:ocean"),
            desert: tag("c:desert"),
            beach: tag("c:beach"),
            mesa: tag("c:mesa"),
            snowy: tag("c:snowy"),
            icy: tag("c:icy"),
            plains: tag("c:plains"),
            forest: tag("c:forest"),
        }
    }
    /// FalloutMode.fromBiome: SAND (desert, beach), then TERRACOTTA (mesa), then ICE_SNOW (snowy, icy).
    fn fallout(&self, biome: &str) -> FalloutMode {
        if self.desert.contains(biome) || self.beach.contains(biome) {
            FalloutMode::Sand
        } else if self.mesa.contains(biome) {
            FalloutMode::Terracotta
        } else if self.snowy.contains(biome) || self.icy.contains(biome) {
            FalloutMode::IceSnow
        } else {
            FalloutMode::Default
        }
    }
    /// FalloutCopy's block, from `level.getBiome(meteorPos)`.
    fn copied(&self, biome: &str) -> &'static str {
        if self.mesa.contains(biome) {
            "minecraft:terracotta"
        } else if self.icy.contains(biome) {
            "minecraft:snow_block"
        } else if self.beach.contains(biome) || self.desert.contains(biome) {
            "minecraft:sand"
        } else if self.plains.contains(biome) || self.forest.contains(biome) {
            "minecraft:dirt"
        } else {
            "minecraft:cobblestone"
        }
    }
}

/// What the worker thread needs to work starts out.
struct Lookup {
    terrain: Arc<VanillaTerrain>,
    registry: &'static BlockRegistry,
    seed: i64,
    min_y: i32,
    /// The region's BiomeManager (`BiomeManager.obfuscateSeed(seed)`).
    zoom: BiomeZoom,
    tags: Tags,
    /// Each biome's climate (`Biome.ClimateSettings`), by name.
    climates: HashMap<String, BiomeTemperatureSettings>,
}

/// Guava's StatsAccumulator (Welford's running mean), as MeteoriteStructure.generatePieces uses it.
#[derive(Default)]
struct Stats {
    count: u64,
    mean: f64,
    sum_of_squares_of_deltas: f64,
    min: f64,
    max: f64,
}
impl Stats {
    fn add(&mut self, value: f64) {
        if self.count == 0 {
            self.count = 1;
            self.mean = value;
            self.min = value;
            self.max = value;
            if !value.is_finite() {
                self.sum_of_squares_of_deltas = f64::NAN;
            }
        } else {
            self.count += 1;
            if value.is_finite() && self.mean.is_finite() {
                let delta = value - self.mean;
                self.mean += delta / self.count as f64;
                self.sum_of_squares_of_deltas += delta * (value - self.mean);
            } else {
                // calculateNewMeanNonFinite
                self.mean = if self.mean.is_finite() {
                    value
                } else if value.is_finite() || self.mean == value {
                    self.mean
                } else {
                    f64::NAN
                };
                self.sum_of_squares_of_deltas = f64::NAN;
            }
            self.min = java_min(self.min, value);
            self.max = java_max(self.max, value);
        }
    }
    fn population_variance(&self) -> f64 {
        if self.sum_of_squares_of_deltas.is_nan() {
            f64::NAN
        } else if self.count == 1 {
            0.0
        } else {
            self.sum_of_squares_of_deltas.max(0.0) / self.count as f64
        }
    }
}
/// Java's Math.min/max: NaN wins.
fn java_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}
fn java_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

/// The worker thread's side: MeteoriteStructure's start for one start chunk.
struct Starts {
    lookup: Lookup,
    worker: GeneratorWorker,
    /// `getBaseHeight` by column: (OCEAN_FLOOR_WG, WORLD_SURFACE_WG). OCEAN_FLOOR (non-WG) reads the same noise
    /// column with the same predicate.
    heights: HashMap<(i32, i32), (i32, i32)>,
}
impl Starts {
    /// NoiseBasedChunkGenerator.getBaseHeight: one above the highest block of the noise column (aquifers, ore
    /// veins; no surface or features) that blocks motion (OCEAN_FLOOR_WG: water and lava don't) or isn't air
    /// (WORLD_SURFACE_WG), or the level's bottom.
    fn heights(&mut self, x: i32, z: i32) -> (i32, i32) {
        if let Some(h) = self.heights.get(&(x, z)) {
            return *h;
        }
        let (_, spans) = self.worker.surface_column(x, z);
        let (mut floor, mut surface) = (self.lookup.min_y, self.lookup.min_y);
        for (_, top, state) in spans {
            let fluid = matches!(
                self.lookup.registry.state(state).map(|s| s.name.as_str()),
                Some("minecraft:water" | "minecraft:lava")
            );
            surface = surface.max(top);
            if !fluid {
                floor = floor.max(top);
            }
        }
        if self.heights.len() > 65536 {
            self.heights.clear();
        }
        self.heights.insert((x, z), (floor, surface));
        (floor, surface)
    }
    /// BiomeSource.getNoiseBiome at a quart.
    fn noise_biome(&mut self, quart: [i32; 3]) -> String {
        self.worker.noise_biomes(&[quart]).pop().flatten().unwrap_or_default()
    }
    fn climate(&self, biome: &str) -> BiomeTemperatureSettings {
        self.lookup.climates.get(biome).copied().unwrap_or(BiomeTemperatureSettings {
            base_temperature: 0.8,
            modifier: TemperatureModifier::None,
            has_precipitation: true,
            downfall: 0.4,
        })
    }

    /// Structure.generate for the start chunk (whose findGenerationPoint boolean already passed).
    fn meteorite(&mut self, start: [i32; 2]) -> Option<Meteor> {
        let seed = self.lookup.seed;
        // onTopOfChunkCenter(OCEAN_FLOOR_WG) gives the stub; isValidBiome tests its noise biome against
        // #ae2:has_meteorites. The stub's position is used for nothing else.
        let (mx, mz) = (start[0] * 16 + 7, start[1] * 16 + 7);
        let stub_y = self.heights(mx, mz).0 - 1;
        let biome = self.noise_biome([mx >> 2, stub_y >> 2, mz >> 2]);
        if !self.lookup.tags.has_meteorites.contains(&biome) {
            return None;
        }
        // generatePieces, with the context's random: seeded as the boolean's was, and still unused.
        let mut random = Random::legacy(0);
        random.set_large_feature_seed(seed, start[0], start[1]);
        let center_x = start[0] * 16 + random.next_int(16);
        let center_z = start[1] * 16 + random.next_int(16);
        let radius = random.next_float() * 6.0 + 2.0;
        let y_offset = f64::from(radius).ceil() as i32 + 1;

        // getBiomesWithin(x, seaLevel, z, 0): one noise biome, at sea level.
        let spawn = self.noise_biome([center_x >> 2, SEA_LEVEL >> 2, center_z >> 2]);
        let ocean = self.lookup.tags.ocean.contains(&spawn);

        let mut stats = Stats::default();
        let scan = 1.0f32.max(radius * 2.0) as i32;
        for x in -scan..=scan {
            for z in -scan..=scan {
                let (floor, surface) = self.heights(center_x + x, center_z + z);
                stats.add(f64::from(if ocean { floor } else { surface }));
            }
        }
        let mut center_y = stats.mean as i32;
        if stats.population_variance() > 5.0 {
            center_y = (f64::from(center_y) - (stats.mean - stats.min) * 0.75) as i32;
        }
        center_y -= y_offset;
        center_y = (self.lookup.min_y + y_offset).max(center_y);
        let pos = [center_x, center_y, center_z];

        let lake = self.water_around(pos, radius);
        let crater = self.crater_type(pos, &spawn, &mut random);
        let pure = random.next_float() > 0.9;
        let fallout = self.lookup.tags.fallout(&spawn);
        // FalloutCopy: `level.getBiome(pos)`, through the region's BiomeManager zoom, at the meteor's own y.
        let quart = self.lookup.zoom.quart_at_block(BlockPos::new(pos[0], pos[1], pos[2]));
        let here = self.noise_biome(quart);
        let copied = self.lookup.tags.copied(&here);
        Some(Meteor { pos, radius, crater, fallout, pure, lake, copied })
    }

    /// locateWaterAroundTheCrater: a column below sea level in the ring where the crater's slope crosses it.
    fn water_around(&mut self, pos: [i32; 3], radius: f32) -> bool {
        let max_y = SEA_LEVEL - 1;
        for i in pos[0] - 32..=pos[0] + 32 {
            for k in pos[2] - 32..=pos[2] + 32 {
                let dx = f64::from(i - pos[0]);
                let dz = f64::from(k - pos[2]);
                let h = f64::from(pos[1] as f32 - radius + 1.0);
                let distance = dx * dx + dz * dz;
                if f64::from(max_y) > h + distance * 0.0175
                    && f64::from(max_y) < h + distance * 0.02
                    && self.heights(i, k).0 < SEA_LEVEL
                {
                    return true;
                }
            }
        }
        false
    }

    /// determineCraterType: 0 to 4 floats, by the spawn biome's climate.
    fn crater_type(&self, pos: [i32; 3], biome: &str, random: &mut Random) -> Crater {
        let climate = self.climate(biome);
        let temp = climate.base_temperature;
        if self.lookup.tags.ocean.contains(biome) {
            return Crater::None;
        }
        let special = random.next_float() > 0.5;
        if !special {
            return Crater::Normal;
        }
        // Biome.coldEnoughToSnow(pos): the height-adjusted temperature below 0.15.
        let cold = climate.height_adjusted(BlockPos::new(pos[0], pos[1], pos[2]), SEA_LEVEL) < 0.15;
        let precipitation = climate.has_precipitation;
        if temp >= 1.0 {
            let lava = random.next_float() > 0.5;
            if !precipitation {
                return if lava { Crater::Lava } else { Crater::Normal };
            }
            if !cold {
                let obsidian = random.next_float() > 0.75;
                let alternative = if obsidian { Crater::Obsidian } else { Crater::Lava };
                return if lava { alternative } else { Crater::Normal };
            }
            // Precipitation and cold: falls through, and the two tests below fail.
        }
        if temp < 1.0 && f64::from(temp) >= 0.2 {
            let water = random.next_float() > 0.25;
            let lava = random.next_float() > 0.8;
            if !precipitation {
                return if lava { Crater::Lava } else { Crater::Normal };
            }
            if !cold {
                let obsidian = random.next_float() > 0.75;
                let alternative = if obsidian { Crater::Obsidian } else { Crater::Lava };
                let lake = if water { Crater::Water } else { Crater::Normal };
                return if lava { alternative } else { lake };
            }
            let snow = random.next_float() > 0.75;
            let water = if water { Crater::Water } else { Crater::Normal };
            return if snow { Crater::Snow } else { water };
        }
        if f64::from(temp) < 0.2 {
            let snow = random.next_float() > 0.25;
            let lava = random.next_float() > 0.95;
            let ice = random.next_float() > 0.25;
            if !precipitation {
                return if lava { Crater::Lava } else { Crater::Normal };
            }
            if !cold {
                let alternative = if ice { Crater::Ice } else { Crater::Water };
                let lake = if snow { alternative } else { Crater::Normal };
                return if lava { Crater::Lava } else { lake };
            }
            let snow = if snow { Crater::Snow } else { Crater::Normal };
            return if lava { Crater::Lava } else { snow };
        }
        Crater::Normal
    }
}

/// The worker thread: answers each asked start, and gives its scratch back while idle.
fn work(lookup: Lookup, asks: Receiver<((i32, i32), [i32; 2])>, answers: Sender<((i32, i32), Option<Meteor>)>) {
    let worker: GeneratorWorker = ChunkGenerator::worker(&*lookup.terrain);
    let mut starts = Starts { lookup, worker, heights: HashMap::new() };
    loop {
        let (region, start) = match asks.try_recv() {
            Ok(ask) => ask,
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {
                starts.worker.release();
                starts.heights.clear();
                match asks.recv() {
                    Ok(ask) => ask,
                    Err(_) => return,
                }
            }
        };
        let found = starts.meteorite(start);
        if answers.send((region, found)).is_err() {
            return;
        }
    }
}

enum Start {
    Waiting,
    Done(Option<Meteor>),
}

/// One level's meteorites: the starts by region, and the new chunks still to place.
struct LevelMeteors {
    seed: i64,
    /// The generator these starts came from (another world, or a reopened one, starts over).
    generator: usize,
    min_y: i32,
    height: i32,
    starts: HashMap<(i32, i32), Start>,
    pending: Vec<[i32; 2]>,
    asks: Option<Sender<((i32, i32), [i32; 2])>>,
    answers: Option<Receiver<((i32, i32), Option<Meteor>)>>,
}
impl LevelMeteors {
    fn open(game: &Game<'_>, terrain: Arc<VanillaTerrain>, generator: usize) -> Self {
        let seed = game.level.seed;
        let (min_y, height) = (game.world.shape.min_y, game.world.shape.height);
        let tags = Tags::new(game);
        if tags.has_meteorites.is_empty() {
            log::warn!("ae2: #ae2:has_meteorites holds no biome; no meteorites will generate");
        }
        let biomes = match &game.world_biomes {
            Some(b) => b.clone(),
            None => game.res.biomes().clone(),
        };
        let climates = biomes
            .list
            .iter()
            .map(|b| {
                let settings = BiomeTemperatureSettings {
                    base_temperature: b.temperature,
                    modifier: if b.frozen { TemperatureModifier::Frozen } else { TemperatureModifier::None },
                    has_precipitation: b.precipitation,
                    downfall: b.downfall,
                };
                (b.name.clone(), settings)
            })
            .collect();
        let lookup = Lookup {
            terrain,
            registry: game.pack.assets.registry,
            seed,
            min_y,
            zoom: BiomeZoom::new(obfuscate_seed(seed)),
            tags,
            climates,
        };
        let (ask, asks) = mpsc::channel();
        let (answer, answers) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("ae2-meteorites".to_owned())
            .spawn(move || work(lookup, asks, answer));
        let (asks, answers) = match spawned {
            Ok(_) => (Some(ask), Some(answers)),
            Err(e) => {
                log::warn!("ae2: could not start the meteorite thread: {e}");
                (None, None)
            }
        };
        LevelMeteors {
            seed,
            generator,
            min_y,
            height,
            starts: HashMap::new(),
            pending: Vec::new(),
            asks,
            answers,
        }
    }
    /// The worker's answers so far.
    fn collect(&mut self) {
        let Some(answers) = &self.answers else { return };
        let mut lost = false;
        loop {
            match answers.try_recv() {
                Ok((region, found)) => {
                    if let Some(m) = &found {
                        log::info!(
                            "ae2: meteorite at {:?}: radius {}, crater {:?}, fallout {:?}, pure {}, lake {}",
                            m.pos, m.radius, m.crater, m.fallout, m.pure, m.lake
                        );
                    }
                    self.starts.insert(region, Start::Done(found));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    lost = true;
                    break;
                }
            }
        }
        if lost {
            log::warn!("ae2: the meteorite thread stopped");
            self.asks = None;
            self.answers = None;
            for start in self.starts.values_mut() {
                if matches!(start, Start::Waiting) {
                    *start = Start::Done(None);
                }
            }
        }
    }
}

#[derive(Default)]
struct Server {
    levels: HashMap<String, LevelMeteors>,
}

thread_local! {
    static SERVER: RefCell<Server> = RefCell::new(Server::default());
}

/// `native_chunk_load`: a chunk just generated asks for the meteorite starts that reach it, and waits for them.
pub fn chunk_load(game: &mut Game<'_>, dimension: &str, chunk: [i32; 2], new: bool) {
    if !new || !game.native_on(ID) || dimension != game.dim.id() {
        return;
    }
    let Some(terrain) = game.end.locator() else { return };
    // A superflat world's structure sets come from its settings, which AE2's meteorites aren't in by default.
    if terrain.is_flat() {
        return;
    }
    let seed = game.level.seed;
    let generator = Arc::as_ptr(&terrain) as usize;
    let fresh = SERVER.with_borrow(|s| {
        s.levels.get(dimension).is_none_or(|l| l.seed != seed || l.generator != generator)
    });
    if fresh {
        let level = LevelMeteors::open(game, terrain, generator);
        SERVER.with_borrow_mut(|s| {
            s.levels.insert(dimension.to_owned(), level);
        });
    }
    SERVER.with_borrow_mut(|s| {
        let Some(level) = s.levels.get_mut(dimension) else { return };
        let mut wanted = false;
        for (region, start) in starts_near(level.seed, chunk) {
            if !level.starts.contains_key(&region) {
                let asked = generation_point(level.seed, start)
                    && level.asks.as_ref().is_some_and(|asks| asks.send((region, start)).is_ok());
                level.starts.insert(region, if asked { Start::Waiting } else { Start::Done(None) });
            }
            wanted |= !matches!(level.starts.get(&region), Some(Start::Done(None)) | None);
        }
        if wanted && !level.pending.contains(&chunk) {
            level.pending.push(chunk);
        }
    });
}

/// `native_level_tick_post`: the level is in place; the chunks whose starts are known get their part of each.
pub fn level_tick(game: &mut Game<'_>, dimension: &str) {
    if !game.native_on(ID) || dimension != game.dim.id() {
        return;
    }
    let ready = SERVER.with_borrow_mut(|s| {
        let Some(level) = s.levels.get_mut(dimension) else { return None };
        level.collect();
        let mut ready = Vec::new();
        let mut keep = Vec::new();
        for chunk in std::mem::take(&mut level.pending) {
            if ready.len() >= CHUNKS_PER_TICK {
                keep.push(chunk);
                continue;
            }
            let near = starts_near(level.seed, chunk);
            if near.iter().any(|(r, _)| matches!(level.starts.get(r), Some(Start::Waiting))) {
                keep.push(chunk);
                continue;
            }
            // Its blocks are read and written: only while it's loaded.
            if !game.world.loaded(chunk[0] * 16, chunk[1] * 16) {
                keep.push(chunk);
                continue;
            }
            let meteors: Vec<Meteor> = near
                .iter()
                .filter_map(|(r, _)| match level.starts.get(r) {
                    Some(Start::Done(Some(m))) => Some(m.clone()),
                    _ => None,
                })
                .collect();
            ready.push((chunk, meteors));
        }
        if keep.len() > MOST_PENDING {
            keep.drain(..keep.len() - MOST_PENDING);
        }
        level.pending = keep;
        Some((level.seed, level.min_y, level.height, ready))
    });
    let Some((seed, min_y, height, ready)) = ready else { return };
    if ready.iter().all(|(_, m)| m.is_empty()) {
        return;
    }
    let Some(blocks) = Blocks::new(game.pack.assets.registry) else {
        log::warn!("ae2: a block meteorites place is missing; none placed");
        return;
    };
    for (chunk, meteors) in ready {
        if meteors.is_empty() {
            continue;
        }
        // ChunkGenerator.applyBiomeDecoration: one random for the chunk, its decoration seed from the chunk's
        // corner, reseeded for the structure's step and index before its starts are placed.
        let mut random = Random::xoroshiro();
        let decoration = random.set_decoration_seed(seed, chunk[0] * 16, chunk[1] * 16);
        random.set_feature_seed(decoration, STEP_INDEX, STEP);
        // StructureStart.placeInChunk's box: the chunk's writable area.
        let bounds = [chunk[0] * 16, min_y + 1, chunk[1] * 16, chunk[0] * 16 + 15, min_y + height - 1, chunk[1] * 16 + 15];
        for meteor in &meteors {
            // The piece's box (one layer at the meteor's y) must meet the area.
            if meteor.pos[1] < bounds[1] || meteor.pos[1] > bounds[4] {
                continue;
            }
            let Some(copied) = game.native_default_state(meteor.copied) else { continue };
            let size = f64::from(meteor.radius);
            let real_crater = size * 2.0 + 5.0;
            let mut placer = Placer {
                game: &mut *game,
                dimension,
                random: &mut random,
                blocks: &blocks,
                m: meteor,
                copied,
                bounds,
                bottom: min_y,
                top: min_y + height,
                size,
                squared: size * size,
                crater: real_crater * real_crater,
            };
            placer.place();
        }
    }
}

/// For a debug command (`/ae2debug meteor`): the starts of the regions around `at`, as worked out so far.
#[allow(dead_code)]
pub fn report(game: &Game<'_>, dimension: &str, at: [i32; 3]) -> Vec<String> {
    let seed = game.level.seed;
    let (rx, rz) = ((at[0] >> 4).div_euclid(SPACING), (at[2] >> 4).div_euclid(SPACING));
    let mut lines = Vec::new();
    SERVER.with_borrow(|s| {
        let level = s.levels.get(dimension);
        for dx in -1..=1 {
            for dz in -1..=1 {
                let region = (rx + dx, rz + dz);
                let start = start_chunk(seed, region.0, region.1);
                let state = match level.and_then(|l| l.starts.get(&region)) {
                    None => "no chunk in reach generated yet".to_owned(),
                    Some(Start::Waiting) => "being worked out".to_owned(),
                    Some(Start::Done(None)) => "none".to_owned(),
                    Some(Start::Done(Some(m))) => format!(
                        "meteorite at {:?}, radius {:.2}, crater {:?}, fallout {:?}, pure {}, lake {}",
                        m.pos, m.radius, m.crater, m.fallout, m.pure, m.lake
                    ),
                };
                lines.push(format!(
                    "region {region:?}: start chunk {start:?} (blocks {} {}), boolean {}: {state}",
                    start[0] * 16,
                    start[1] * 16,
                    generation_point(seed, start),
                ));
            }
        }
    });
    lines
}

/// The block states the placer writes (default states; the buds facing up).
struct Blocks {
    air: BlockStateId,
    void_air: BlockStateId,
    water: BlockStateId,
    lava: BlockStateId,
    obsidian: BlockStateId,
    snow_block: BlockStateId,
    ice: BlockStateId,
    stone: BlockStateId,
    cobblestone: BlockStateId,
    dirt: BlockStateId,
    gravel: BlockStateId,
    grass_block: BlockStateId,
    glass: BlockStateId,
    snow: BlockStateId,
    sky_stone: BlockStateId,
    mysterious_cube: BlockStateId,
    /// getQuartzBudList without spawnFlawlessOnly (its default).
    quartz: [BlockStateId; 5],
    buds: [BlockStateId; 3],
}
impl Blocks {
    fn new(registry: &BlockRegistry) -> Option<Self> {
        let s = |name: &str| registry.default_state(name);
        // CertusQuartzClusterBlock's default state, `setValue(FACING, UP)`.
        let up = |name: &str| {
            let id = registry.default_state(name)?;
            let mut state = registry.state(id)?.clone();
            if state.properties.contains_key("facing") {
                state.properties.insert("facing".to_owned(), "up".to_owned());
            }
            registry.id(&state)
        };
        Some(Blocks {
            air: s("minecraft:air")?,
            void_air: s("minecraft:void_air")?,
            water: s("minecraft:water")?,
            lava: s("minecraft:lava")?,
            obsidian: s("minecraft:obsidian")?,
            snow_block: s("minecraft:snow_block")?,
            ice: s("minecraft:ice")?,
            stone: s("minecraft:stone")?,
            cobblestone: s("minecraft:cobblestone")?,
            dirt: s("minecraft:dirt")?,
            gravel: s("minecraft:gravel")?,
            grass_block: s("minecraft:grass_block")?,
            glass: s("minecraft:glass")?,
            snow: s("minecraft:snow")?,
            sky_stone: s("ae2:sky_stone_block")?,
            mysterious_cube: s("ae2:mysterious_cube")?,
            quartz: [
                s("ae2:quartz_block")?,
                s("ae2:damaged_budding_quartz")?,
                s("ae2:chipped_budding_quartz")?,
                s("ae2:flawed_budding_quartz")?,
                s("ae2:flawless_budding_quartz")?,
            ],
            buds: [up("ae2:small_quartz_bud")?, up("ae2:medium_quartz_bud")?, up("ae2:large_quartz_bud")?],
        })
    }
    fn filler(&self, crater: Crater) -> BlockStateId {
        match crater {
            Crater::None | Crater::Normal => self.air,
            Crater::Lava => self.lava,
            Crater::Obsidian => self.obsidian,
            Crater::Water => self.water,
            Crater::Snow => self.snow_block,
            Crater::Ice => self.ice,
        }
    }
}

/// MeteoritePlacer for one chunk: `bounds` is the chunk's writable area, and the min/max helpers clamp to it.
struct Placer<'p, 'g> {
    game: &'p mut Game<'g>,
    dimension: &'p str,
    random: &'p mut Random,
    blocks: &'p Blocks,
    m: &'p Meteor,
    copied: BlockStateId,
    /// min x, min y, min z, max x, max y, max z.
    bounds: [i32; 6],
    /// The level's build range (`top` exclusive): reads outside it are void air, writes do nothing.
    bottom: i32,
    top: i32,
    size: f64,
    squared: f64,
    crater: f64,
}
impl Placer<'_, '_> {
    fn state(&self, p: [i32; 3]) -> BlockStateId {
        if p[1] < self.bottom || p[1] >= self.top {
            self.blocks.void_air
        } else {
            self.game.world.block(p[0], p[1], p[2])
        }
    }
    fn is(&self, state: BlockStateId, block: &str) -> bool {
        &*self.game.res.facts().get(state).name == block
    }
    fn is_air(&self, state: BlockStateId) -> bool {
        matches!(&*self.game.res.facts().get(state).name, "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air")
    }
    /// BlockState.canBeReplaced() (the `replaceable` property).
    fn replaceable(&self, state: BlockStateId) -> bool {
        self.game.res.facts().get(state).replaceable
    }
    /// BlockState.isSolid() (legacy solidity).
    fn solid(&self, state: BlockStateId) -> bool {
        self.game.res.facts().get(state).legacy_solid
    }
    /// `level.setBlock(pos, state, 3)` in the WorldGenRegion.
    fn set(&mut self, p: [i32; 3], state: BlockStateId) {
        if p[1] < self.bottom || p[1] >= self.top {
            return;
        }
        self.game.native_set_block(self.dimension, p, state, WORLDGEN_FLAGS);
    }
    /// MeteoriteBlockPutter.put: never over bedrock, nor the same state.
    fn put(&mut self, p: [i32; 3], state: BlockStateId) {
        let original = self.state(p);
        if self.is(original, "minecraft:bedrock") || original == state {
            return;
        }
        self.set(p, state);
    }
    fn clamp_x(&self, x: i32) -> i32 {
        if x < self.bounds[0] {
            self.bounds[0]
        } else if x > self.bounds[3] {
            self.bounds[3]
        } else {
            x
        }
    }
    fn clamp_z(&self, z: i32) -> i32 {
        if z < self.bounds[2] {
            self.bounds[2]
        } else if z > self.bounds[5] {
            self.bounds[5]
        } else {
            z
        }
    }
    /// Fallout.adjustCrater: 2 for FalloutSand and FalloutSnow.
    fn adjust_crater(&self) -> i32 {
        match self.m.fallout {
            FalloutMode::Sand | FalloutMode::IceSnow => 2,
            _ => 0,
        }
    }

    fn place(&mut self) {
        let place_crater = self.m.crater != Crater::None;
        if place_crater {
            self.place_crater();
        }
        self.place_meteorite();
        if place_crater {
            self.decay();
        }
        if self.m.lake {
            self.place_crater_lake();
        }
    }

    /// placeCrater: everything above the crater's paraboloid (up to y 255) cleared, the crater type's filler in
    /// solid blocks below the meteor. Its ItemEntity sweep finds nothing in a WorldGenRegion.
    fn place_crater(&mut self) {
        let [x, y, z] = self.m.pos;
        let filler = self.blocks.filler(self.m.crater);
        let adjust = f64::from(self.adjust_crater());
        for j in y - 5..=255 {
            for i in self.bounds[0]..=self.bounds[3] {
                for k in self.bounds[2]..=self.bounds[5] {
                    let dx = f64::from(i - x);
                    let dz = f64::from(k - z);
                    let h = f64::from(y) - self.size + 1.0 + adjust;
                    let distance = dx * dx + dz * dz;
                    if f64::from(j) > h + distance * 0.02 {
                        let current = self.state([i, j, k]);
                        if self.m.crater != Crater::Normal && j < y && self.solid(current) {
                            self.put([i, j, k], filler);
                        } else {
                            self.put([i, j, k], self.blocks.air);
                        }
                    }
                }
            }
        }
    }

    /// placeMeteorite: the sky stone, then the mysterious cube at the centre when this chunk holds it
    /// (AEConfig's `spawnPressesInMeteorites`).
    fn place_meteorite(&mut self) {
        self.place_sky_stone();
        let [x, y, z] = self.m.pos;
        let b = self.bounds;
        if super::port_ae2_me::spawn_presses_in_meteorites() && x >= b[0] && x <= b[3] && y >= b[1] && y <= b[4] && z >= b[2] && z <= b[5] {
            self.put(self.m.pos, self.blocks.mysterious_cube);
        }
    }

    /// placeMeteoriteSkyStone: an ellipsoid of sky stone around a hollow 3x3x3 core whose bottom layer is quartz
    /// (budding, with a bud on top now and then).
    fn place_sky_stone(&mut self) {
        let [x, y, z] = self.m.pos;
        let (x0, x1) = (self.clamp_x(x - 8), self.clamp_x(x + 8));
        let (z0, z1) = (self.clamp_z(z - 8), self.clamp_z(z + 8));
        for i in x0..=x1 {
            for j in y - 8..y + 8 {
                for k in z0..=z1 {
                    let dx = i - x;
                    let dy = j - y;
                    let dz = k - z;
                    if f64::from(dx * dx) * 0.7
                        + f64::from(dy * dy) * (if j > y { 1.4 } else { 0.8 })
                        + f64::from(dz * dz) * 0.7
                        < self.squared
                    {
                        if dx.abs() <= 1 && dy.abs() <= 1 && dz.abs() <= 1 {
                            if dy == -1 {
                                // getQuartzBudList: only the flawless block with AEConfig's `spawnFlawlessOnly`.
                                let index = if super::port_ae2_me::spawn_flawless_only() { 4 + self.random.next_int(1) as usize } else { self.random.next_int(self.blocks.quartz.len() as i32) as usize };
                                self.put([i, j, k], self.blocks.quartz[index]);
                                if index != 0
                                    && (dx != 0 || dz != 0)
                                    && f64::from(self.random.next_float()) <= 0.7
                                {
                                    let bud = self.blocks.buds[self.random.next_int(3) as usize];
                                    self.put([i, j + 1, k], bud);
                                }
                            }
                        } else {
                            self.put([i, j, k], self.blocks.sky_stone);
                        }
                    }
                }
            }
        }
    }

    /// decay: replaceable blocks take the block above them; fallout lands on the crater's floor; exposed blocks
    /// near the meteor get inset fallout.
    fn decay(&mut self) {
        let [x, y, z] = self.m.pos;
        let mut random_shit = 0.0f64;
        let (x0, x1) = (self.clamp_x(x - 30), self.clamp_x(x + 30));
        let (z0, z1) = (self.clamp_z(z - 30), self.clamp_z(z + 30));
        let filler = self.m.crater.filler();
        for i in x0..=x1 {
            for k in z0..=z1 {
                for j in y - 9..y + 30 {
                    let state = self.state([i, j, k]);
                    if self.m.pure && self.is(state, filler) {
                        continue;
                    }
                    if self.replaceable(state) {
                        let up = self.state([i, j + 1, k]);
                        if !self.is_air(up) {
                            // A direct setBlock: no bedrock or same-state check.
                            self.set([i, j, k], up);
                        } else if random_shit < 100.0 * self.crater {
                            let dx = f64::from(i - x);
                            let dy = f64::from(j - y);
                            let dz = f64::from(k - z);
                            let distance = dx * dx + dy * dy + dz * dz;
                            let below = self.state([i, j - 1, k]);
                            if !self.replaceable(below) {
                                let extra_range = self.random.next_double() * 0.6;
                                let height = self.crater * (extra_range + 0.2) - (distance - self.crater * 1.7).abs();
                                if !self.is_air(below) && height > 0.0 && self.random.next_double() > 0.6 {
                                    random_shit += 1.0;
                                    self.random_fall([i, j, k]);
                                }
                            }
                        }
                    } else {
                        let up = self.state([i, j + 1, k]);
                        if self.is_air(up) && self.random.next_double() > 0.4 {
                            let dx = f64::from(i - x);
                            let dy = f64::from(j - y);
                            let dz = f64::from(k - z);
                            let distance = dx * dx + dy * dy + dz * dz;
                            if (dx.abs() > 1.0 || dy.abs() > 1.0 || dz.abs() > 1.0) && distance < self.crater * 1.6 {
                                self.random_inset([i, j, k]);
                            }
                        }
                    }
                }
            }
        }
    }

    /// placeCraterLake: water in the crater's plain air up to sea level.
    fn place_crater_lake(&mut self) {
        let [x, y, z] = self.m.pos;
        let max_y = SEA_LEVEL - 1;
        let adjust = f64::from(self.adjust_crater());
        for j in y - 5..=max_y {
            for i in self.bounds[0]..=self.bounds[3] {
                for k in self.bounds[2]..=self.bounds[5] {
                    let dx = f64::from(i - x);
                    let dz = f64::from(k - z);
                    let h = f64::from(y) - self.size + 1.0 + adjust;
                    let distance = dx * dx + dz * dz;
                    if f64::from(j) > h + distance * 0.02 {
                        let current = self.state([i, j, k]);
                        if self.is(current, "minecraft:air") {
                            self.put([i, j, k], self.blocks.water);
                        }
                    }
                }
            }
        }
    }

    /// Fallout.getRandomFall (FalloutCopy's for SAND, TERRACOTTA and ICE_SNOW).
    fn random_fall(&mut self, p: [i32; 3]) {
        let a = self.random.next_float();
        if self.m.fallout == FalloutMode::Default {
            let state = if a > 0.9 {
                self.blocks.stone
            } else if a > 0.8 {
                self.blocks.cobblestone
            } else if a > 0.7 {
                self.blocks.dirt
            } else {
                self.blocks.gravel
            };
            self.put(p, state);
        } else if a > 0.9 {
            self.put(p, self.copied);
        } else {
            self.other(p, a);
        }
    }
    /// Fallout.getRandomInset (FalloutCopy's for SAND, TERRACOTTA and ICE_SNOW).
    fn random_inset(&mut self, p: [i32; 3]) {
        let a = self.random.next_float();
        if self.m.fallout == FalloutMode::Default {
            let state = if a > 0.9 {
                self.blocks.cobblestone
            } else if a > 0.8 {
                self.blocks.stone
            } else if a > 0.7 {
                self.blocks.grass_block
            } else if a > 0.6 {
                self.blocks.sky_stone
            } else if a > 0.5 {
                self.blocks.gravel
            } else {
                self.blocks.air
            };
            self.put(p, state);
        } else if a > 0.9 {
            self.put(p, self.copied);
        } else if a > 0.8 {
            self.put(p, self.blocks.air);
        } else {
            self.other(p, a - 0.1);
        }
    }
    /// getOther: nothing for FalloutCopy, glass for FalloutSand, snow or ice for FalloutSnow.
    fn other(&mut self, p: [i32; 3], a: f32) {
        match self.m.fallout {
            FalloutMode::Sand => {
                if a > 0.66 {
                    self.put(p, self.blocks.glass);
                }
            }
            FalloutMode::IceSnow => {
                if a > 0.7 {
                    self.put(p, self.blocks.snow);
                } else if a > 0.5 {
                    self.put(p, self.blocks.ice);
                }
            }
            _ => {}
        }
    }
}

/// CompassService.getDirection: the chunk of the nearest meteorite (its mysterious cube) within `range` chunks of
/// `chunk`, the nearest ring out first, then the nearest in it. A cube mined out of a loaded chunk no longer counts.
pub fn nearest(game: &Game<'_>, dimension: &str, chunk: [i32; 2], range: i32) -> Option<[i32; 2]> {
    SERVER.with_borrow(|s| {
        let level = s.levels.get(dimension)?;
        let mut best: Option<(i32, i32, [i32; 2])> = None;
        for start in level.starts.values() {
            let Start::Done(Some(m)) = start else { continue };
            let c = [m.pos[0] >> 4, m.pos[2] >> 4];
            if game.world.chunks.contains_key(&(c[0], c[1])) && game.native_block(m.pos) != "ae2:mysterious_cube" {
                continue;
            }
            let ring = (c[0] - chunk[0]).abs().max((c[1] - chunk[1]).abs());
            if ring >= range {
                continue;
            }
            let dist = ((c[1] - chunk[1]) * 16).pow(2) + ((c[0] - chunk[0]) * 16).pow(2);
            if best.is_none_or(|b| (ring, dist) < (b.0, b.1)) {
                best = Some((ring, dist, c));
            }
        }
        best.map(|b| b.2)
    })
}
