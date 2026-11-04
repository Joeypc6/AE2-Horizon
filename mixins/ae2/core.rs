// Applied Energistics 2's storage core, ported by hand from appliedenergistics2-fabric-15.4.12.jar:
// the keys a network stores (appeng.api.stacks), the counter that indexes them with fuzzy search
// (KeyCounter, VariantCounter, FuzzySearch), partition lists (appeng.util.prioritylist), amount
// formatting (AEKeyType.formatAmount, ReadableNumberConverter) and the basic storage cell
// (appeng.me.cells.BasicCellInventory, appeng.items.storage). None of it needs the game: a key is
// an id plus its components in vanilla's item syntax, and the game side turns stacks into keys and
// a cell's saved contents into components. No `//!` comments: module payloads can't hold inner
// attributes.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

/// `Actionable`: whether an insert or extract happens or is only asked about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Actionable {
    Modulate,
    Simulate,
}

/// `IncludeExclude`: whether a partition list lets only its keys in, or keeps them out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IncludeExclude {
    Whitelist,
    Blacklist,
}

/// `FuzzyMode`: how much durability may differ between keys that fuzzy-match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum FuzzyMode {
    #[default]
    IgnoreAll,
    Percent99,
    Percent75,
    Percent50,
    Percent25,
}

impl FuzzyMode {
    pub const VALUES: [FuzzyMode; 5] =
        [Self::IgnoreAll, Self::Percent99, Self::Percent75, Self::Percent50, Self::Percent25];

    pub fn percentage(self) -> f32 {
        match self {
            Self::IgnoreAll => -1.0,
            Self::Percent99 => 0.0,
            Self::Percent75 => 25.0,
            Self::Percent50 => 50.0,
            Self::Percent25 => 75.0,
        }
    }

    pub fn break_point(self) -> f32 {
        self.percentage() / 100.0
    }

    /// `calculateBreakPoint`: the damage below which an item counts as the lighter-worn group.
    pub fn calculate_break_point(self, max_damage: i32) -> i32 {
        (self.percentage() * max_damage as f32 / 100.0) as i32
    }

    /// Java's `name()`, which cells save under `FuzzyMode`.
    pub fn java_name(self) -> &'static str {
        match self {
            Self::IgnoreAll => "IGNORE_ALL",
            Self::Percent99 => "PERCENT_99",
            Self::Percent75 => "PERCENT_75",
            Self::Percent50 => "PERCENT_50",
            Self::Percent25 => "PERCENT_25",
        }
    }

    /// `BasicStorageCell.getFuzzyMode`: a missing or unknown name is `IGNORE_ALL`.
    pub fn from_saved(name: &str) -> Self {
        Self::VALUES.into_iter().find(|m| m.java_name() == name).unwrap_or_default()
    }
}

/// `AmountFormat`: the full amount for tooltips, or a short one that fits a slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmountFormat {
    Full,
    Slot,
    SlotLargeFont,
}

/// `AEKeyType`: items or fluids. AE2's two built-in key spaces (`ae2:i` and `ae2:f`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyType {
    Items,
    Fluids,
}

impl KeyType {
    /// The key type's registry id, written under `#c` in a generic key's tag.
    pub fn id(self) -> &'static str {
        match self {
            Self::Items => "ae2:i",
            Self::Fluids => "ae2:f",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "ae2:i" => Some(Self::Items),
            "ae2:f" => Some(Self::Fluids),
            _ => None,
        }
    }

    /// Fluids count in mB (1000 a bucket, as Horizon's tanks do); items one by one.
    pub fn amount_per_unit(self) -> i64 {
        match self {
            Self::Items => 1,
            Self::Fluids => 1_000,
        }
    }

    pub fn amount_per_operation(self) -> i64 {
        match self {
            Self::Items => 1,
            Self::Fluids => 125,
        }
    }

    /// How many of the key one byte of a storage cell holds.
    pub fn amount_per_byte(self) -> i64 {
        match self {
            Self::Items => 8,
            Self::Fluids => 8_000,
        }
    }

    pub fn unit_symbol(self) -> Option<&'static str> {
        match self {
            Self::Items => None,
            Self::Fluids => Some("B"),
        }
    }

    pub fn supports_fuzzy_range_search(self) -> bool {
        self == Self::Items
    }

    /// `AEKeyType.formatAmount`.
    pub fn format_amount(self, amount: i64, format: AmountFormat) -> String {
        match format {
            AmountFormat::Full => self.format_full_amount(amount),
            AmountFormat::Slot => self.format_short_amount(amount, 4),
            AmountFormat::SlotLargeFont => self.format_short_amount(amount, 3),
        }
    }

    fn format_full_amount(self, amount: i64) -> String {
        let mut result = if self.amount_per_unit() > 1 {
            group_decimal(amount as f64 / self.amount_per_unit() as f64)
        } else {
            group_long(amount)
        };
        if let Some(unit) = self.unit_symbol() {
            result.push(' ');
            result.push_str(unit);
        }
        result
    }

    fn format_short_amount(self, amount: i64, max_width: usize) -> String {
        if self.amount_per_unit() > 1 {
            readable_number::format_f64(amount as f64 / self.amount_per_unit() as f64, max_width)
        } else {
            readable_number::format_i64(amount, max_width)
        }
    }
}

