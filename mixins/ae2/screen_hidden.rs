// The open screen's layout put away, and put back: AE2's crafting status, amount and plan screens are menus of their
// own, with nothing of the terminal behind them.
use super::Game;

thread_local! {
    static SAVED: std::cell::RefCell<Option<(i32, Vec<Option<[f32; 2]>>, [f32; 4], bool)>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn set_hidden(game: &mut Game<'_>, hidden: bool) {
    let Some(open) = game.mod_screen.as_mut() else {
        SAVED.with_borrow_mut(|s| *s = None);
        return;
    };
    SAVED.with_borrow_mut(|saved| {
        if saved.as_ref().is_some_and(|s| s.0 != open.window) {
            *saved = None;
        }
        let screen = &mut open.screen;
        if hidden && saved.is_none() {
            *saved = Some((
                open.window,
                screen.slots.clone(),
                [screen.width, screen.height, screen.inventory_x, screen.inventory_y],
                screen.own_labels,
            ));
            for slot in screen.slots.iter_mut().flatten() {
                *slot = [-10_000.0, -10_000.0];
            }
            screen.width = 2.0;
            screen.height = 2.0;
            screen.inventory_x = -10_000.0;
            screen.inventory_y = -10_000.0;
            screen.own_labels = true;
        } else if !hidden && let Some((_, slots, [w, h, x, y], labels)) = saved.take() {
            screen.slots = slots;
            screen.width = w;
            screen.height = h;
            screen.inventory_x = x;
            screen.inventory_y = y;
            screen.own_labels = labels;
        }
    });
}

/// The screen laid out again by its block's mod (a terminal's new style).
pub(crate) fn relayout(game: &mut Game<'_>) {
    let Some(block) = game.mod_screen.as_ref().map(|s| s.block.clone()) else { return };
    let mut screen = None;
    game.native_screen(&block, &mut screen);
    if let (Some(screen), Some(open)) = (screen, game.mod_screen.as_mut()) {
        open.screen = screen;
    }
}
