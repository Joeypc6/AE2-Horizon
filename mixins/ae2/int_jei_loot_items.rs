// The loot tables an `Items` holds, for JEI's sources.
use super::Items;

impl Items {
    pub fn hz_jei_tables(&self) -> std::sync::Arc<horizon_world::loot::LootTables> {
        self.loot.clone()
    }
}
