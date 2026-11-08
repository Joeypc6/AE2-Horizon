// Applied Energistics 2's crafting CPU, ported by hand from appliedenergistics2-fabric-15.4.12.jar
// (appeng.crafting.execution: CraftingCpuLogic, ExecutingCraftingJob, CraftingCpuHelper,
// ElapsedTimeTracker): it takes a plan's ingredients out of storage, pushes patterns to pattern
// providers a few per tick as its co-processors allow, waits for what they send back, and hands
// the final output to whoever asked. The world it acts on (ME storage, the providers, the energy
// grid, a requesting machine) is the `CpuNetwork` the game side implements. No `//!` comments:
// module payloads can't hold inner attributes.

use super::port_ae2_core::{Actionable, AeKey, FuzzyMode, GenericStack, KeyCounter, KeyType};
use super::port_ae2_crafting::{CraftingPlan, Pattern};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

/// What a crafting CPU works with on its network.
pub trait CpuNetwork {
    /// Takes up to `amount` from the network's storage.
    fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64;
    /// Puts up to `amount` into the network's storage; answers how much went in.
    fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64;
    /// How many pattern providers hold `pattern` (`getProviders`), tried in this order.
    fn provider_count(&self, pattern: &Pattern) -> usize;
    fn provider_busy(&self, pattern: &Pattern, provider: usize) -> bool;
    /// `pushPattern`: hands one craft's inputs, slot by slot, to a provider. False when it
    /// can't take them now.
    fn push_pattern(&mut self, pattern: &Pattern, provider: usize, inputs: &[KeyCounter]) -> bool;
    /// `extractAEPower` with the config's multiplier: answers how much AE there was.
    fn extract_power(&mut self, amount: f64, mode: Actionable) -> f64;
    /// `CraftingLink.insert`: delivers the final output to the machine that requested the
    /// job. A job a player asked for has none: its output goes to storage.
    fn deliver(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
        let _ = (what, amount, mode);
        0
    }
}

/// Why a job wasn't started (`CraftingSubmitErrorCode`).
#[derive(Clone, Debug, PartialEq)]
pub enum SubmitError {
    /// The plan has missing items.
    IncompletePlan,
    CpuBusy,
    CpuOffline,
    /// The plan needs more bytes than the CPU's storage.
    CpuTooSmall,
    /// Storage changed since the plan: this much of a key wasn't there.
    MissingIngredient(GenericStack),
}

/// What a CPU tells the game side about (`postChange`, `notifyJobOwner`).
#[derive(Clone, Debug, PartialEq)]
pub enum CpuEvent {
    Started { craft_id: u128, player: Option<i32> },
    Finished { craft_id: u128, player: Option<i32> },
    Cancelled { craft_id: u128, player: Option<i32> },
}

/// `ElapsedTimeTracker`: how long the job has run and how much of its work is done, in units
/// (items, buckets).
#[derive(Clone, Debug)]
pub struct ElapsedTimeTracker {
    last_time: Instant,
    elapsed_nanos: u64,
    started: [i64; 2],
    completed: [i64; 2],
}

impl Default for ElapsedTimeTracker {
    fn default() -> Self {
        Self { last_time: Instant::now(), elapsed_nanos: 0, started: [0; 2], completed: [0; 2] }
    }
}

fn type_index(kind: KeyType) -> usize {
    match kind {
        KeyType::Items => 0,
        KeyType::Fluids => 1,
    }
}

impl ElapsedTimeTracker {
    fn update_time(&mut self) {
        let now = Instant::now();
        self.elapsed_nanos += now.duration_since(self.last_time).as_nanos() as u64;
        self.last_time = now;
    }

    fn decrement_items(&mut self, amount: i64, kind: KeyType) {
        self.update_time();
        let c = &mut self.completed[type_index(kind)];
        *c = c.checked_add(amount).unwrap_or(i64::MAX);
    }

    fn add_max_items(&mut self, amount: i64, kind: KeyType) {
        self.update_time();
        let s = &mut self.started[type_index(kind)];
        *s = s.checked_add(amount).unwrap_or(i64::MAX);
    }

    /// Nanoseconds the job has run; the clock stops once all work is done.
    /// What has been started and completed so far, of every kind.
    pub fn done_so_far(&self) -> i64 {
        self.started.iter().chain(&self.completed).sum()
    }

    pub fn elapsed_nanos(&self) -> u64 {
        let all_done = (0..2).all(|i| self.completed[i] >= self.started[i]);
        if all_done { self.elapsed_nanos } else { self.elapsed_nanos + self.last_time.elapsed().as_nanos() as u64 }
    }

    /// From 0 to 1.
    pub fn progress(&self) -> f32 {
        let mut started = 0.0;
        let mut completed = 0.0;
        for kind in [KeyType::Items, KeyType::Fluids] {
            let per_unit = kind.amount_per_unit() as f64;
            started += self.started[type_index(kind)] as f64 / per_unit;
            completed += self.completed[type_index(kind)] as f64 / per_unit;
        }
        ((completed / started) as f32).clamp(0.0, 1.0)
    }
}

