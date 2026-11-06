// Applied Energistics 2's crafting calculator, ported by hand from appliedenergistics2-fabric-15.4.12.jar:
// what a crafting request needs, found by trying patterns down a tree (appeng.crafting:
// CraftingCalculation, CraftingTreeNode, CraftingTreeProcess) against a simulated copy of the
// network's storage (appeng.crafting.inv), ending in a plan (CraftingPlan): what to take from
// storage, what to craft how many times, what is missing and how many bytes of crafting storage it
// needs. Patterns are plain data here (appeng.api.crafting.IPatternDetails); the game side decodes
// encoded pattern items into them. The calculation runs to the end in one call: AE2 spreads it over
// ticks on a thread (handlePausing), which a caller may do by running it off the game thread. No
// `//!` comments: module payloads can't hold inner attributes.

use super::port_ae2_core::{Actionable, AeKey, FuzzyMode, GenericStack, KeyCounter};
use std::fmt;
use std::sync::Arc;

/// One input slot of a pattern (`IPatternDetails.IInput`).
#[derive(Clone, Debug, PartialEq)]
pub struct PatternInput {
    /// What may go in, the encoded one first, each with the amount one craft takes.
    pub possible: Vec<GenericStack>,
    /// How many times the slot's amount one craft takes (`getMultiplier`).
    pub multiplier: i64,
    /// Whether the other `possible` keys may stand in for the first (a crafting pattern with
    /// substitutions on). Without it only the first is valid, as a processing pattern takes.
    pub substitute: bool,
    /// What an input leaves behind in the crafting grid (`getRemainingKey`): a milk bucket
    /// leaves a bucket.
    pub remainders: Vec<(AeKey, AeKey)>,
}

impl PatternInput {
    /// A slot that takes `amount` of one key.
    pub fn of(what: AeKey, amount: i64) -> Self {
        Self { possible: vec![GenericStack { what, amount }], multiplier: 1, substitute: false, remainders: vec![] }
    }

    /// `isValid`: the encoded key, or with substitutions any of the possible ones.
    pub fn is_valid(&self, key: &AeKey) -> bool {
        match self.possible.first() {
            Some(first) if first.what == *key => true,
            _ => self.substitute && self.possible.iter().any(|p| p.what == *key),
        }
    }

    pub fn remaining_key(&self, template: &AeKey) -> Option<&AeKey> {
        self.remainders.iter().find(|(input, _)| input == template).map(|(_, left)| left)
    }

    fn primary(&self) -> &GenericStack {
        &self.possible[0]
    }
}

/// An encoded pattern (`IPatternDetails`): its inputs and outputs per craft. Two patterns are
/// the same when their encoded items are (`AECraftingPattern.equals`).
#[derive(Clone, Debug)]
pub struct Pattern {
    /// The encoded pattern item (`getDefinition`).
    pub definition: AeKey,
    pub inputs: Vec<PatternInput>,
    pub outputs: Vec<GenericStack>,
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.definition == other.definition
    }
}

impl Pattern {
    pub fn primary_output(&self) -> &GenericStack {
        &self.outputs[0]
    }

    /// `CraftingTreeProcess.getOutputCount`.
    fn output_count(&self, what: &AeKey) -> i64 {
        self.outputs.iter().filter(|o| o.what == *what).map(|o| o.amount).sum()
    }
}

/// `CalculationStrategy`: on a shortfall, report what is missing, or craft as many as can be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CalculationStrategy {
    #[default]
    ReportMissingItems,
    CraftLess,
}

/// What the calculator asks the network's crafting service (`ICraftingService`).
pub trait CraftingService {
    /// `canEmitFor`: a level emitter crafts this by signal, without a pattern.
    fn can_emit_for(&self, key: &AeKey) -> bool;
    /// `getCraftingFor`: the patterns that output this key, the one to try first first.
    fn crafting_for(&self, key: &AeKey) -> Vec<Arc<Pattern>>;
    /// `getFuzzyCraftable`: a craftable variant of this key that `filter` accepts.
    fn fuzzy_craftable(&self, key: &AeKey, filter: &dyn Fn(&AeKey) -> bool) -> Option<AeKey>;
}

/// The network's patterns and level emitters (`NetworkCraftingProviders`), as a crafting
/// service.
#[derive(Clone, Debug, Default)]
pub struct PatternBook {
    patterns: Vec<(i32, Arc<Pattern>)>,
    craftable: KeyCounter,
    emitable: Vec<AeKey>,
}