/// `NumberFormat.getNumberInstance()` for a long, in the en-US form: `1,234,567`.
fn group_long(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `NumberFormat.getNumberInstance()` for a double: grouped, at most three fraction digits.
fn group_decimal(x: f64) -> String {
    let fixed = format!("{:.3}", x);
    let (int, frac) = fixed.split_once('.').unwrap_or((fixed.as_str(), ""));
    let frac = frac.trim_end_matches('0');
    let mut out = group_long(int.parse::<i64>().unwrap_or(0));
    if int.starts_with('-') && !out.starts_with('-') {
        out.insert(0, '-');
    }
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    out
}

/// `ReadableNumberConverter`: amounts shortened to fit a slot, `1.5K`, `12M`, `~3`.
pub mod readable_number {
    const POSTFIXES: [char; 6] = ['K', 'M', 'G', 'T', 'P', 'E'];

    /// `format(long, width)`. Panics on a negative number, as the Java checks.
    pub fn format_i64(number: i64, width: usize) -> String {
        assert!(number >= 0, "Non-negative numbers cannot be formatted by this method");
        let number_string = number.to_string();
        let mut number_size = number_string.len();
        if number_size <= width {
            return number_string;
        }
        let mut base = number;
        let mut last = base as f64 * 1000.0;
        let mut exponent: usize = 0;
        let mut post_fix = String::new();
        while number_size > width {
            last = base as f64;
            base /= 1000;
            exponent += 1;
            number_size = base.to_string().len() + 1;
            post_fix = POSTFIXES[exponent - 1].to_string();
        }
        let with_precision = decimal_down(last / 1000.0, 1) + &post_fix;
        let without_precision = format!("{base}{post_fix}");
        if with_precision.len() <= width { with_precision } else { without_precision }
    }

    /// `format(double, width)`.
    pub fn format_f64(number: f64, width: usize) -> String {
        assert!(number >= 0.0, "Non-negative numbers cannot be formatted by this method");
        let integer_digits = (number.log10() + 1.0).max(0.0) as i64;
        let fractional_digits = width as i64 - integer_digits - 1;
        let min_fractional = 10f64.powi(-fractional_digits as i32);
        let fractional = number - number.floor();
        if fractional >= 1.0e-9 && integer_digits <= width as i64 - 1 {
            if fractional + 1.0e-9 < min_fractional && integer_digits - 1 <= width as i64 {
                return format!("~{}", format_i64(number as i64, width - 1));
            }
            return decimal_down(number, fractional_digits.max(0) as usize);
        }
        format_i64(number as i64, width)
    }

    /// `new DecimalFormat(".#")` with `RoundingMode.DOWN`: no integer digit when it is 0
    /// (`.5`), trailing zeros dropped, no separator without a fraction. It works from the
    /// shortest decimal form of the double, as Java's DecimalFormat does.
    pub(super) fn decimal_down(x: f64, max_fraction: usize) -> String {
        let shortest = format!("{x}");
        let (int, frac) = shortest.split_once('.').unwrap_or((shortest.as_str(), ""));
        let frac = frac[..frac.len().min(max_fraction)].trim_end_matches('0');
        let int = if int == "0" && !frac.is_empty() { "" } else { int };
        if frac.is_empty() { int.to_string() } else { format!("{int}.{frac}") }
    }
}

/// `AEKey`: what a network stores, without an amount. An item key is the item's id and its
/// components beyond the item's own, in vanilla's item syntax (`""` for none); its damage and
/// maximum damage drive fuzzy search. A fluid key is the fluid's id and its components.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AeKey {
    kind: KeyType,
    id: Arc<str>,
    components: Arc<str>,
    damage: i32,
    max_damage: i32,
}

impl AeKey {
    /// `AEItemKey.of`: an item with its components, its current damage and its maximum
    /// damage (0 for an item without durability).
    pub fn item(id: &str, components: &str, damage: i32, max_damage: i32) -> Self {
        Self { kind: KeyType::Items, id: id.into(), components: components.into(), damage, max_damage }
    }

    /// `AEFluidKey.of`.
    pub fn fluid(id: &str, components: &str) -> Self {
        Self { kind: KeyType::Fluids, id: id.into(), components: components.into(), damage: 0, max_damage: 0 }
    }

    pub fn key_type(&self) -> KeyType {
        self.kind
    }

    /// `getId`: `minecraft:diamond`, `minecraft:water`.
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn components(&self) -> &str {
        &self.components
    }

    /// `getModId`: the id's namespace.
    pub fn mod_id(&self) -> &str {
        self.id.split_once(':').map_or("minecraft", |(ns, _)| ns)
    }

    /// `getPrimaryKey`: the item or fluid itself, without components. Fuzzy search and the
    /// counter's sub-indices group by it.
    pub fn primary_key(&self) -> PrimaryKey {
        PrimaryKey(self.kind, self.id.clone())
    }

    /// `dropSecondary`: the bare item or fluid, without components.
    pub fn drop_secondary(&self) -> Self {
        Self { components: "".into(), damage: 0, ..self.clone() }
    }

