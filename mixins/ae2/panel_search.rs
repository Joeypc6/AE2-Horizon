// The search box of the item panel a mod shows, for AE2's terminal settings.
use super::{BrowserEvent, Game};

pub(crate) fn search_text(game: &Game<'_>) -> String {
    game.panel.search.as_ref().map_or_else(String::new, |s| s.value.clone())
}

pub(crate) fn set_search(game: &mut Game<'_>, text: &str) {
    let Some(search) = game.panel.search.as_mut() else { return };
    if search.value == text {
        return;
    }
    search.value = text.to_owned();
    if let Some(owner) = game.browser_sync() {
        game.panel_event(&owner, BrowserEvent::Search(text.to_owned()));
    }
}