impl PatternBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a pattern from a provider of `priority`: higher priorities are tried first.
    pub fn add(&mut self, pattern: Pattern, priority: i32) -> Arc<Pattern> {
        for output in &pattern.outputs {
            self.craftable.add(&output.what, 1);
        }
        let pattern = Arc::new(pattern);
        self.patterns.push((priority, pattern.clone()));
        pattern
    }

    /// A level emitter set to craft `key`.
    pub fn add_emitable(&mut self, key: AeKey) {
        self.emitable.push(key);
    }
}

impl CraftingService for PatternBook {
    fn can_emit_for(&self, key: &AeKey) -> bool {
        self.emitable.contains(key)
    }

    fn crafting_for(&self, key: &AeKey) -> Vec<Arc<Pattern>> {
        let mut found: Vec<_> =
            self.patterns.iter().filter(|(_, p)| p.outputs.iter().any(|o| o.what == *key)).collect();
        found.sort_by_key(|(priority, _)| std::cmp::Reverse(*priority));
        found.into_iter().map(|(_, p)| p.clone()).collect()
    }

    fn fuzzy_craftable(&self, key: &AeKey, filter: &dyn Fn(&AeKey) -> bool) -> Option<AeKey> {
        self.craftable.find_fuzzy(key, FuzzyMode::IgnoreAll).into_iter().map(|(k, _)| k).find(|k| filter(k))
    }
}

/// Why a calculation stopped.
#[derive(Clone, Debug, PartialEq)]
pub enum CraftError {
    /// `CraftBranchFailure`: this many of a key could be neither found nor crafted.
    Branch { what: AeKey, amount: i64 },
    /// A state AE2 reports as its own bug (`UnsupportedOperationException`,
    /// `IllegalStateException`).
    Bug(String),
}

