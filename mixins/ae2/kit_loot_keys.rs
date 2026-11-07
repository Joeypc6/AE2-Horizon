// The ids of the loot tables, for JEI's sources.
use super::LootTables;

impl LootTables {
    pub fn hz_kit_ids_ae2(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }
}