/// `ExecutingCraftingJob`.
#[derive(Clone, Debug)]
pub struct CraftingJob {
    pub craft_id: u128,
    pub final_output: GenericStack,
    pub remaining_amount: i64,
    /// What providers and level emitters still owe the CPU.
    waiting_for: KeyCounter,
    /// Each pattern still to push, and how many more times.
    tasks: Vec<(Arc<Pattern>, i64)>,
    pub time: ElapsedTimeTracker,
    /// A machine asked for it, and receives the final output (`requester != null`).
    has_requester: bool,
    pub player: Option<i32>,
}

impl CraftingJob {
    fn new(plan: &CraftingPlan, craft_id: u128, has_requester: bool, player: Option<i32>) -> Self {
        let mut job = Self {
            craft_id,
            final_output: plan.final_output.clone(),
            remaining_amount: plan.final_output.amount,
            waiting_for: KeyCounter::new(),
            tasks: Vec::new(),
            time: ElapsedTimeTracker::default(),
            has_requester,
            player,
        };
        for (key, amount) in plan.emitted_items.iter() {
            job.waiting_for.add(key, amount);
            job.time.add_max_items(amount, key.key_type());
        }
        for (pattern, times) in &plan.pattern_times {
            match job.tasks.iter_mut().find(|(p, _)| p == pattern) {
                Some((_, t)) => *t += times,
                None => job.tasks.push((pattern.clone(), *times)),
            }
            for output in &pattern.outputs {
                let amount = output.amount * times * output.what.key_type().amount_per_unit();
                job.time.add_max_items(amount, output.what.key_type());
            }
        }
        job
    }
}

/// `ListCraftingInventory.extract`: takes what is there, dropping emptied keys.
fn list_extract(list: &mut KeyCounter, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
    let extracted = list.get(what).min(amount).max(0);
    if mode == Actionable::Modulate {
        list.remove(what, extracted);
        list.remove_zeros();
    }
    extracted
}

/// `CraftingCpuHelper.extractPatternInputs`: one craft's inputs taken out of the CPU's
/// inventory, slot by slot, with what the craft will send back. `None`, with nothing taken,
/// when an input is short.
fn extract_pattern_inputs(pattern: &Pattern, inv: &mut KeyCounter) -> Option<(Vec<KeyCounter>, KeyCounter, KeyCounter)> {
    let mut holder = Vec::with_capacity(pattern.inputs.len());
    let mut expected_containers = KeyCounter::new();
    let mut found = true;
    for input in &pattern.inputs {
        let mut list = KeyCounter::new();
        let mut remaining = input.multiplier;
        // getValidItemTemplates over the CPU's inventory.
        let mut templates = Vec::new();
        for stack in &input.possible {
            for (fuzz, _) in inv.find_fuzzy(&stack.what, FuzzyMode::IgnoreAll) {
                if input.is_valid(&fuzz) {
                    templates.push((fuzz, stack.amount));
                }
            }
        }
        for (key, amount) in templates {
            // extractTemplates: whole multiples only.
            let available = list_extract(inv, &key, amount.saturating_mul(remaining), Actionable::Simulate);
            let times = available / amount;
            if times > 0 {
                list_extract(inv, &key, amount * times, Actionable::Modulate);
                list.add(&key, times * amount);
                if let Some(left) = input.remaining_key(&key) {
                    expected_containers.add(left, times);
                }
                remaining -= times;
                if remaining == 0 {
                    break;
                }
            }
        }
        holder.push(list);
        if remaining > 0 {
            found = false;
            break;
        }
    }
    if !found {
        reinject(inv, &holder);
        return None;
    }
    let mut expected_outputs = KeyCounter::new();
    for output in &pattern.outputs {
        expected_outputs.add(&output.what, output.amount);
    }
    Some((holder, expected_outputs, expected_containers))
}

fn reinject(inv: &mut KeyCounter, holder: &[KeyCounter]) {
    for list in holder {
        for (key, amount) in list.iter() {
            inv.add(key, amount);
        }
    }
}

/// `calculatePatternPower`: one AE per operation's worth of input.
fn pattern_power(inputs: &[KeyCounter]) -> f64 {
    inputs.iter().flat_map(KeyCounter::iter).map(|(k, a)| a as f64 / k.key_type().amount_per_operation() as f64).sum()
}

fn new_craft_id() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    nanos ^ ((COUNTER.fetch_add(1, Ordering::Relaxed) as u128) << 64)
}

/// `CraftingCpuLogic` with its cluster's numbers: a crafting CPU multiblock.
#[derive(Debug, Default)]
pub struct CraftingCpu {
    /// The cluster's crafting storage, in bytes.
    pub storage_bytes: i64,
    /// Co-processing units: one more pattern push a tick each.
    pub co_processors: i32,
    /// Powered and on a network.
    pub active: bool,
    job: Option<CraftingJob>,
    /// Ingredients and intermediate results held by the CPU.
    inventory: KeyCounter,
    used_ops: [i32; 3],
    cant_store_items: bool,
    dirty: bool,
    changed: HashSet<AeKey>,
    events: Vec<CpuEvent>,
}