impl fmt::Display for CraftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Branch { what, amount } => write!(f, "Failed: {what:?} x {amount}"),
            Self::Bug(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for CraftError {}

/// `CraftingPlan`: the outcome of a calculation.
#[derive(Clone, Debug)]
pub struct CraftingPlan {
    /// What will be crafted, and how many (fewer than asked with `CraftLess`).
    pub final_output: GenericStack,
    /// Crafting storage the job needs, in bytes.
    pub bytes: i64,
    /// Not everything was there: the plan only shows what is missing and can't start.
    pub simulation: bool,
    /// Some key had more than one pattern to choose from.
    pub multiple_paths: bool,
    /// What to take from the network's storage.
    pub used_items: KeyCounter,
    /// What level emitters will be asked for.
    pub emitted_items: KeyCounter,
    pub missing_items: KeyCounter,
    /// Each pattern and how many times it runs.
    pub pattern_times: Vec<(Arc<Pattern>, i64)>,
}

/// One `CraftingSimulationState`: a copy-on-read cache over the layer below.
#[derive(Default)]
struct Layer {
    unmodified: KeyCounter,
    modifiable: KeyCounter,
    emitted: KeyCounter,
    bytes: f64,
    crafts: Vec<(Arc<Pattern>, i64)>,
    required_extract: KeyCounter,
}

impl Layer {
    fn add_crafting(&mut self, pattern: &Arc<Pattern>, times: i64) {
        match self.crafts.iter_mut().find(|(p, _)| p == pattern) {
            Some((_, total)) => *total += times,
            None => self.crafts.push((pattern.clone(), times)),
        }
    }

    fn update_required_extract(&mut self, key: &AeKey, delta: i64) {
        if delta > 0 {
            let max = delta.max(self.required_extract.get(key));
            self.required_extract.set(key, max);
        }
    }
}

/// The simulated inventories, stacked: layer 0 is `NetworkCraftingSimulationState` over the
/// network's storage, each layer above a `ChildCraftingSimulationState` of the one below.
/// Operations act on the top layer. A child is pushed to try something, then applied onto its
/// parent (`applyDiff`) or dropped.
pub struct SimulationStack {
    network: KeyCounter,
    layers: Vec<Layer>,
}

impl SimulationStack {
    /// `NetworkCraftingSimulationState`: what the network holds, positive amounts only.
    pub fn new(storage: &KeyCounter) -> Self {
        let mut network = KeyCounter::new();
        for (key, amount) in storage.iter() {
            if amount > 0 {
                network.add(key, amount);
            }
        }
        Self { network, layers: vec![Layer::default()] }
    }

    fn top(&self) -> usize {
        self.layers.len() - 1
    }

    pub fn depth(&self) -> usize {
        self.layers.len()
    }

    pub fn push_child(&mut self) {
        self.layers.push(Layer::default());
    }

    /// Drops the layers above `depth`, keeping their parents as they were.
    pub fn truncate(&mut self, depth: usize) {
        self.layers.truncate(depth.max(1));
    }

    fn simulate_extract_parent(&mut self, layer: usize, what: &AeKey, amount: i64) -> i64 {
        if layer == 0 {
            self.network.get(what).min(amount)
        } else {
            self.extract_at(layer - 1, what, amount, Actionable::Simulate)
        }
    }

    fn find_fuzzy_parent(&mut self, layer: usize, what: &AeKey) -> Vec<AeKey> {
        if layer == 0 {
            self.network.find_fuzzy(what, FuzzyMode::IgnoreAll).into_iter().map(|(k, _)| k).collect()
        } else {
            self.fuzzy_templates_at(layer - 1, what)
        }
    }

    /// `cacheFuzzy`: the first time a layer sees an item, it copies every variant of it the
    /// layer below has.
    fn cache_fuzzy(&mut self, layer: usize, what: &AeKey) {
        if !self.layers[layer].unmodified.find_fuzzy(what, FuzzyMode::IgnoreAll).is_empty() {
            return;
        }
        let mut inserted_any = false;
        for key in self.find_fuzzy_parent(layer, what) {
            let extracted = self.simulate_extract_parent(layer, &key, i64::MAX);
            if extracted != 0 {
                inserted_any = true;
            }
            let l = &mut self.layers[layer];
            l.modifiable.add(&key, extracted);
            l.unmodified.add(&key, extracted);
        }
        if !inserted_any {
            self.layers[layer].unmodified.add(what, 0);
        }
    }

    fn insert_at(&mut self, layer: usize, what: &AeKey, amount: i64, mode: Actionable) {
        self.cache_fuzzy(layer, what);
        if mode == Actionable::Modulate {
            self.layers[layer].modifiable.add(what, amount);
        }
    }

    fn extract_at(&mut self, layer: usize, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        self.cache_fuzzy(layer, what);
        let l = &mut self.layers[layer];
        let cached = l.modifiable.get(what);
        if cached == 0 {
            return 0;
        }
        let extracted = cached.min(amount);
        if mode == Actionable::Modulate {
            l.modifiable.remove(what, extracted);
        }
        let delta = l.unmodified.get(what) - l.modifiable.get(what);
        l.update_required_extract(what, delta);
        extracted
    }

    fn fuzzy_templates_at(&mut self, layer: usize, what: &AeKey) -> Vec<AeKey> {
        self.cache_fuzzy(layer, what);
        self.layers[layer].modifiable.find_fuzzy(what, FuzzyMode::IgnoreAll).into_iter().map(|(k, _)| k).collect()
    }

    pub fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) {
        self.insert_at(self.top(), what, amount, mode);
    }

    pub fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        self.extract_at(self.top(), what, amount, mode)
    }

    /// `findFuzzyTemplates`: every variant of `what`'s item this layer can see.
    pub fn find_fuzzy_templates(&mut self, what: &AeKey) -> Vec<AeKey> {
        self.fuzzy_templates_at(self.top(), what)
    }

    pub fn emit_items(&mut self, what: &AeKey, amount: i64) {
        let top = self.top();
        self.layers[top].emitted.add(what, amount);
    }

    pub fn add_bytes(&mut self, bytes: f64) {
        let top = self.top();
        self.layers[top].bytes += bytes;
    }

    /// `addStackBytes`: eight bytes per byte the stack would take in a storage cell.
    pub fn add_stack_bytes(&mut self, key: &AeKey, amount: i64, multiplier: i64) {
        self.add_bytes(amount as f64 * multiplier as f64 / key.key_type().amount_per_byte() as f64 * 8.0);
    }

    pub fn add_crafting(&mut self, pattern: &Arc<Pattern>, times: i64) {
        let top = self.top();
        self.layers[top].add_crafting(pattern, times);
    }

    /// `ignore`: the layer acts as if the network held none of `what`.
    pub fn ignore(&mut self, what: &AeKey) {
        let top = self.top();
        self.cache_fuzzy(top, what);
        let l = &mut self.layers[top];
        l.unmodified.set(what, 0);
        l.modifiable.set(what, 0);
    }

    /// `applyDiff`: pops the top layer and applies what it changed onto its parent.
    pub fn apply_child(&mut self) -> Result<(), CraftError> {
        if self.layers.len() < 2 {
            return Err(CraftError::Bug("No child crafting state to apply".into()));
        }
        let child = self.layers.pop().expect("checked above");
        let parent = self.top();
        for (key, amount) in child.required_extract.iter() {
            let p = &mut self.layers[parent];
            let delta = p.unmodified.get(key) - p.modifiable.get(key) + amount;
            p.update_required_extract(key, delta);
        }
        for (key, amount) in child.modifiable.iter() {
            let size_delta = amount - child.unmodified.get(key);
            if size_delta > 0 {
                self.insert_at(parent, key, size_delta, Actionable::Modulate);
            } else if size_delta < 0 {
                let really = self.extract_at(parent, key, -size_delta, Actionable::Modulate);
                if really != -size_delta {
                    return Err(CraftError::Bug("Failed to extract from parent. This is a bug!".into()));
                }
            }
        }
        let p = &mut self.layers[parent];
        for (key, amount) in child.emitted.iter() {
            p.emitted.add(key, amount);
        }
        p.bytes += child.bytes;
        for (pattern, times) in &child.crafts {
            p.add_crafting(pattern, *times);
        }
        Ok(())
    }
}

