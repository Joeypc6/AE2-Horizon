ae2_optional::if_mod_items!("jei", {
// JEI's panel on the creative screen. The game draws it on inventory screens only, so around each of its calls it is
// told that one is open, as wide as the creative window.
use crate::game::Game;
use horizon_client::hud::HudBatch;
use std::cell::{Cell, RefCell};

/// The creative window's width (`CreativeModeInventoryScreen.imageWidth`).
const CREATIVE_WIDTH: f32 = 195.0;

#[derive(Default)]
struct Queue {
    press: Option<([f32; 2], bool)>,
    unfocus: bool,
    typed: String,
    backspaces: u32,
    keys: Vec<(String, bool)>,
    typing: bool,
    was_open: bool,
}

thread_local! {
    static QUEUE: RefCell<Queue> = RefCell::default();
    static SWAPPED: Cell<bool> = const { Cell::new(false) };
}

fn open(game: &Game<'_>) -> bool {
    game.screen == crate::game::Screen::Creative && game.native_on("jei") && !crate::game::port_ae2_guide::is_open()
}

fn left(game: &Game<'_>) -> f32 {
    let (w, _) = game.gui_size();
    ((w - CREATIVE_WIDTH) / 2.0).floor() + CREATIVE_WIDTH + 8.0
}

/// `native_screen_width`, while the panel works for the creative screen.
pub fn screen_width() -> Option<f32> {
    SWAPPED.get().then_some(CREATIVE_WIDTH)
}

fn with_panel<R>(game: &mut Game<'_>, f: impl FnOnce(&mut Game<'_>) -> R) -> R {
    let screen = game.screen;
    game.screen = crate::game::Screen::Inventory;
    SWAPPED.set(true);
    let out = f(game);
    SWAPPED.set(false);
    game.screen = screen;
    out
}

pub fn draw(game: &mut Game<'_>, batch: &mut HudBatch) {
    if !open(game) {
        return;
    }
    // Off the panel it has nothing to show (and the inventory screen it is told of has other slots there).
    let cursor = game.cursor;
    if cursor.0 < left(game) {
        game.cursor = (-10000.0, -10000.0);
    }
    with_panel(game, |g| g.draw_mod_browser(batch));
    game.cursor = cursor;
    let typing = with_panel(game, |g| g.mod_browser_typing());
    QUEUE.with_borrow_mut(|q| q.typing = typing);
}

/// A click on the creative screen: on the panel it is the panel's; anywhere else takes the focus off its search box.
pub fn press(game: &Game<'_>, mouse: [f32; 2], button: i32, canceled: &mut bool) {
    if !open(game) {
        return;
    }
    if mouse[0] < left(game) {
        QUEUE.with_borrow_mut(|q| q.unfocus = true);
        return;
    }
    *canceled = true;
    if button == 0 || button == 1 {
        QUEUE.with_borrow_mut(|q| q.press = Some((mouse, button == 1)));
    }
}

/// A key on the creative screen: the panel's search box takes it, or it looks up the item under the pointer.
pub fn key(game: &Game<'_>, mouse: [f32; 2], key: &str, canceled: &mut bool) {
    if !open(game) {
        return;
    }
    if QUEUE.with_borrow(|q| q.typing) {
        if key == "key.keyboard.backspace" {
            QUEUE.with_borrow_mut(|q| q.backspaces += 1);
        }
        if key != "key.keyboard.escape" {
            *canceled = true;
        }
        return;
    }
    let (recipe, uses) = (game.native_mod_key("jei", "recipe", "key.keyboard.r"), game.native_mod_key("jei", "uses", "key.keyboard.u"));
    if key == recipe || key == uses {
        // Not typed into the creative search box (its tab's own search takes keys first).
        let search = game.creative_screen.tab().search;
        let panel = mouse[0] >= left(game);
        if panel || !search {
            *canceled = true;
            QUEUE.with_borrow_mut(|q| q.keys.push((key.to_owned(), panel)));
        }
    }
}

pub fn char_typed(c: char, canceled: &mut bool) {
    QUEUE.with_borrow_mut(|q| {
        if q.typing && !c.is_control() {
            q.typed.push(c);
            *canceled = true;
        }
    });
}

/// The stack under the pointer on the creative screen: its grid's, by the row it is scrolled to, or a slot of the player's.
fn pointed(game: &mut Game<'_>) -> Option<crate::game::native_hooks::NativeStack> {
    use horizon_client::creative::Creative;
    let batch = crate::game::native_panel::port_jei_panel_size::batch(game)?;
    let slot = game.slot_at(&batch, game.cursor.0, game.cursor.1)?;
    let stack = if slot >= Creative::DISPLAY {
        let names = &game.pack.names;
        let i = slot - Creative::DISPLAY;
        let first = game.creative_screen.first_row(names);
        game.creative_screen.items(names).get((first + i / 9) * 9 + i % 9).cloned()
    } else {
        game.slot_view(slot)
    }?;
    Some(crate::game::native_hooks::NativeStack { item: stack.item.to_string(), count: 1, components: String::new() })
}

/// What the frame has to do for the panel, with the game to hand.
pub fn frame(game: &mut Game<'_>) {
    let is_open = open(game);
    let (press, unfocus, typed, backspaces, keys, was_open) = QUEUE.with_borrow_mut(|q| {
        let was = std::mem::replace(&mut q.was_open, is_open);
        (q.press.take(), std::mem::take(&mut q.unfocus), std::mem::take(&mut q.typed), std::mem::take(&mut q.backspaces), std::mem::take(&mut q.keys), was)
    });
    if was_open && !is_open {
        with_panel(game, |g| g.mod_browser_closed());
        QUEUE.with_borrow_mut(|q| q.typing = false);
    }
    if !is_open {
        return;
    }
    if unfocus {
        with_panel(game, |g| g.mod_browser_closed());
    }
    if let Some((at, right)) = press {
        let cursor = game.cursor;
        game.cursor = (at[0], at[1]);
        with_panel(game, |g| g.mod_browser_press(right));
        game.cursor = cursor;
    }
    if !typed.is_empty() {
        with_panel(game, |g| g.mod_browser_text(Some(&typed), false));
    }
    for _ in 0..backspaces {
        with_panel(game, |g| g.mod_browser_text(None, true));
    }
    for (key, panel) in keys {
        if panel {
            with_panel(game, |g| g.native_panel_key(&key));
        } else if let Some(stack) = pointed(game) {
            // A slot of the creative screen's own.
            game.native_panel_event("jei", &crate::game::native_hooks::NativePanelEvent::Key { key, stack });
        }
    }
}
});