impl CraftingCpu {
    pub fn new(storage_bytes: i64, co_processors: i32) -> Self {
        Self { storage_bytes, co_processors, active: true, ..Self::default() }
    }

    /// `trySubmitJob`: takes the plan's ingredients from storage and starts. `has_requester`:
    /// a machine asked and gets the output through `CpuNetwork::deliver`.
    pub fn submit(
        &mut self,
        plan: &CraftingPlan,
        net: &mut dyn CpuNetwork,
        has_requester: bool,
        player: Option<i32>,
    ) -> Result<u128, SubmitError> {
        if plan.simulation {
            return Err(SubmitError::IncompletePlan);
        }
        if self.job.is_some() {
            return Err(SubmitError::CpuBusy);
        }
        if !self.active {
            return Err(SubmitError::CpuOffline);
        }
        if self.storage_bytes < plan.bytes {
            return Err(SubmitError::CpuTooSmall);
        }
        // tryExtractInitialItems: all or nothing.
        for (what, to_extract) in plan.used_items.iter() {
            let extracted = net.extract(what, to_extract, Actionable::Modulate);
            self.inventory.add(what, extracted);
            if extracted < to_extract {
                self.inventory.remove_zeros();
                // What storage won't take back now stays here, and `store_items` keeps trying.
                self.store_items(net);
                return Err(SubmitError::MissingIngredient(GenericStack { what: what.clone(), amount: to_extract - extracted }));
            }
        }
        self.inventory.remove_zeros();
        let craft_id = new_craft_id();
        let job = CraftingJob::new(plan, craft_id, has_requester, player);
        self.events.push(CpuEvent::Started { craft_id, player });
        self.job = Some(job);
        self.dirty = true;
        Ok(craft_id)
    }

    /// `tickCraftingLogic`: once a server tick. Without a job, it returns what it holds to
    /// storage. With one, it pushes patterns: one a tick, plus one per co-processor, averaged
    /// over the last three ticks.
    pub fn tick(&mut self, net: &mut dyn CpuNetwork) {
        if !self.active {
            return;
        }
        self.cant_store_items = false;
        if self.job.is_none() {
            self.store_items(net);
            if !self.inventory.is_empty() {
                self.cant_store_items = true;
            }
            return;
        }
        let mut remaining = self.co_processors + 1 - self.used_ops.iter().sum::<i32>();
        let started = remaining;
        while remaining > 0 {
            let pushed = self.execute_crafting(remaining, net);
            if pushed <= 0 {
                break;
            }
            remaining -= pushed;
        }
        self.used_ops = [started - remaining, self.used_ops[0], self.used_ops[1]];
    }

    /// `executeCrafting`: pushes up to `max_patterns` crafts whose inputs the CPU holds.
    pub fn execute_crafting(&mut self, max_patterns: i32, net: &mut dyn CpuNetwork) -> i32 {
        let Some(job) = self.job.as_mut() else { return 0 };
        let inventory = &mut self.inventory;
        let mut pushed = 0;
        let mut i = 0;
        while i < job.tasks.len() {
            if job.tasks[i].1 <= 0 {
                job.tasks.remove(i);
                continue;
            }
            let pattern = job.tasks[i].0.clone();
            let mut container = extract_pattern_inputs(&pattern, inventory);
            let mut removed = false;
            let providers = net.provider_count(&pattern);
            let mut provider = 0;
            while provider < providers {
                let Some((inputs, outputs, containers)) = &container else { break };
                if net.provider_busy(&pattern, provider) {
                    provider += 1;
                    continue;
                }
                let power = pattern_power(inputs);
                if net.extract_power(power, Actionable::Simulate) < power - 0.01 {
                    break;
                }
                if !net.push_pattern(&pattern, provider, inputs) {
                    provider += 1;
                    continue;
                }
                net.extract_power(power, Actionable::Modulate);
                pushed += 1;
                for (key, amount) in outputs.iter() {
                    job.waiting_for.add(key, amount);
                }
                for (key, amount) in containers.iter() {
                    job.waiting_for.add(key, amount);
                    job.time.add_max_items(amount, key.key_type());
                }
                self.dirty = true;
                job.tasks[i].1 -= 1;
                if job.tasks[i].1 <= 0 {
                    job.tasks.remove(i);
                    removed = true;
                    container = None;
                    break;
                }
                if pushed == max_patterns {
                    return pushed;
                }
                // The same provider list goes on with the next craft (AE2's iterator).
                provider += 1;
                container = extract_pattern_inputs(&pattern, inventory);
            }
            if let Some((inputs, _, _)) = &container {
                reinject(inventory, inputs);
            }
            if !removed {
                i += 1;
            }
        }
        pushed
    }