/// `CraftingCpuHelper.extractTemplates`: takes whole multiples of a template, as many as are
/// there up to `multiplier`, and answers how many.
fn extract_templates(inv: &mut SimulationStack, key: &AeKey, amount: i64, multiplier: i64) -> Result<i64, CraftError> {
    let extracted = inv.extract(key, amount.saturating_mul(multiplier), Actionable::Simulate);
    if extracted == 0 {
        return Ok(0);
    }
    let multiplier = extracted / amount;
    let max_total = amount * multiplier;
    if max_total == 0 {
        return Ok(0);
    }
    let extracted = inv.extract(key, max_total, Actionable::Modulate);
    if extracted != 0 && extracted == max_total {
        Ok(multiplier)
    } else {
        Err(CraftError::Bug("Failed to correctly extract whole number. Invalid simulation!".into()))
    }
}

/// What a request carries down the tree.
/// How far one calculation may go before it is refused. A client names the amount, and each step can be one craft: on the
/// server thread a big one would stop the game. AE2 keeps this off the server thread and lets a screen cancel it.
pub const SERVER_THREAD_STEPS: u64 = 20_000;
pub const PLAN_THREAD_STEPS: u64 = 20_000_000;

thread_local! {
    static STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(SERVER_THREAD_STEPS) };
    static STEPS_LEFT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static CANCEL: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

/// The next calculations on this thread: how many steps each may take, and what stops them early.
pub fn limit_calculation(steps: u64, cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) {
    STEPS.set(steps);
    CANCEL.set(cancel);
}

fn calculation_step() -> Result<(), CraftError> {
    let left = STEPS_LEFT.get();
    let stopped = left % 4096 == 0 && CANCEL.with_borrow(|c| c.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)));
    if left == 0 || stopped {
        return Err(CraftError::Bug("That is too much to work out at once: ask for fewer.".to_owned()));
    }
    STEPS_LEFT.set(left - 1);
    Ok(())
}

/// Recipes inside recipes, at most. A stack overflow ends the process, whatever a client asked for.
const MAX_DEPTH: usize = 128;

struct Context<'a> {
    service: &'a dyn CraftingService,
    simulate: bool,
    missing: &'a mut KeyCounter,
    /// The keys of the nodes above the one requesting, root first, for `notRecursive`.
    ancestors: Vec<AeKey>,
}

/// `CraftingTreeNode`: `amount` of a key, per request, found in storage or crafted.
struct Node {
    parent_input: Option<PatternInput>,
    what: AeKey,
    amount: i64,
    processes: Option<Vec<Process>>,
    can_emit: bool,
}

impl Node {
    fn new(service: &dyn CraftingService, what: AeKey, amount: i64, parent_input: Option<PatternInput>) -> Self {
        let can_emit = service.can_emit_for(&what);
        let what = Self::find_crafted_stack(service, what, parent_input.as_ref());
        Self { parent_input, what, amount, processes: None, can_emit }
    }

    /// `findCraftedStack`: a key no pattern makes may have a craftable variant the input takes.
    fn find_crafted_stack(service: &dyn CraftingService, what: AeKey, parent_input: Option<&PatternInput>) -> AeKey {
        if service.can_emit_for(&what) {
            return what;
        }
        let Some(input) = parent_input else { return what };
        if !service.crafting_for(&what).is_empty() {
            return what;
        }
        let acceptable_amount = input.primary().amount;
        for possible in input.possible.iter().filter(|p| p.amount == acceptable_amount) {
            if let Some(fuzzy) = service.fuzzy_craftable(&possible.what, &|candidate| input.is_valid(candidate)) {
                return fuzzy;
            }
        }
        what
    }