    pub fn fuzzy_search_value(&self) -> i32 {
        self.damage
    }

    pub fn fuzzy_search_max_value(&self) -> i32 {
        self.max_damage
    }

    /// `supportsFuzzyRangeSearch`: an item with durability.
    pub fn supports_fuzzy_range_search(&self) -> bool {
        self.kind.supports_fuzzy_range_search() && self.max_damage > 0
    }

    /// `AEKey.fuzzyEquals`.
    pub fn fuzzy_equals(&self, other: &AeKey, mode: FuzzyMode) -> bool {
        if self.kind != other.kind || self.id != other.id {
            return false;
        }
        if !self.supports_fuzzy_range_search() || mode == FuzzyMode::IgnoreAll {
            return true;
        }
        if mode == FuzzyMode::Percent99 {
            return (self.damage > 0) == (other.damage > 0);
        }
        let percent_a = self.damage as f32 / self.max_damage as f32;
        let percent_b = other.damage as f32 / other.max_damage as f32;
        (percent_a > mode.break_point()) == (percent_b > mode.break_point())
    }

    pub fn format_amount(&self, amount: i64, format: AmountFormat) -> String {
        self.kind.format_amount(amount, format)
    }
}

impl fmt::Debug for AeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.id)?;
        if !self.components.is_empty() {
            write!(f, "[{}]", self.components)?;
        }
        Ok(())
    }
}

/// The item or fluid a key is a variant of (`AEKey.getPrimaryKey`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrimaryKey(pub KeyType, pub Arc<str>);

/// `GenericStack`: a key and an amount.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GenericStack {
    pub what: AeKey,
    pub amount: i64,
}

/// A fuzzy sub-index sorts by damage, most worn first (`FuzzySearch.KeyComparator`), then by key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FuzzyOrder(Reverse<i32>, AeKey);

/// `VariantCounter`: the amounts of one item's or fluid's variants.
#[derive(Clone, Debug)]
enum Variants {
    /// `UnorderedVariantMap`, for keys without durability.
    Unordered(HashMap<AeKey, i64>),
    /// `FuzzyVariantMap`, sorted for range searches by damage.
    Fuzzy(BTreeMap<FuzzyOrder, i64>),
}

impl Variants {
    fn for_key(key: &AeKey) -> Self {
        if key.fuzzy_search_max_value() > 0 {
            Self::Fuzzy(BTreeMap::new())
        } else {
            Self::Unordered(HashMap::new())
        }
    }

    fn get(&self, key: &AeKey) -> i64 {
        match self {
            Self::Unordered(m) => m.get(key).copied().unwrap_or(0),
            Self::Fuzzy(m) => m.get(&FuzzyOrder(Reverse(key.damage), key.clone())).copied().unwrap_or(0),
        }
    }

    fn slot(&mut self, key: &AeKey) -> &mut i64 {
        match self {
            Self::Unordered(m) => m.entry(key.clone()).or_insert(0),
            Self::Fuzzy(m) => m.entry(FuzzyOrder(Reverse(key.damage), key.clone())).or_insert(0),
        }
    }

    fn remove(&mut self, key: &AeKey) {
        match self {
            Self::Unordered(m) => {
                m.remove(key);
            }
            Self::Fuzzy(m) => {
                m.remove(&FuzzyOrder(Reverse(key.damage), key.clone()));
            }
        }
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (&AeKey, i64)> + '_> {
        match self {
            Self::Unordered(m) => Box::new(m.iter().map(|(k, v)| (k, *v))),
            Self::Fuzzy(m) => Box::new(m.iter().map(|(k, v)| (&k.1, *v))),
        }
    }

    fn values_mut(&mut self) -> Box<dyn Iterator<Item = &mut i64> + '_> {
        match self {
            Self::Unordered(m) => Box::new(m.values_mut()),
            Self::Fuzzy(m) => Box::new(m.values_mut()),
        }
    }

    fn retain_nonzero(&mut self) {
        match self {
            Self::Unordered(m) => m.retain(|_, v| *v != 0),
            Self::Fuzzy(m) => m.retain(|_, v| *v != 0),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Unordered(m) => m.len(),
            Self::Fuzzy(m) => m.len(),
        }
    }

    /// `findFuzzy`: an unordered map has no damage to search by, so it answers everything. A
    /// fuzzy map answers the keys whose damage lies in the mode's range for `key`'s damage
    /// (`FuzzySearch.makeLowerBound`/`makeUpperBound`: above the upper bound, up to the lower).
    fn find_fuzzy(&self, key: &AeKey, mode: FuzzyMode) -> Vec<(AeKey, i64)> {
        match self {
            Self::Unordered(m) => m.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            Self::Fuzzy(m) => {
                let (lower, upper) = fuzzy_bounds(key, mode);
                m.iter()
                    .filter(|(k, _)| k.1.damage > upper && k.1.damage <= lower)
                    .map(|(k, v)| (k.1.clone(), *v))
                    .collect()
            }
        }
    }
}

