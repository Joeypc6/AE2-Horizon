ae2_optional::if_mod_items!("jade", {
// AE2's Jade plugin: AE2's own lines (waila.ae2.*) for its blocks, from the server's network state.
use crate::game::Game;
use crate::game::jade::{Line, Target};

/// TooltipIds, as `config.jade.plugin_ae2.*` names them.
const OPTIONS: [(&str, &str); 7] = [
    ("plugin_ae2.grid_node_state", "AE2 Network State"),
    ("plugin_ae2.power_storage", "AE2 Power State"),
    ("plugin_ae2.crafting_monitor", "AE2 Crafting Monitor"),
    ("plugin_ae2.pattern_provider", "AE2 Pattern Provider"),
    ("plugin_ae2.charger", "AE2 Charger"),
    ("plugin_ae2.part_tooltip", "AE2 Part Tooltip"),
    ("plugin_ae2.debug", "AE2 Debug Info"),
];

pub fn register() {
    for (key, label) in OPTIONS {
        crate::game::jade::register_option(key, label, true);
    }
    crate::game::jade::register_server(server_lines);
    crate::game::jade::register_client(client_lines);
    crate::game::jade::register_name(part_host_name);
}

/// `PartHostTooltips.getName` / `getIcon` / `getModName`: a cable bus shows the facade or part under the crosshair.
fn part_host_name(game: &Game<'_>, pos: [i32; 3], block: &str, hit: [f64; 3]) -> Option<(String, String, String)> {
    crate::game::port_ae2_me::part_host_name(game, pos, block, hit)
}

/// `GridNodeStateProvider`, `ChannelDataProvider`, `P2PStateDataProvider`, `ChargerDataProvider`,
/// `CraftingMonitorDataProvider`, `PowerStorageDataProvider`: what AE2 knows about the block.
fn server_lines(game: &mut Game<'_>, pos: [i32; 3], block: &str, lines: &mut Vec<String>) {
    if !block.starts_with("ae2:") {
        return;
    }
    let off = OPTIONS.iter().map(|(k, _)| *k).filter(|k| !crate::game::jade::plugin_on(game, k)).collect();
    // A cell of parts: the part looked at.
    let pos = crate::game::jade::request_hit().and_then(|hit| crate::game::port_ae2_me::part_under(game, pos, hit)).unwrap_or(pos);
    lines.extend(crate::game::port_ae2_me::describe_without(game, pos, off));
}

/// A cell in the hand: nothing extra (its tooltip has it); kept for parts drawn on the client.
fn client_lines(_: &Game<'_>, _: &Target, _: &mut Vec<Line>) {}
});