    /// `buildChildPatterns`: the patterns for this key, but none that would make an ancestor.
    fn build_child_patterns(&mut self, ctx: &Context<'_>) {
        if self.processes.is_some() {
            return;
        }
        let processes = ctx
            .service
            .crafting_for(&self.what)
            .into_iter()
            .filter(|p| not_recursive(&ctx.ancestors, p))
            .map(|p| Process::new(ctx.service, p))
            .collect();
        self.processes = Some(processes);
    }

    /// `getValidItemTemplates`: the keys in storage that may fill this node.
    fn valid_templates(&self, inv: &mut SimulationStack) -> Vec<(AeKey, i64)> {
        let Some(input) = &self.parent_input else { return vec![(self.what.clone(), 1)] };
        let mut templates = Vec::new();
        for stack in &input.possible {
            for fuzz in inv.find_fuzzy_templates(&stack.what) {
                if input.is_valid(&fuzz) {
                    templates.push((fuzz, stack.amount));
                }
            }
        }
        templates
    }

    fn add_container_items(&self, template: &AeKey, multiplier: i64, out: Option<&mut KeyCounter>) {
        if let (Some(out), Some(input)) = (out, &self.parent_input) {
            if let Some(left) = input.remaining_key(template) {
                out.add(left, multiplier);
            }
        }
    }

    /// `request`: `requested` times this node's amount, from storage first, then crafted.
    fn request(
        &mut self,
        inv: &mut SimulationStack,
        ctx: &mut Context<'_>,
        mut requested: i64,
        mut container_items: Option<&mut KeyCounter>,
    ) -> Result<(), CraftError> {
        calculation_step()?;
        inv.add_stack_bytes(&self.what, self.amount, requested);
        for (key, amount) in self.valid_templates(inv) {
            let extracted = extract_templates(inv, &key, amount, requested)?;
            if extracted > 0 {
                requested -= extracted;
                self.add_container_items(&key, extracted, container_items.as_deref_mut());
                if requested == 0 {
                    return Ok(());
                }
            }
        }
        let what = self.what.clone();
        self.add_container_items(&what, requested, container_items);
        if self.can_emit {
            inv.emit_items(&what, self.amount * requested);
            return Ok(());
        }
        self.build_child_patterns(ctx);
        let mut total = requested * self.amount;
        let processes = self.processes.as_mut().expect("built above");
        if ctx.ancestors.len() >= MAX_DEPTH {
            return Err(CraftError::Bug("That recipe chain is too long to work out.".to_owned()));
        }
        ctx.ancestors.push(what.clone());
        let result = Self::craft(&what, processes, inv, ctx, &mut total, requested, self.amount);
        ctx.ancestors.pop();
        if result? {
            return Ok(());
        }
        if ctx.simulate {
            ctx.missing.add(&what, total);
            Ok(())
        } else {
            Err(CraftError::Branch { what, amount: total })
        }
    }

    /// The crafting half of `request`: answers whether `total` was crafted in full.
    fn craft(
        what: &AeKey,
        processes: &mut [Process],
        inv: &mut SimulationStack,
        ctx: &mut Context<'_>,
        total: &mut i64,
        requested: i64,
        amount: i64,
    ) -> Result<bool, CraftError> {
        if let [pro] = &mut *processes {
            let crafted_per_pattern = pro.pattern.output_count(what);
            while pro.possible && *total > 0 {
                let times = if pro.limit_qty || crafted_per_pattern <= 0 {
                    1
                } else {
                    (*total + crafted_per_pattern - 1) / crafted_per_pattern
                };
                pro.request(inv, ctx, times)?;
                let available = inv.extract(what, *total, Actionable::Modulate);
                if available == 0 {
                    return Err(CraftError::Bug(format!(
                        "Unexpected error in the crafting calculation: can't find created items.\n\
                         - Found none of {what:?}. Remaining request: {total} of {requested}*{amount}.\n\
                         - Tried crafting {times} times the pattern {:?}.\n- Pattern outputs: {:?}.",
                        pro.pattern.definition, pro.pattern.outputs
                    )));
                }
                *total -= available;
                if *total <= 0 {
                    return Ok(true);
                }
            }
        } else {
            for pro in processes.iter_mut() {
                let depth = inv.depth();
                match Self::try_one_by_one(what, pro, inv, ctx, total) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(CraftError::Branch { .. }) => {
                        inv.truncate(depth);
                        pro.possible = true;
                    }
                    Err(bug) => return Err(bug),
                }
            }
        }
        Ok(false)
    }

    /// With several patterns, each is tried one craft at a time in a child state, kept only when
    /// it made some of the key.
    fn try_one_by_one(
        what: &AeKey,
        pro: &mut Process,
        inv: &mut SimulationStack,
        ctx: &mut Context<'_>,
        total: &mut i64,
    ) -> Result<bool, CraftError> {
        while pro.possible && *total > 0 {
            let depth = inv.depth();
            inv.push_child();
            pro.request(inv, ctx, 1)?;
            let available = inv.extract(what, *total, Actionable::Modulate);
            if available != 0 {
                inv.apply_child()?;
                *total -= available;
                if *total <= 0 {
                    return Ok(true);
                }
            } else {
                inv.truncate(depth);
                pro.possible = false;
            }
        }
        Ok(false)
    }

    fn node_count(&self) -> i64 {
        1 + self.processes.iter().flatten().map(Process::node_count).sum::<i64>()
    }

    fn has_multiple_paths(&self) -> bool {
        match &self.processes {
            None => false,
            Some(p) if p.len() > 1 => true,
            Some(p) => p.iter().any(Process::has_multiple_paths),
        }
    }
}