/// `FuzzySearch.makeLowerBound` and `makeUpperBound`, as damage values.
fn fuzzy_bounds(key: &AeKey, mode: FuzzyMode) -> (i32, i32) {
    let max_value = key.fuzzy_search_max_value();
    assert!(max_value > 0, "Cannot use fuzzy search on keys that don't have a fuzzy max value: {key:?}");
    if mode == FuzzyMode::IgnoreAll {
        return (max_value, -1);
    }
    let breakpoint = mode.calculate_break_point(max_value);
    if key.fuzzy_search_value() <= breakpoint { (breakpoint, -1) } else { (max_value, breakpoint) }
}

/// `KeyCounter`: amounts by key, indexed by item or fluid so fuzzy searches only look at that
/// item's variants. Amounts may go negative (`removeAll` of a key it doesn't hold).
#[derive(Clone, Debug, Default)]
pub struct KeyCounter {
    lists: HashMap<PrimaryKey, Variants>,
}

impl KeyCounter {
    pub fn new() -> Self {
        Self::default()
    }

    fn sub_index(&mut self, key: &AeKey) -> &mut Variants {
        self.lists.entry(key.primary_key()).or_insert_with(|| Variants::for_key(key))
    }

    /// `findFuzzy`: the entries that fuzzy-match `key` under `mode`.
    pub fn find_fuzzy(&self, key: &AeKey, mode: FuzzyMode) -> Vec<(AeKey, i64)> {
        self.lists.get(&key.primary_key()).map_or_else(Vec::new, |v| v.find_fuzzy(key, mode))
    }

    pub fn add(&mut self, key: &AeKey, amount: i64) {
        *self.sub_index(key).slot(key) += amount;
    }

    pub fn remove(&mut self, key: &AeKey, amount: i64) {
        self.add(key, -amount);
    }

    pub fn set(&mut self, key: &AeKey, amount: i64) {
        *self.sub_index(key).slot(key) = amount;
    }

    pub fn get(&self, key: &AeKey) -> i64 {
        self.lists.get(&key.primary_key()).map_or(0, |v| v.get(key))
    }

    pub fn add_all(&mut self, other: &KeyCounter) {
        for (key, amount) in other.iter() {
            self.add(key, amount);
        }
    }

    pub fn remove_all(&mut self, other: &KeyCounter) {
        for (key, amount) in other.iter() {
            self.add(key, -amount);
        }
    }

    /// `removeZeros`: drops zero amounts, then the sub-indices left empty.
    pub fn remove_zeros(&mut self) {
        self.lists.retain(|_, v| {
            v.retain_nonzero();
            v.len() > 0
        });
    }

    pub fn remove_empty_submaps(&mut self) {
        self.lists.retain(|_, v| v.len() > 0);
    }

    /// `reset`: keeps the keys, at zero.
    pub fn reset(&mut self) {
        for v in self.lists.values_mut() {
            for amount in v.values_mut() {
                *amount = 0;
            }
        }
    }

    pub fn clear(&mut self) {
        self.lists.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.lists.values().all(|v| v.len() == 0)
    }

    pub fn size(&self) -> usize {
        self.lists.values().map(Variants::len).sum()
    }

    /// Every entry, zeros included.
    pub fn iter(&self) -> impl Iterator<Item = (&AeKey, i64)> + '_ {
        self.lists.values().flat_map(Variants::iter)
    }

    pub fn key_set(&self) -> HashSet<AeKey> {
        self.iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn first_key(&self) -> Option<&AeKey> {
        self.iter().next().map(|(k, _)| k)
    }

    /// `getFirstKey(Class)`: the first key of a key type.
    pub fn first_key_of(&self, kind: KeyType) -> Option<&AeKey> {
        self.iter().map(|(k, _)| k).find(|k| k.key_type() == kind)
    }

    /// Removes one key outright, not just its amount.
    pub fn remove_key(&mut self, key: &AeKey) {
        if let Some(v) = self.lists.get_mut(&key.primary_key()) {
            v.remove(key);
        }
    }
}

/// `IPartitionList`: a storage's filter. `Default` lets everything through; `Precise` lists
/// exact keys; `Fuzzy` lists keys matched within a fuzzy mode.
#[derive(Clone, Debug, Default)]
pub enum PartitionList {
    #[default]
    Default,
    Precise(KeyCounter),
    Fuzzy(KeyCounter, FuzzyMode),
}

impl PartitionList {
    /// `IPartitionList.builder()...build()`.
    pub fn build<'a>(keys: impl IntoIterator<Item = &'a AeKey>, fuzzy: Option<FuzzyMode>) -> Self {
        let mut counter = KeyCounter::new();
        for key in keys {
            counter.add(key, 1);
        }
        if counter.is_empty() {
            Self::Default
        } else if let Some(mode) = fuzzy {
            Self::Fuzzy(counter, mode)
        } else {
            Self::Precise(counter)
        }
    }

    pub fn is_listed(&self, key: &AeKey) -> bool {
        match self {
            Self::Default => false,
            Self::Precise(list) => list.get(key) > 0,
            Self::Fuzzy(list, mode) => !list.find_fuzzy(key, *mode).is_empty(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Default => true,
            Self::Precise(list) | Self::Fuzzy(list, _) => list.is_empty(),
        }
    }

    pub fn is_fuzzy(&self) -> bool {
        matches!(self, Self::Fuzzy(..))
    }

    /// `matchesFilter`: an empty list lets anything through.
    pub fn matches_filter(&self, key: &AeKey, mode: IncludeExclude) -> bool {
        if self.is_empty() {
            return true;
        }
        match mode {
            IncludeExclude::Whitelist => self.is_listed(key),
            IncludeExclude::Blacklist => !self.is_listed(key),
        }
    }
}