    /// `insert`: something a provider or emitter sends back. The CPU takes only what it
    /// waits for; the final output goes on to the requester, or (answering 0) to storage.
    pub fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable, net: &mut dyn CpuNetwork) -> i64 {
        let Some(job) = self.job.as_mut() else { return 0 };
        let waiting = list_extract(&mut job.waiting_for, what, amount, Actionable::Simulate);
        if waiting <= 0 {
            return 0;
        }
        let amount = amount.min(waiting);
        if mode == Actionable::Modulate {
            job.time.decrement_items(amount, what.key_type());
            list_extract(&mut job.waiting_for, what, amount, Actionable::Modulate);
            self.dirty = true;
        }
        if *what == job.final_output.what {
            let inserted = if job.has_requester { net.deliver(what, amount, mode) } else { 0 };
            if mode == Actionable::Modulate {
                self.changed.insert(what.clone());
                job.remaining_amount = (job.remaining_amount - amount).max(0);
                if job.remaining_amount <= 0 {
                    self.finish_job(true, net);
                }
            }
            inserted
        } else {
            if mode == Actionable::Modulate {
                self.inventory.add(what, amount);
                self.changed.insert(what.clone());
            }
            amount
        }
    }

    fn finish_job(&mut self, success: bool, net: &mut dyn CpuNetwork) {
        let Some(job) = self.job.take() else { return };
        for (pattern, _) in &job.tasks {
            for output in &pattern.outputs {
                self.changed.insert(output.what.clone());
            }
        }
        let (craft_id, player) = (job.craft_id, job.player);
        self.events.push(if success {
            CpuEvent::Finished { craft_id, player }
        } else {
            CpuEvent::Cancelled { craft_id, player }
        });
        self.store_items(net);
    }

    /// `cancel`: drops the job and returns what the CPU holds to storage.
    pub fn cancel(&mut self, net: &mut dyn CpuNetwork) {
        if self.job.is_some() {
            self.finish_job(false, net);
        }
    }

    /// `storeItems`: what the CPU holds, back into storage as far as it fits.
    pub fn store_items(&mut self, net: &mut dyn CpuNetwork) {
        debug_assert!(self.job.is_none(), "CPU should not have a job to prevent re-insertion when dumping items");
        if self.inventory.is_empty() {
            return;
        }
        let held: Vec<(AeKey, i64)> = self.inventory.iter().map(|(k, a)| (k.clone(), a)).collect();
        for (key, amount) in held {
            self.changed.insert(key.clone());
            let inserted = net.insert(&key, amount, Actionable::Modulate);
            self.inventory.remove(&key, inserted);
        }
        self.inventory.remove_zeros();
        self.dirty = true;
    }

    /// Ticks until the job is done, by playing its remaining work forward: each pattern's crafts start as its
    /// machines free up (`machine` says a pattern's crafts a tick and ticks per craft, for what is connected now)
    /// and as their ingredients arrive, within what the CPU pushes a tick. None when nothing can make it go on.
    pub fn forecast(&self, machine: &mut dyn FnMut(&Pattern) -> Option<(f64, f64)>, owed_rate: &mut dyn FnMut(&AeKey) -> (f64, f64)) -> Option<f64> {
        struct Task {
            inputs: Vec<(Vec<AeKey>, f64)>,
            outputs: Vec<(AeKey, f64)>,
            left: f64,
            rate: f64,
            latency: f64,
            credit: f64,
            flying: std::collections::VecDeque<(f64, f64)>,
        }
        let job = self.job.as_ref()?;
        let mut stock: HashMap<AeKey, f64> = self.inventory.iter().map(|(k, n)| (k.clone(), n as f64)).collect();
        let mut tasks: Vec<Task> = Vec::new();
        for (pattern, times) in &job.tasks {
            if *times <= 0 {
                continue;
            }
            let (rate, latency) = machine(pattern)?;
            let mut outputs: Vec<(AeKey, f64)> = pattern.outputs.iter().map(|o| (o.what.clone(), o.amount as f64)).collect();
            let mut inputs = Vec::new();
            for input in &pattern.inputs {
                let per = input.possible.first().map_or(1, |g| g.amount) as f64 * input.multiplier as f64;
                let keys: Vec<AeKey> = if input.substitute { input.possible.iter().map(|g| g.what.clone()).collect() } else { input.possible.iter().take(1).map(|g| g.what.clone()).collect() };
                if let Some(left) = keys.first().and_then(|k| input.remaining_key(k)) {
                    outputs.push((left.clone(), input.multiplier as f64));
                }
                inputs.push((keys, per));
            }
            // Every machine is free at the start.
            let credit = (rate * latency).max(1.0);
            tasks.push(Task { inputs, outputs, left: *times as f64, rate, latency, credit, flying: Default::default() });
        }
        // What is out being made now comes back at its machines' pace, however much waits in them.
        let mut owed: Vec<(f64, AeKey, f64)> = Vec::new();
        for (k, n) in job.waiting_for.iter() {
            // How much of the next one its machine has already done counts too.
            let (rate, done) = owed_rate(k);
            let (n, rate) = (n as f64, rate.max(1e-6));
            let all = (n / rate - done).max(0.0);
            const SLICES: usize = 16;
            for i in 0..SLICES {
                owed.push(((i + 1) as f64 / SLICES as f64 * all, k.clone(), n / SLICES as f64));
            }
        }
        owed.sort_by(|a, b| a.0.total_cmp(&b.0));
        // The machines that still have those to make aren't free for the next crafts.
        for t in &mut tasks {
            let backlog = t.outputs.first().map_or(0.0, |(key, each)| job.waiting_for.get(key) as f64 / each.max(1.0));
            t.credit -= backlog;
        }
        let mut owed = std::collections::VecDeque::from(owed);
        let longest = tasks.iter().map(|t| t.left / t.rate).sum::<f64>() + owed.back().map_or(0.0, |o| o.0);
        let step = (longest / 500.0).max(1.0);
        // `tick`: pushes in the last three ticks count against the CPU's limit, so a push frees its place on the
        // fourth tick after it: co-processors + 1 pushes every four ticks (one a tick at three co-processors).
        let ops_per_tick = f64::from(self.co_processors + 1) / 4.0;
        let mut now = 0.0f64;
        let mut ops = 0.0f64;
        // Long jobs' steps split crafts: what's left of one is nothing.
        const DONE: f64 = 1e-6;
        for _ in 0..4000 {
            if tasks.iter().all(|t| t.left <= DONE && t.flying.is_empty()) && owed.is_empty() {
                return Some(now);
            }
            now += step;
            while owed.front().is_some_and(|o| o.0 <= now) {
                if let Some((_, key, n)) = owed.pop_front() {
                    *stock.entry(key).or_insert(0.0) += n;
                }
            }
            let mut moved = false;
            let mut ready = false;
            for t in &mut tasks {
                while t.flying.front().is_some_and(|f| f.0 <= now) {
                    if let Some((_, n)) = t.flying.pop_front() {
                        for (key, each) in &t.outputs {
                            *stock.entry(key.clone()).or_insert(0.0) += each * n;
                        }
                        moved = true;
                    }
                }
            }
            // The CPU's allowance builds up over ticks: at most what it may push at once.
            ops = (ops + ops_per_tick * step).min(f64::from(self.co_processors + 1).max(ops_per_tick * step));
            for t in &mut tasks {
                t.credit = (t.credit + t.rate * step).min((t.rate * t.latency).max(1.0) + t.rate * step);
                if t.left <= DONE {
                    continue;
                }
                let mut can = t.left.min(t.credit).min(ops);
                for (keys, per) in &t.inputs {
                    let have: f64 = keys.iter().map(|k| stock.get(k).copied().unwrap_or(0.0)).sum();
                    can = can.min(have / per.max(1.0));
                }
                // It only waits for a machine or the CPU when its ingredients are there.
                ready |= t.inputs.iter().all(|(keys, per)| keys.iter().map(|k| stock.get(k).copied().unwrap_or(0.0)).sum::<f64>() >= *per);
                // Whole crafts while they are few; a long job's steps may split them.
                let n = if step <= 1.0 { can.floor() } else { can };
                if n <= 0.0 {
                    continue;
                }
                for (keys, per) in &t.inputs {
                    let mut need = per * n;
                    for k in keys {
                        let e = stock.entry(k.clone()).or_insert(0.0);
                        let take = need.min(*e);
                        *e -= take;
                        need -= take;
                    }
                }
                t.left -= n;
                t.credit -= n;
                ops -= n;
                t.flying.push_back((now + t.latency, n));
                moved = true;
            }
            if !moved && !ready && owed.is_empty() && tasks.iter().all(|t| t.flying.is_empty()) {
                // All done with this step's arrivals, or everything waits for what never comes.
                return tasks.iter().all(|t| t.left <= DONE).then_some(now);
            }
        }
        Some(now)
    }

    /// What the job holds and still has to push, for `/ae2debug eta`.
    pub fn debug_state(&self) -> String {
        let Some(job) = self.job.as_ref() else { return String::new() };
        let tasks: Vec<String> = job.tasks.iter().map(|(p, n)| format!("{}x{n}", p.primary_output().what.id())).collect();
        let held: Vec<String> = self.inventory.iter().map(|(k, n)| format!("{}x{n}", k.id())).collect();
        let owed: Vec<String> = job.waiting_for.iter().map(|(k, n)| format!("{}x{n}", k.id())).collect();
        format!("tasks {tasks:?} held {held:?} owed {owed:?}")
    }

    /// The patterns the job has yet to run.
    pub fn task_patterns(&self) -> Vec<Arc<Pattern>> {
        self.job.as_ref().map_or(Vec::new(), |j| j.tasks.iter().filter(|(_, n)| *n > 0).map(|(p, _)| p.clone()).collect())
    }

    /// What the job waits for now.
    pub fn waiting_keys(&self) -> Vec<AeKey> {
        self.job.as_ref().map_or(Vec::new(), |j| j.waiting_for.iter().filter(|(_, n)| *n > 0).map(|(k, _)| k.clone()).collect())
    }

    pub fn has_job(&self) -> bool {
        self.job.is_some()
    }

    pub fn job(&self) -> Option<&CraftingJob> {
        self.job.as_ref()
    }

    /// `getFinalJobOutput`, with what is still to come.
    pub fn final_output(&self) -> Option<GenericStack> {
        self.job.as_ref().map(|j| GenericStack { what: j.final_output.what.clone(), amount: j.remaining_amount })
    }

    pub fn stored(&self, what: &AeKey) -> i64 {
        self.inventory.get(what).max(0)
    }

    pub fn waiting_for(&self, what: &AeKey) -> i64 {
        self.job.as_ref().map_or(0, |j| j.waiting_for.get(what).max(0))
    }

    /// `getPendingOutputs`: what the remaining pushes will make.
    pub fn pending_outputs(&self, what: &AeKey) -> i64 {
        self.job.as_ref().map_or(0, |j| {
            j.tasks
                .iter()
                .flat_map(|(p, times)| p.outputs.iter().filter(|o| o.what == *what).map(move |o| o.amount * times))
                .sum()
        })
    }

    /// `getAllItems`: held, awaited and still to craft, for the CPU status screen.
    pub fn all_items(&self, out: &mut KeyCounter) {
        out.add_all(&self.inventory);
        if let Some(job) = &self.job {
            out.add_all(&job.waiting_for);
            for (pattern, times) in &job.tasks {
                for output in &pattern.outputs {
                    out.add(&output.what, output.amount * times);
                }
            }
        }
    }

    /// What changes whenever the job gets anywhere: results back and crafts started, what it waits for, the tasks left.
    pub fn progress_mark(&self) -> Option<(i64, i64, i64)> {
        let job = self.job.as_ref()?;
        Some((job.time.done_so_far(), job.waiting_for.iter().map(|(_, n)| n).sum(), job.tasks.iter().map(|(_, n)| *n).sum()))
    }

    pub fn cant_store_items(&self) -> bool {
        self.cant_store_items
    }

    /// Whether the CPU changed since the last call: the game side saves it then.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// The keys whose amounts in the CPU changed since the last call (`postChange`).
    pub fn take_changes(&mut self) -> HashSet<AeKey> {
        std::mem::take(&mut self.changed)
    }

    /// Jobs started, finished or cancelled since the last call, for the players to hear.
    pub fn take_events(&mut self) -> Vec<CpuEvent> {
        std::mem::take(&mut self.events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::port_ae2_crafting::{CalculationStrategy, PatternBook, PatternInput, calculate};

    fn item(id: &str) -> AeKey {
        AeKey::item(id, "", 0, 0)
    }

    fn pattern(name: &str, input: (&str, i64), output: (&str, i64)) -> Pattern {
        Pattern {
            definition: AeKey::item("ae2:processing_pattern", name, 0, 0),
            inputs: vec![PatternInput::of(item(input.0), input.1)],
            outputs: vec![GenericStack { what: item(output.0), amount: output.1 }],
        }
    }

    /// A network whose providers craft instantly: a push is answered on the next `deliver_all`.
    #[derive(Default)]
    struct TestNet {
        storage: KeyCounter,
        power: f64,
        providers: usize,
        pushed: Vec<(String, usize)>,
        in_flight: Vec<GenericStack>,
    }

    impl CpuNetwork for TestNet {
        fn extract(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
            list_extract(&mut self.storage, what, amount, mode)
        }
        fn insert(&mut self, what: &AeKey, amount: i64, mode: Actionable) -> i64 {
            if mode == Actionable::Modulate {
                self.storage.add(what, amount);
            }
            amount
        }
        fn provider_count(&self, _: &Pattern) -> usize {
            self.providers
        }
        fn provider_busy(&self, _: &Pattern, _: usize) -> bool {
            false
        }
        fn push_pattern(&mut self, pattern: &Pattern, provider: usize, _: &[KeyCounter]) -> bool {
            self.pushed.push((pattern.definition.components().to_string(), provider));
            self.in_flight.extend(pattern.outputs.iter().cloned());
            true
        }
        fn extract_power(&mut self, amount: f64, mode: Actionable) -> f64 {
            let got = amount.min(self.power);
            if mode == Actionable::Modulate {
                self.power -= got;
            }
            got
        }
    }

    /// The providers' results go back to the CPU; what it doesn't take goes to storage.
    fn deliver_all(cpu: &mut CraftingCpu, net: &mut TestNet) {
        for out in std::mem::take(&mut net.in_flight) {
            let taken = cpu.insert(&out.what, out.amount, Actionable::Modulate, net);
            if taken < out.amount {
                net.storage.add(&out.what, out.amount - taken);
            }
        }
    }

    fn setup(logs: i64) -> (PatternBook, TestNet) {
        let mut book = PatternBook::new();
        book.add(pattern("planks", ("minecraft:oak_log", 1), ("minecraft:oak_planks", 4)), 0);
        book.add(pattern("sticks", ("minecraft:oak_planks", 2), ("minecraft:stick", 4)), 0);
        let mut net = TestNet { power: 1000.0, providers: 1, ..Default::default() };
        net.storage.add(&item("minecraft:oak_log"), logs);
        (book, net)
    }

    fn run(cpu: &mut CraftingCpu, net: &mut TestNet, max_ticks: usize) -> usize {
        for t in 0..max_ticks {
            if !cpu.has_job() {
                return t;
            }
            cpu.tick(net);
            deliver_all(cpu, net);
        }
        max_ticks
    }

    #[test]
    fn a_job_runs_to_the_end() {
        let (book, mut net) = setup(2);
        let plan = calculate(&book, &net.storage, &item("minecraft:stick"), 8, CalculationStrategy::ReportMissingItems).unwrap();
        let mut cpu = CraftingCpu::new(1 << 20, 0);
        let id = cpu.submit(&plan, &mut net, false, Some(7)).unwrap();
        // The one log the plan uses moved into the CPU.
        assert_eq!(net.storage.get(&item("minecraft:oak_log")), 1);
        assert_eq!(cpu.stored(&item("minecraft:oak_log")), 1);
        assert!(run(&mut cpu, &mut net, 100) < 100);
        assert_eq!(net.storage.get(&item("minecraft:stick")), 8);
        assert!(cpu.inventory.is_empty());
        assert_eq!(
            cpu.take_events(),
            [CpuEvent::Started { craft_id: id, player: Some(7) }, CpuEvent::Finished { craft_id: id, player: Some(7) }]
        );
    }

    #[test]
    fn one_push_a_tick_spread_over_three() {
        let (book, mut net) = setup(64);
        let plan = calculate(&book, &net.storage, &item("minecraft:oak_planks"), 16, CalculationStrategy::ReportMissingItems).unwrap();
        let mut cpu = CraftingCpu::new(1 << 20, 0);
        cpu.submit(&plan, &mut net, false, None).unwrap();
        // With no co-processor: one push, then three ticks of rest.
        let pushes: Vec<usize> = (0..8)
            .map(|_| {
                let before = net.pushed.len();
                cpu.tick(&mut net);
                net.pushed.len() - before
            })
            .collect();
        assert_eq!(pushes, [1, 0, 0, 0, 1, 0, 0, 0]);
        // Co-processors push more at once, across providers in turn.
        let (_, mut net) = setup(64);
        net.providers = 2;
        let mut fast = CraftingCpu::new(1 << 20, 3);
        fast.submit(&plan, &mut net, false, None).unwrap();
        fast.tick(&mut net);
        assert_eq!(net.pushed.iter().map(|p| p.1).collect::<Vec<_>>(), [0, 1, 0, 1]);
        deliver_all(&mut fast, &mut net);
        assert!(!fast.has_job());
    }

    #[test]
    fn no_power_no_push() {
        let (book, mut net) = setup(2);
        net.power = 0.0;
        let plan = calculate(&book, &net.storage, &item("minecraft:oak_planks"), 4, CalculationStrategy::ReportMissingItems).unwrap();
        let mut cpu = CraftingCpu::new(1 << 20, 0);
        cpu.submit(&plan, &mut net, false, None).unwrap();
        cpu.tick(&mut net);
        assert!(net.pushed.is_empty());
        assert_eq!(cpu.stored(&item("minecraft:oak_log")), 1);
    }

    #[test]
    fn submit_checks_the_cpu_and_storage() {
        let (book, mut net) = setup(1);
        let plan = calculate(&book, &net.storage, &item("minecraft:oak_planks"), 4, CalculationStrategy::ReportMissingItems).unwrap();
        assert_eq!(CraftingCpu::new(1, 0).submit(&plan, &mut net, false, None), Err(SubmitError::CpuTooSmall));
        let mut busy = CraftingCpu::new(1 << 20, 0);
        busy.submit(&plan, &mut net, false, None).unwrap();
        assert_eq!(busy.submit(&plan, &mut net, false, None), Err(SubmitError::CpuBusy));
        // The log is in the first CPU now: a second job is short of it.
        let mut other = CraftingCpu::new(1 << 20, 0);
        assert_eq!(
            other.submit(&plan, &mut net, false, None),
            Err(SubmitError::MissingIngredient(GenericStack { what: item("minecraft:oak_log"), amount: 1 }))
        );
        let short = calculate(&book, &KeyCounter::new(), &item("minecraft:oak_planks"), 4, CalculationStrategy::ReportMissingItems).unwrap();
        assert_eq!(other.submit(&short, &mut net, false, None), Err(SubmitError::IncompletePlan));
    }

    #[test]
    fn cancelling_returns_the_ingredients() {
        let (book, mut net) = setup(2);
        let plan = calculate(&book, &net.storage, &item("minecraft:oak_planks"), 8, CalculationStrategy::ReportMissingItems).unwrap();
        let mut cpu = CraftingCpu::new(1 << 20, 0);
        cpu.submit(&plan, &mut net, false, None).unwrap();
        assert_eq!(cpu.pending_outputs(&item("minecraft:oak_planks")), 8);
        cpu.cancel(&mut net);
        assert!(!cpu.has_job());
        assert_eq!(net.storage.get(&item("minecraft:oak_log")), 2);
    }

    #[test]
    fn a_requester_gets_the_output() {
        struct Requested(TestNet, i64);
        impl CpuNetwork for Requested {
            fn extract(&mut self, w: &AeKey, a: i64, m: Actionable) -> i64 { self.0.extract(w, a, m) }
            fn insert(&mut self, w: &AeKey, a: i64, m: Actionable) -> i64 { self.0.insert(w, a, m) }
            fn provider_count(&self, p: &Pattern) -> usize { self.0.provider_count(p) }
            fn provider_busy(&self, p: &Pattern, i: usize) -> bool { self.0.provider_busy(p, i) }
            fn push_pattern(&mut self, p: &Pattern, i: usize, x: &[KeyCounter]) -> bool { self.0.push_pattern(p, i, x) }
            fn extract_power(&mut self, a: f64, m: Actionable) -> f64 { self.0.extract_power(a, m) }
            fn deliver(&mut self, _: &AeKey, amount: i64, mode: Actionable) -> i64 {
                if mode == Actionable::Modulate {
                    self.1 += amount;
                }
                amount
            }
        }
        let (book, net) = setup(1);
        let plan = calculate(&book, &net.storage, &item("minecraft:oak_planks"), 4, CalculationStrategy::ReportMissingItems).unwrap();
        let mut net = Requested(net, 0);
        let mut cpu = CraftingCpu::new(1 << 20, 0);
        cpu.submit(&plan, &mut net, true, None).unwrap();
        cpu.tick(&mut net);
        let outs = std::mem::take(&mut net.0.in_flight);
        for out in outs {
            assert_eq!(cpu.insert(&out.what, out.amount, Actionable::Modulate, &mut net), 4);
        }
        assert_eq!(net.1, 4);
        assert!(!cpu.has_job());
        assert_eq!(cpu.job().map(|j| j.time.progress()), None);
    }
}

/// What a CPU saves (`CraftingCpuLogic.writeToNBT`).
pub struct JobSave {
    pub craft_id: u128,
    pub final_output: GenericStack,
    pub remaining_amount: i64,
    pub has_requester: bool,
    pub player: Option<i32>,
    pub waiting_for: Vec<(AeKey, i64)>,
    pub tasks: Vec<(AeKey, i64)>,
    pub time: Vec<i64>,
}

pub struct CpuSave {
    pub job: Option<JobSave>,
    pub inventory: Vec<(AeKey, i64)>,
}

impl ElapsedTimeTracker {
    pub fn to_vec(&self) -> Vec<i64> {
        vec![self.elapsed_nanos() as i64, self.started[0], self.started[1], self.completed[0], self.completed[1]]
    }
}

impl CraftingCpu {
    pub fn save_state(&self) -> CpuSave {
        CpuSave {
            job: self.job.as_ref().map(|j| JobSave {
                craft_id: j.craft_id,
                final_output: j.final_output.clone(),
                remaining_amount: j.remaining_amount,
                has_requester: j.has_requester,
                player: j.player,
                waiting_for: j.waiting_for.iter().map(|(k, n)| (k.clone(), n)).collect(),
                tasks: j.tasks.iter().map(|(p, n)| (p.definition.clone(), *n)).collect(),
                time: j.time.to_vec(),
            }),
            inventory: self.inventory.iter().map(|(k, n)| (k.clone(), n)).collect(),
        }
    }

    /// Back as saved; a pattern that no longer decodes is left out, as `ExecutingCraftingJob` leaves it.
    pub fn restore_state(&mut self, save: CpuSave, decode: &mut dyn FnMut(&AeKey) -> Option<Arc<Pattern>>) {
        for (k, n) in &save.inventory {
            self.inventory.add(k, *n);
        }
        if let Some(s) = save.job {
            let mut waiting_for = KeyCounter::new();
            for (k, n) in &s.waiting_for {
                waiting_for.add(k, *n);
            }
            let mut time = ElapsedTimeTracker::default();
            if let [elapsed, s0, s1, c0, c1] = s.time[..] {
                time.elapsed_nanos = elapsed as u64;
                time.started = [s0, s1];
                time.completed = [c0, c1];
            }
            self.job = Some(CraftingJob {
                craft_id: s.craft_id,
                final_output: s.final_output,
                remaining_amount: s.remaining_amount,
                waiting_for,
                tasks: s.tasks.iter().filter_map(|(k, n)| Some((decode(k)?, *n))).collect(),
                time,
                has_requester: s.has_requester,
                player: s.player,
            });
        }
    }
}