/// `CraftingTreeNode.notRecursive`: a pattern that makes, or takes first, a key being crafted
/// above would loop.
fn not_recursive(ancestors: &[AeKey], pattern: &Pattern) -> bool {
    !ancestors.iter().any(|a| {
        pattern.outputs.iter().any(|o| o.what == *a) || pattern.inputs.iter().any(|i| i.primary().what == *a)
    })
}

/// `CraftingTreeProcess`: one pattern under a node, with a node per input.
struct Process {
    pattern: Arc<Pattern>,
    nodes: Vec<(Node, i64)>,
    possible: bool,
    container_items: bool,
    limit_qty: bool,
}

impl Process {
    fn new(service: &dyn CraftingService, pattern: Arc<Pattern>) -> Self {
        let mut limit_qty = false;
        let mut container_items = false;
        // updateLimitQty: a pattern that gives back one of its inputs, or leaves a container,
        // runs one craft at a time.
        for input in &pattern.inputs {
            let primary = input.primary();
            if pattern.outputs.iter().any(|o| o.what == primary.what) {
                limit_qty = true;
            }
            if input.remaining_key(&primary.what).is_some() {
                limit_qty = true;
                container_items = true;
            }
        }
        let nodes = pattern
            .inputs
            .iter()
            .map(|input| {
                let first = input.primary();
                (Node::new(service, first.what.clone(), first.amount, Some(input.clone())), input.multiplier)
            })
            .collect();
        Self { pattern, nodes, possible: true, container_items, limit_qty }
    }

    fn request(&mut self, inv: &mut SimulationStack, ctx: &mut Context<'_>, times: i64) -> Result<(), CraftError> {
        let mut containers = self.container_items.then(KeyCounter::new);
        for (node, multiplier) in &mut self.nodes {
            node.request(inv, ctx, *multiplier * times, containers.as_mut())?;
        }
        if let Some(containers) = containers {
            for (key, amount) in containers.iter() {
                inv.insert(key, amount, Actionable::Modulate);
                inv.add_stack_bytes(key, amount, 1);
            }
        }
        for out in &self.pattern.outputs {
            inv.insert(&out.what, out.amount * times, Actionable::Modulate);
        }
        inv.add_crafting(&self.pattern, times);
        inv.add_bytes(times as f64);
        Ok(())
    }

    fn node_count(&self) -> i64 {
        self.nodes.iter().map(|(n, _)| n.node_count()).sum()
    }

    fn has_multiple_paths(&self) -> bool {
        self.nodes.iter().any(|(n, _)| n.has_multiple_paths())
    }
}

/// `CraftingCalculation`: plans crafting `amount` of `output` from what the network
/// `storage` holds, with the patterns and emitters of `service`. The requested key itself is
/// never taken from storage: a request always crafts.
pub fn calculate(
    service: &dyn CraftingService,
    storage: &KeyCounter,
    output: &AeKey,
    amount: i64,
    strategy: CalculationStrategy,
) -> Result<CraftingPlan, CraftError> {
    STEPS_LEFT.set(STEPS.get());
    let mut calc = Calculation {
        service,
        network: SimulationStack::new(storage),
        tree: Node::new(service, output.clone(), 1, None),
        output: output.clone(),
        missing: KeyCounter::new(),
    };
    if let Some(plan) = calc.attempt(false, amount)? {
        return Ok(plan);
    }
    if strategy == CalculationStrategy::CraftLess && amount > 0 {
        // A binary search for the most that can be crafted.
        let mut successful_amount = 0;
        let mut successful_plan = None;
        let mut increment = 1i64 << (63 - amount.leading_zeros());
        while increment > 0 {
            let test_amount = successful_amount + increment;
            if test_amount < amount {
                if let Some(plan) = calc.attempt(false, test_amount)? {
                    successful_amount = test_amount;
                    successful_plan = Some(plan);
                }
            }
            increment /= 2;
        }
        if let Some(plan) = successful_plan {
            return Ok(plan);
        }
    }
    Ok(calc.attempt(true, amount)?.expect("a simulated attempt always plans"))
}