/// `CellState`: what a cell's light shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CellState {
    Absent,
    Empty,
    NotEmpty,
    TypesFull,
    Full,
}

impl CellState {
    /// `getStateColor`: RGB.
    pub fn state_color(self) -> u32 {
        match self {
            Self::Absent => 0x000000,
            Self::Empty => 0x00FF00,
            Self::NotEmpty => 0x00AAFF,
            Self::TypesFull => 0xFFAA00,
            Self::Full => 0xFF0000,
        }
    }
}

/// `StorageTier`: a cell component's size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StorageTier {
    pub index: u8,
    pub name_prefix: &'static str,
    pub bytes: i64,
    pub idle_drain: f64,
}

impl StorageTier {
    pub const SIZE_1K: Self = Self { index: 1, name_prefix: "1k", bytes: 1024, idle_drain: 0.5 };
    pub const SIZE_4K: Self = Self { index: 2, name_prefix: "4k", bytes: 4096, idle_drain: 1.0 };
    pub const SIZE_16K: Self = Self { index: 3, name_prefix: "16k", bytes: 16384, idle_drain: 1.5 };
    pub const SIZE_64K: Self = Self { index: 4, name_prefix: "64k", bytes: 65536, idle_drain: 2.0 };
    pub const SIZE_256K: Self = Self { index: 5, name_prefix: "256k", bytes: 262144, idle_drain: 2.5 };
    pub const ALL: [Self; 5] = [Self::SIZE_1K, Self::SIZE_4K, Self::SIZE_16K, Self::SIZE_64K, Self::SIZE_256K];

    /// The component item, `ae2:cell_component_1k`.
    pub fn component_id(self) -> String {
        format!("ae2:cell_component_{}", self.name_prefix)
    }
}

/// `BasicStorageCell`: what a storage cell item holds and costs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellType {
    pub key_type: KeyType,
    pub total_bytes: i64,
    pub bytes_per_type: i64,
    pub total_types: i32,
    pub idle_drain: f64,
    /// `storableInStorageCell`: whether this cell, full, may itself go into a cell.
    pub storable_in_storage_cell: bool,
}

impl CellType {
    /// The item cells (`ae2:item_storage_cell_1k`...): 8 bytes a type at 1k, 63 types.
    pub fn item_cell(tier: StorageTier) -> Self {
        Self::basic(tier, KeyType::Items, 63)
    }

    /// The fluid cells (`ae2:fluid_storage_cell_1k`...): 18 types.
    pub fn fluid_cell(tier: StorageTier) -> Self {
        Self::basic(tier, KeyType::Fluids, 18)
    }

    fn basic(tier: StorageTier, key_type: KeyType, total_types: i32) -> Self {
        Self {
            key_type,
            total_bytes: tier.bytes,
            bytes_per_type: tier.bytes / 128,
            total_types,
            idle_drain: tier.idle_drain,
            storable_in_storage_cell: false,
        }
    }

    /// `BasicStorageCell.getUpgrades`: item cells take four cards, fluid cells three.
    pub fn upgrade_slots(&self) -> usize {
        if self.key_type == KeyType::Items { 4 } else { 3 }
    }

    /// The cell's item id for a tier.
    pub fn item_id(&self, tier: StorageTier) -> String {
        let kind = if self.key_type == KeyType::Items { "item" } else { "fluid" };
        format!("ae2:{kind}_storage_cell_{}", tier.name_prefix)
    }
}

/// The upgrade cards installed in a cell that change how it stores.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellUpgrades {
    pub inverter: bool,
    pub fuzzy: bool,
    pub equal_distribution: bool,
    pub void: bool,
}

/// What a cell saves on its item: keys and amounts side by side (`keys` and `amts`), and the
/// total (`ic`). The game side writes them into the cell item's components.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CellContents {
    pub keys: Vec<AeKey>,
    pub amounts: Vec<i64>,
    pub item_count: i64,
}

/// `BasicCellInventory`: a storage cell's contents and the byte and type limits that bound
/// them. Changes stay in memory until `persist`, which answers what to save.
pub struct BasicCellInventory {
    cell: CellType,
    max_item_types: i64,
    stored: HashMap<AeKey, i64>,
    stored_item_types: i64,
    stored_item_count: i64,
    partition_list: PartitionList,
    partition_mode: IncludeExclude,
    max_items_per_type: i64,
    has_void_upgrade: bool,
    persisted: bool,
    /// `cellType.isBlackListed` and the nested-cell check of `innerInsert`: answers whether a
    /// key may go in. The game side sets it for keys that are full storage cells.
    pub accepts: Option<Box<dyn Fn(&AeKey) -> bool + Send + Sync>>,
}

