ae2_optional::if_mod_items!("jei", {
// What the integration asks of JEI's panel.
use super::Game;

/// A batch of the window's size, to ask where things are.
pub(crate) fn batch(game: &Game<'_>) -> Option<horizon_client::hud::HudBatch> {
    game.browser_batch()
}

/// JEI's own viewer has put a recipe page on the panel: the recipes show in the window, the panel keeps its items.
pub(crate) fn has_recipes(game: &Game<'_>) -> bool {
    game.panel.recipes.is_some()
}

/// `mod_id`'s items on the panel and the text searched, when they have changed since version `seen`. Nothing is copied
/// until they have.
pub(crate) fn changed_items(game: &Game<'_>, mod_id: &str, seen: u64) -> Option<(u64, String, Vec<crate::game::native_hooks::NativeStack>)> {
    let shown = game.panel.native.as_ref()?;
    if shown.version == seen || shown.owner.native_id() != mod_id {
        return None;
    }
    Some((shown.version, game.panel.sent.clone(), shown.items.clone()))
}

/// The panel's last change.
pub(crate) fn items_version(game: &Game<'_>) -> u64 {
    game.panel.native.as_ref().map_or(0, |shown| shown.version)
}
});
