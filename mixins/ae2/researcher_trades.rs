// InitVillager.init, as 26.3's trade_set and villager_trade files. Villager.updateTrades takes two of a level's trades.
fn ae2_researcher_trades(files: &mut Vec<(String, Vec<u8>)>) {
    // buyItems: VillagerTrades.ItemsForEmeralds(item, emeralds, count, xp), whose maxUses is 12.
    const BUY: [(usize, &str, u32, u32, u32); 9] = [
        (1, "ae2:certus_quartz_crystal", 3, 4, 10),
        (1, "ae2:meteorite_compass", 2, 1, 5),
        (2, "ae2:sky_stone_block", 5, 8, 20),
        (3, "ae2:fluix_crystal", 5, 4, 14),
        (4, "ae2:calculation_processor_press", 10, 1, 20),
        (4, "ae2:engineering_processor_press", 10, 1, 20),
        (4, "ae2:logic_processor_press", 10, 1, 20),
        (4, "ae2:silicon_press", 10, 1, 20),
        (5, "minecraft:slime_ball", 8, 5, 12),
    ];
    // sellItems: VillagerTrades.EmeraldForItems(item, count, maxUses, xp), for one emerald.
    const SELL: [(usize, &str, u32, u32, u32); 4] = [
        (2, "ae2:charged_certus_quartz_crystal", 3, 10, 15),
        (2, "ae2:silicon", 5, 8, 13),
        (3, "ae2:quartz_glass", 2, 10, 10),
        (4, "ae2:matter_ball", 5, 8, 12),
    ];
    let mut levels: [Vec<String>; 5] = Default::default();
    let mut add = |level: usize, item: &str, trade: serde_json::Value| {
        let path = item.rsplit(':').next().unwrap_or(item);
        let id = format!("ae2:fluix_researcher/{level}/{path}");
        files.push((format!("villager_trade/{id}"), serde_json::to_vec(&trade).unwrap_or_default()));
        levels[level - 1].push(id);
    };
    for (level, item, emeralds, count, xp) in BUY {
        add(level, item, serde_json::json!({
            "wants": { "id": "minecraft:emerald", "count": emeralds },
            "gives": { "id": item, "count": count },
            "max_uses": 12,
            "xp": xp,
            "reputation_discount": 0.05,
        }));
    }
    for (level, item, count, max_uses, xp) in SELL {
        add(level, item, serde_json::json!({
            "wants": { "id": item, "count": count },
            "gives": { "id": "minecraft:emerald", "count": 1 },
            "max_uses": max_uses,
            "xp": xp,
            "reputation_discount": 0.05,
        }));
    }
    for (i, trades) in levels.iter().enumerate() {
        let level = i + 1;
        let set = serde_json::json!({
            "amount": 2,
            "random_sequence": format!("ae2:trade_set/fluix_researcher/level_{level}"),
            "trades": trades,
        });
        files.push((format!("trade_set/ae2:fluix_researcher/level_{level}"), serde_json::to_vec(&set).unwrap_or_default()));
    }
}