impl BasicCellInventory {
    /// `BasicCellInventory(cellType, stack, container)`: the cell, its configured partition
    /// (`config`), fuzzy mode, cards, and what it saved last.
    pub fn new(
        cell: CellType,
        config: &[AeKey],
        fuzzy_mode: FuzzyMode,
        upgrades: CellUpgrades,
        saved: &CellContents,
    ) -> Self {
        let max_item_types = (cell.total_types as i64).clamp(1, 63);
        let partition_mode =
            if upgrades.inverter { IncludeExclude::Blacklist } else { IncludeExclude::Whitelist };
        let partition_list = PartitionList::build(config, upgrades.fuzzy.then_some(fuzzy_mode));
        let max_items_per_type = if upgrades.equal_distribution {
            let mut max_types = i32::MAX as i64;
            if !upgrades.fuzzy && partition_mode == IncludeExclude::Whitelist && !config.is_empty() {
                max_types = config.iter().collect::<HashSet<_>>().len() as i64;
            }
            max_types = max_types.min(max_item_types);
            let total_storage =
                (cell.total_bytes - cell.bytes_per_type * max_types) * cell.key_type.amount_per_byte();
            ((total_storage + max_types - 1) / max_types).max(0)
        } else {
            i64::MAX
        };
        let mut inv = Self {
            cell,
            max_item_types,
            stored: HashMap::new(),
            stored_item_types: saved.amounts.len() as i64,
            stored_item_count: saved.item_count,
            partition_list,
            partition_mode,
            max_items_per_type,
            has_void_upgrade: upgrades.void,
            persisted: true,
            accepts: None,
        };
        inv.load(saved);
        inv
    }

    /// `loadCellItems`: a mismatched or non-positive entry is dropped and the cell resaved.
    fn load(&mut self, saved: &CellContents) {
        let mut corrupted = saved.keys.len() != saved.amounts.len();
        for (key, &amount) in saved.keys.iter().zip(&saved.amounts) {
            if amount > 0 {
                self.stored.insert(key.clone(), amount);
            } else {
                corrupted = true;
            }
        }
        if corrupted {
            self.save_changes();
        }
    }

    /// `saveChanges`: recounts types and amounts; the cell is dirty until `persist`.
    fn save_changes(&mut self) {
        self.stored_item_types = self.stored.len() as i64;
        self.stored_item_count = self.stored.values().sum();
        self.persisted = false;
    }

    /// `persist`: what to save on the cell item, or `None` when nothing changed since.
    pub fn persist(&mut self) -> Option<CellContents> {
        if self.persisted {
            return None;
        }
        let mut out = CellContents::default();
        let mut entries: Vec<_> = self.stored.iter().filter(|(_, a)| **a > 0).collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (key, &amount) in entries {
            out.item_count += amount;
            out.keys.push(key.clone());
            out.amounts.push(amount);
        }
        self.stored_item_types = self.stored.len() as i64;
        self.stored_item_count = out.item_count;
        self.persisted = true;
        Some(out)
    }

    pub fn cell_type(&self) -> &CellType {
        &self.cell
    }

    pub fn partition_mode(&self) -> IncludeExclude {
        self.partition_mode
    }

    pub fn is_preformatted(&self) -> bool {
        !self.partition_list.is_empty()
    }

    pub fn is_fuzzy(&self) -> bool {
        self.partition_list.is_fuzzy()
    }

    /// Whether the cell's partition lists `key` (fuzzily, with a fuzzy card).
    pub fn is_listed(&self, key: &AeKey) -> bool {
        self.partition_list.is_listed(key)
    }

    pub fn idle_drain(&self) -> f64 {
        self.cell.idle_drain
    }

    /// `canFitInsideCell`: an empty cell fits in another; a full one only if its type allows.
    pub fn can_fit_inside_cell(&self) -> bool {
        self.cell.storable_in_storage_cell || self.stored.is_empty()
    }

    /// `getAvailableStacks`.
    pub fn available_stacks(&self, out: &mut KeyCounter) {
        for (key, &amount) in &self.stored {
            out.add(key, amount);
        }
    }

    pub fn amount_of(&self, key: &AeKey) -> i64 {
        self.stored.get(key).copied().unwrap_or(0)
    }

    pub fn total_bytes(&self) -> i64 {
        self.cell.total_bytes
    }

    pub fn bytes_per_type(&self) -> i64 {
        self.cell.bytes_per_type
    }

    pub fn total_item_types(&self) -> i64 {
        self.max_item_types
    }

    pub fn stored_item_count(&self) -> i64 {
        self.stored_item_count
    }

    pub fn stored_item_types(&self) -> i64 {
        self.stored_item_types
    }

    pub fn free_bytes(&self) -> i64 {
        self.total_bytes() - self.used_bytes()
    }

    /// `getUsedBytes`: each type's fixed cost, and the amounts rounded up to whole bytes.
    pub fn used_bytes(&self) -> i64 {
        let bytes_for_item_count =
            (self.stored_item_count + self.unused_item_count()) / self.cell.key_type.amount_per_byte();
        self.stored_item_types * self.bytes_per_type() + bytes_for_item_count
    }