struct Calculation<'a> {
    service: &'a dyn CraftingService,
    network: SimulationStack,
    tree: Node,
    output: AeKey,
    missing: KeyCounter,
}

impl Calculation<'_> {
    /// `runCraftAttempt`: `None` when the full amount can't be had and this isn't a simulation.
    fn attempt(&mut self, simulate: bool, amount: i64) -> Result<Option<CraftingPlan>, CraftError> {
        let inv = &mut self.network;
        inv.push_child();
        inv.ignore(&self.output);
        let mut ctx = Context { service: self.service, simulate, missing: &mut self.missing, ancestors: Vec::new() };
        match self.tree.request(inv, &mut ctx, amount, None) {
            Ok(()) => {}
            Err(CraftError::Branch { .. }) => {
                inv.truncate(1);
                return Ok(None);
            }
            Err(bug) => {
                inv.truncate(1);
                return Err(bug);
            }
        }
        inv.add_bytes(self.tree.node_count() as f64 * 8.0);
        let state = inv.layers.pop().expect("the attempt's child state");
        inv.truncate(1);
        Ok(Some(CraftingPlan {
            final_output: GenericStack { what: self.output.clone(), amount },
            bytes: state.bytes.ceil() as i64,
            simulation: simulate,
            multiple_paths: self.tree.has_multiple_paths(),
            used_items: state.required_extract,
            emitted_items: state.emitted,
            missing_items: self.missing.clone(),
            pattern_times: state.crafts,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> AeKey {
        AeKey::item(id, "", 0, 0)
    }

    fn stack(id: &str, amount: i64) -> GenericStack {
        GenericStack { what: item(id), amount }
    }

    fn pattern(name: &str, inputs: &[(&str, i64)], outputs: &[(&str, i64)]) -> Pattern {
        Pattern {
            definition: AeKey::item("ae2:processing_pattern", name, 0, 0),
            inputs: inputs.iter().map(|(id, n)| PatternInput::of(item(id), *n)).collect(),
            outputs: outputs.iter().map(|(id, n)| stack(id, *n)).collect(),
        }
    }

    fn storage(items: &[(&str, i64)]) -> KeyCounter {
        let mut c = KeyCounter::new();
        for (id, n) in items {
            c.add(&item(id), *n);
        }
        c
    }

    fn times_of(plan: &CraftingPlan, name: &str) -> i64 {
        plan.pattern_times.iter().find(|(p, _)| p.definition.components() == name).map_or(0, |(_, t)| *t)
    }

    fn book() -> PatternBook {
        let mut book = PatternBook::new();
        book.add(pattern("planks", &[("minecraft:oak_log", 1)], &[("minecraft:oak_planks", 4)]), 0);
        book.add(pattern("sticks", &[("minecraft:oak_planks", 2)], &[("minecraft:stick", 4)]), 0);
        book
    }

    #[test]
    fn crafts_from_storage() {
        let plan = calculate(&book(), &storage(&[("minecraft:oak_log", 5)]), &item("minecraft:oak_planks"), 8, CalculationStrategy::ReportMissingItems).unwrap();
        assert!(!plan.simulation);
        assert_eq!(times_of(&plan, "planks"), 2);
        assert_eq!(plan.used_items.get(&item("minecraft:oak_log")), 2);
        assert!(plan.missing_items.is_empty());
        assert!(plan.bytes > 0);
    }

    #[test]
    fn the_requested_item_is_crafted_not_taken() {
        let plan = calculate(&book(), &storage(&[("minecraft:oak_log", 1), ("minecraft:oak_planks", 64)]), &item("minecraft:oak_planks"), 4, CalculationStrategy::ReportMissingItems).unwrap();
        assert_eq!(times_of(&plan, "planks"), 1);
        assert_eq!(plan.used_items.get(&item("minecraft:oak_planks")), 0);
    }

    #[test]
    fn crafts_down_the_tree() {
        let plan = calculate(&book(), &storage(&[("minecraft:oak_log", 1)]), &item("minecraft:stick"), 4, CalculationStrategy::ReportMissingItems).unwrap();
        assert!(!plan.simulation);
        assert_eq!(times_of(&plan, "sticks"), 1);
        assert_eq!(times_of(&plan, "planks"), 1);
        assert_eq!(plan.used_items.get(&item("minecraft:oak_log")), 1);
        // Planks in storage are used before crafting more.
        let plan = calculate(&book(), &storage(&[("minecraft:oak_planks", 2)]), &item("minecraft:stick"), 4, CalculationStrategy::ReportMissingItems).unwrap();
        assert_eq!(times_of(&plan, "planks"), 0);
        assert_eq!(plan.used_items.get(&item("minecraft:oak_planks")), 2);
    }

    #[test]
    fn reports_what_is_missing() {
        let plan = calculate(&book(), &storage(&[("minecraft:oak_log", 1)]), &item("minecraft:oak_planks"), 8, CalculationStrategy::ReportMissingItems).unwrap();
        assert!(plan.simulation);
        assert_eq!(plan.missing_items.get(&item("minecraft:oak_log")), 1);
        assert_eq!(plan.final_output.amount, 8);
    }

    #[test]
    fn craft_less_finds_the_most_it_can() {
        let plan = calculate(&book(), &storage(&[("minecraft:oak_log", 3)]), &item("minecraft:oak_planks"), 100, CalculationStrategy::CraftLess).unwrap();
        assert!(!plan.simulation);
        assert_eq!(plan.final_output.amount, 12);
        assert_eq!(times_of(&plan, "planks"), 3);
    }

    #[test]
    fn a_pattern_loop_ends() {
        let mut book = PatternBook::new();
        book.add(pattern("a", &[("test:b", 1)], &[("test:a", 1)]), 0);
        book.add(pattern("b", &[("test:a", 1)], &[("test:b", 1)]), 0);
        let plan = calculate(&book, &KeyCounter::new(), &item("test:a"), 1, CalculationStrategy::ReportMissingItems).unwrap();
        assert!(plan.simulation);
        assert_eq!(plan.missing_items.get(&item("test:b")), 1);
    }

    #[test]
    fn falls_back_to_the_next_pattern() {
        let mut book = PatternBook::new();
        book.add(pattern("from_gold", &[("minecraft:gold_ingot", 1)], &[("test:gear", 1)]), 10);
        book.add(pattern("from_iron", &[("minecraft:iron_ingot", 1)], &[("test:gear", 1)]), 0);
        let plan = calculate(&book, &storage(&[("minecraft:gold_ingot", 1), ("minecraft:iron_ingot", 5)]), &item("test:gear"), 3, CalculationStrategy::ReportMissingItems).unwrap();
        assert!(!plan.simulation);
        assert!(plan.multiple_paths);
        assert_eq!(times_of(&plan, "from_gold"), 1);
        assert_eq!(times_of(&plan, "from_iron"), 2);
        assert_eq!(plan.used_items.get(&item("minecraft:iron_ingot")), 2);
    }

    #[test]
    fn containers_come_back() {
        let mut book = PatternBook::new();
        let mut milk = PatternInput::of(item("minecraft:milk_bucket"), 1);
        milk.remainders.push((item("minecraft:milk_bucket"), item("minecraft:bucket")));
        book.add(Pattern {
            definition: AeKey::item("ae2:crafting_pattern", "cake", 0, 0),
            inputs: vec![milk, PatternInput::of(item("minecraft:sugar"), 1)],
            outputs: vec![stack("minecraft:cake", 1)],
        }, 0);
        book.add(pattern("milk", &[("minecraft:bucket", 1)], &[("minecraft:milk_bucket", 1)]), 0);
        // One bucket goes round: milked, used in a cake, given back, milked again.
        let plan = calculate(&book, &storage(&[("minecraft:bucket", 1), ("minecraft:sugar", 2)]), &item("minecraft:cake"), 2, CalculationStrategy::ReportMissingItems).unwrap();
        assert!(!plan.simulation, "missing {:?}", plan.missing_items);
        assert_eq!(times_of(&plan, "cake"), 2);
        assert_eq!(times_of(&plan, "milk"), 2);
        assert_eq!(plan.used_items.get(&item("minecraft:bucket")), 1);
    }

    #[test]
    fn emitters_are_asked_not_crafted() {
        let mut book = book();
        book.add_emitable(item("minecraft:oak_log"));
        let plan = calculate(&book, &KeyCounter::new(), &item("minecraft:oak_planks"), 4, CalculationStrategy::ReportMissingItems).unwrap();
        assert!(!plan.simulation);
        assert_eq!(plan.emitted_items.get(&item("minecraft:oak_log")), 1);
    }
}