    pub fn remaining_item_types(&self) -> i64 {
        let based_on_storage = self.free_bytes() / self.bytes_per_type();
        let based_on_total = self.total_item_types() - self.stored_item_types();
        based_on_storage.min(based_on_total)
    }

    pub fn remaining_item_count(&self) -> i64 {
        let remaining =
            self.free_bytes() * self.cell.key_type.amount_per_byte() + self.unused_item_count();
        remaining.max(0)
    }

    /// `getUnusedItemCount`: room left in the last, partly used byte.
    pub fn unused_item_count(&self) -> i64 {
        let per_byte = self.cell.key_type.amount_per_byte();
        let div = self.stored_item_count % per_byte;
        if div == 0 { 0 } else { per_byte - div }
    }

    pub fn can_hold_new_item(&self) -> bool {
        let bytes_free = self.free_bytes();
        (bytes_free > self.bytes_per_type()
            || (bytes_free == self.bytes_per_type() && self.unused_item_count() > 0))
            && self.remaining_item_types() > 0
    }

    pub fn status(&self) -> CellState {
        if self.stored_item_types() == 0 {
            CellState::Empty
        } else if self.can_hold_new_item() {
            CellState::NotEmpty
        } else if self.remaining_item_count() > 0 {
            CellState::TypesFull
        } else {
            CellState::Full
        }
    }

    /// `insert`: how much of `amount` goes in. A void card claims all of what the partition
    /// accepts, the rest being destroyed.
    pub fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        if amount == 0 || what.key_type() != self.cell.key_type {
            return 0;
        }
        if !self.partition_list.matches_filter(what, self.partition_mode) {
            return 0;
        }
        if self.accepts.as_ref().is_some_and(|accepts| !accepts(what)) {
            return 0;
        }
        let inserted = self.inner_insert(what, amount, mode);
        if !self.is_preformatted() && self.has_void_upgrade && !self.can_hold_new_item() {
            if self.stored.contains_key(what) { amount } else { inserted }
        } else if self.has_void_upgrade {
            amount
        } else {
            inserted
        }
    }

    fn inner_insert(&mut self, what: &AeKey, mut amount: i64, mode: Actionable) -> i64 {
        let current_amount = self.amount_of(what);
        let mut remaining_item_count = self.remaining_item_count();
        if current_amount <= 0 {
            if !self.can_hold_new_item() {
                return 0;
            }
            remaining_item_count -= self.bytes_per_type() * self.cell.key_type.amount_per_byte();
            if remaining_item_count <= 0 {
                return 0;
            }
        }
        remaining_item_count =
            remaining_item_count.min(self.max_items_per_type.saturating_sub(current_amount)).max(0);
        amount = amount.min(remaining_item_count);
        if mode == Actionable::Modulate {
            self.stored.insert(what.clone(), current_amount + amount);
            self.save_changes();
        }
        amount
    }

    /// `extract`: how much of `amount` comes out.
    pub fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        let current_amount = self.amount_of(what);
        if current_amount <= 0 {
            return 0;
        }
        if amount >= current_amount {
            if mode == Actionable::Modulate {
                self.stored.remove(what);
                self.save_changes();
            }
            current_amount
        } else {
            if mode == Actionable::Modulate {
                self.stored.insert(what.clone(), current_amount - amount);
                self.save_changes();
            }
            amount
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(damage: i32) -> AeKey {
        AeKey::item("minecraft:diamond_pickaxe", "", damage, 1561)
    }

    #[test]
    fn short_amounts_fit_their_slot() {
        assert_eq!(readable_number::format_i64(999, 4), "999");
        assert_eq!(readable_number::format_i64(9999, 4), "9999");
        assert_eq!(readable_number::format_i64(10_000, 4), "10K");
        assert_eq!(readable_number::format_i64(1_500, 3), "1K");
        assert_eq!(readable_number::format_i64(1_550_000, 4), "1.5M");
        assert_eq!(readable_number::format_i64(123_456_789, 4), "123M");
    }

    #[test]
    fn fluids_format_in_buckets() {
        assert_eq!(KeyType::Fluids.format_amount(81_000, AmountFormat::Full), "1 B");
        assert_eq!(KeyType::Fluids.format_amount(40_500, AmountFormat::Full), "0.5 B");
        assert_eq!(KeyType::Fluids.format_amount(40_500, AmountFormat::Slot), ".5");
        assert_eq!(KeyType::Items.format_amount(1_234_567, AmountFormat::Full), "1,234,567");
    }

    #[test]
    fn counter_adds_and_drops_zeros() {
        let dirt = AeKey::item("minecraft:dirt", "", 0, 0);
        let mut c = KeyCounter::new();
        c.add(&dirt, 5);
        c.remove(&dirt, 5);
        assert_eq!(c.size(), 1);
        c.remove_zeros();
        assert!(c.is_empty());
        let mut other = KeyCounter::new();
        other.add(&dirt, 3);
        c.remove_all(&other);
        assert_eq!(c.get(&dirt), -3);
    }

    #[test]
    fn fuzzy_search_splits_at_the_break_point() {
        let mut c = KeyCounter::new();
        for damage in [0, 100, 800, 1500] {
            c.add(&pick(damage), 1);
        }
        assert_eq!(c.find_fuzzy(&pick(0), FuzzyMode::IgnoreAll).len(), 4);
        // 50% of 1561 is 780: 0 and 100 are on the lighter side, 800 and 1500 on the other.
        let mut light: Vec<_> = c.find_fuzzy(&pick(5), FuzzyMode::Percent50).iter().map(|e| e.0.damage).collect();
        light.sort();
        assert_eq!(light, [0, 100]);
        let mut worn: Vec<_> = c.find_fuzzy(&pick(1000), FuzzyMode::Percent50).iter().map(|e| e.0.damage).collect();
        worn.sort();
        assert_eq!(worn, [800, 1500]);
        assert!(pick(0).fuzzy_equals(&pick(100), FuzzyMode::Percent50));
        assert!(!pick(0).fuzzy_equals(&pick(1000), FuzzyMode::Percent50));
    }

    #[test]
    fn a_1k_cell_holds_what_ae2_says() {
        let cell = CellType::item_cell(StorageTier::SIZE_1K);
        let mut inv = BasicCellInventory::new(cell, &[], FuzzyMode::IgnoreAll, CellUpgrades::default(), &CellContents::default());
        let cobble = AeKey::item("minecraft:cobblestone", "", 0, 0);
        // 1024 bytes, 8 for the type: 1016 bytes of 8 items.
        assert_eq!(inv.insert(&cobble, 100_000, Actionable::Simulate), 8128);
        assert_eq!(inv.insert(&cobble, 100, Actionable::Modulate), 100);
        assert_eq!(inv.used_bytes(), 8 + 13);
        assert_eq!(inv.status(), CellState::NotEmpty);
        assert_eq!(inv.extract(&cobble, 40, Actionable::Modulate), 40);
        let saved = inv.persist().unwrap();
        assert_eq!(saved.item_count, 60);
        assert!(inv.persist().is_none());
        let reloaded = BasicCellInventory::new(cell, &[], FuzzyMode::IgnoreAll, CellUpgrades::default(), &saved);
        assert_eq!(reloaded.amount_of(&cobble), 60);
    }

    #[test]
    fn partitions_inverters_and_void_cards() {
        let cell = CellType::item_cell(StorageTier::SIZE_4K);
        let dirt = AeKey::item("minecraft:dirt", "", 0, 0);
        let sand = AeKey::item("minecraft:sand", "", 0, 0);
        let empty = CellContents::default();
        let mut only_dirt = BasicCellInventory::new(cell, &[dirt.clone()], FuzzyMode::IgnoreAll, CellUpgrades::default(), &empty);
        assert_eq!(only_dirt.insert(&sand, 10, Actionable::Modulate), 0);
        assert_eq!(only_dirt.insert(&dirt, 10, Actionable::Modulate), 10);
        let inverted = CellUpgrades { inverter: true, ..Default::default() };
        let mut no_dirt = BasicCellInventory::new(cell, &[dirt.clone()], FuzzyMode::IgnoreAll, inverted, &empty);
        assert_eq!(no_dirt.insert(&dirt, 10, Actionable::Modulate), 0);
        assert_eq!(no_dirt.insert(&sand, 10, Actionable::Modulate), 10);
        let voiding = CellUpgrades { void: true, ..Default::default() };
        let mut void = BasicCellInventory::new(cell, &[dirt.clone()], FuzzyMode::IgnoreAll, voiding, &empty);
        assert_eq!(void.insert(&dirt, i64::MAX / 2, Actionable::Modulate), i64::MAX / 2);
        assert_eq!(void.status(), CellState::Full);
    }

    #[test]
    fn equal_distribution_splits_the_bytes() {
        let cell = CellType::item_cell(StorageTier::SIZE_1K);
        let a = AeKey::item("minecraft:dirt", "", 0, 0);
        let b = AeKey::item("minecraft:sand", "", 0, 0);
        let upgrades = CellUpgrades { equal_distribution: true, ..Default::default() };
        let mut inv = BasicCellInventory::new(cell, &[a.clone(), b.clone()], FuzzyMode::IgnoreAll, upgrades, &CellContents::default());
        // (1024 - 8 * 2) * 8 / 2 = 4032 each.
        assert_eq!(inv.insert(&a, 100_000, Actionable::Modulate), 4032);
        assert_eq!(inv.insert(&b, 100_000, Actionable::Modulate), 4032);
    }

    #[test]
    fn fluid_cells_count_in_millibuckets() {
        let cell = CellType::fluid_cell(StorageTier::SIZE_1K);
        let water = AeKey::fluid("minecraft:water", "");
        let mut inv = BasicCellInventory::new(cell, &[], FuzzyMode::IgnoreAll, CellUpgrades::default(), &CellContents::default());
        assert_eq!(inv.insert(&AeKey::item("minecraft:dirt", "", 0, 0), 1, Actionable::Modulate), 0);
        // (1024 - 8) bytes of 8 buckets each.
        assert_eq!(inv.insert(&water, i64::MAX / 4, Actionable::Modulate), 1016 * 8_000);
    }
}
