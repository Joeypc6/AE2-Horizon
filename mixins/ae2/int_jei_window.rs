ae2_optional::if_mod_items!("jei", {
// JEI's recipe window: a centred window over the screen, not the panel's column. Tabs along the top, the blocks each
// category is made on down the left, a title and a page bar, and the recipes on cards. The layer (port_ae2_jei) fills
// it; this draws it and turns what the player does on it into `Act`s the layer takes on the next frame.
use crate::game::Game;
use crate::game::jei::{Layout, Stack, View};
use crate::game::native_hooks::{NativeScreenView, NativeStack};
use horizon_client::hud::{HudBatch, rgba};
use horizon_client::mobs::Mob;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

pub const WIDTH: f32 = 192.0;
const SLOT: f32 = 18.0;
const TAB: f32 = 24.0;
const MOB_AREA: f32 = 54.0;
/// The width of a card's text.
pub const TEXT_WIDTH: f32 = WIDTH - 20.0;
const CARD_X: f32 = 6.0;
const CARDS_TOP: f32 = 38.0;

pub const WORLD_ICONS: [&str; 3] = ["minecraft:grass_block", "minecraft:netherrack", "minecraft:end_stone"];

/// A category: its tab's icon, its title, the blocks it is made on, its cards.
pub struct Tab {
    pub icon: String,
    pub title: String,
    pub catalysts: Vec<String>,
    pub cards: Vec<Card>,
}

pub struct MobDrop {
    pub item: String,
    pub count: u32,
    /// "100%, x0-2, more with Looting".
    pub line: String,
}

pub enum Card {
    /// A recipe as JEI lays it out, with its "+" when it has one.
    Recipe(View),
    /// Something that gives an item: its slot, an arrow, the item, and lines under them.
    Source { from: Vec<Stack>, item: Stack, lines: Vec<String> },
    Text(Vec<String>),
    /// Where a block generates in one world: the world's icon and name, and its lines.
    World { dimension: usize, name: String, lines: Vec<String> },
    /// A mob turning in its frame, what it says of itself, and all it drops.
    Mob { mob: String, name: String, info: Vec<String>, drops: Vec<MobDrop>, wanted: String },
    /// A villager's trade.
    Trade { who: String, station: Option<String>, level: Option<u32>, profession: Option<u8>, mob: String, wants: Vec<Stack>, gives: Stack },
    /// An ore's height graph in one world: the share of blocks at each Y from the world's bottom.
    Ore { dimension: usize, name: String, block: String, scope: String, bottom: i32, share: Arc<Vec<f32>>, summary: Vec<String>, drops: Vec<String> },
}

impl Card {
    fn height(&self) -> f32 {
        let lines = |n: usize| n as f32 * 10.0;
        match self {
            Card::Recipe(v) => {
                let block = match v.layout {
                    Layout::Grid => v.height.max(1) as f32 * SLOT,
                    Layout::Cooking => 2.0 * SLOT,
                    _ => SLOT,
                };
                let note = if v.note.is_empty() { 0.0 } else { 1.0 + lines(v.note.lines().count()) };
                block.max(26.0) + note + 10.0
            }
            Card::Source { lines: l, .. } => 26.0 + lines(l.len()) + 10.0,
            Card::Text(l) => lines(l.len()) + 8.0,
            Card::World { lines: l, .. } => 22.0 + lines(l.len()) + 6.0,
            Card::Mob { info, .. } => 6.0 + lines(info.len() + 1) + MOB_AREA + 6.0,
            Card::Trade { station, .. } => 6.0 + lines(1) + if station.is_some() { 18.0 } else { 0.0 } + MOB_AREA + 6.0,
            Card::Ore { drops, summary, .. } => 24.0 + 40.0 + 12.0 + lines(summary.len()) + if drops.is_empty() { 0.0 } else { 20.0 } + 2.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Tab(usize),
    NextTab(i32),
    Page(i32),
    /// A left click (its recipes) or a right click (its uses) on a slot.
    Slot { stack: Stack, uses: bool },
    Plus(usize),
    Close,
    Back,
    /// A key over a slot.
    Key { key: String, stack: Stack },
    /// JEI's clickable area on a screen: the category this item is made on.
    Category(String),
}

/// What a recipe's "+" says on an AE2 terminal (`IRecipeTransferError`): whether it can be pressed, the tint over it
/// (`getButtonHighlightColor`), and while the pointer is on it the tooltip and the slots marked (`showError`).
pub struct Transfer {
    pub enabled: bool,
    pub tint: Option<u32>,
    pub lines: Vec<(String, u32)>,
    pub marks: Vec<(usize, u32)>,
}

pub struct Window {
    pub title: String,
    pub tabs: Vec<Tab>,
    pub tab: usize,
    pub page: usize,
    /// What the left mouse button pressed on a mob turns.
    since: std::time::Instant,
}

#[derive(Default)]
struct Hits {
    slots: Vec<([f32; 4], Stack, Vec<String>)>,
    buttons: Vec<([f32; 4], Act)>,
    mobs: Vec<([f32; 4], String)>,
    graphs: Vec<([f32; 4], i32, Arc<Vec<f32>>)>,
    /// The tooltip of the "+" under the pointer.
    tip: Option<Vec<(String, u32)>>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
    static HITS: RefCell<Hits> = RefCell::default();
    static ACTS: RefCell<Vec<Act>> = const { RefCell::new(Vec::new()) };
    /// Each mob as dragged: yaw and pitch.
    static TURNED: RefCell<HashMap<String, (f32, f32)>> = RefCell::new(HashMap::new());
    static MODELS: RefCell<HashMap<String, Mob>> = RefCell::new(HashMap::new());
    static DRAGGING: RefCell<Option<String>> = const { RefCell::new(None) };
    static LAST: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
    /// This frame's screen has stepped back already.
    static TINTED: Cell<bool> = const { Cell::new(false) };
    /// The real pointer, while the screen under the window is told it is elsewhere.
    static HIDDEN_CURSOR: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
}

pub fn is_open() -> bool {
    WINDOW.with_borrow(|w| w.is_some())
}

/// Shows a page, keeping the card page when it is the same one.
pub fn show(title: String, tabs: Vec<Tab>, tab: usize) {
    WINDOW.with_borrow_mut(|w| {
        let page = w.as_ref().filter(|o| o.title == title && o.tab == tab).map_or(0, |o| o.page);
        let since = w.as_ref().map_or_else(std::time::Instant::now, |o| o.since);
        *w = Some(Window { title, tabs, tab, page, since });
    });
}

pub fn close() {
    WINDOW.with_borrow_mut(|w| *w = None);
    DRAGGING.with_borrow_mut(|d| *d = None);
}

pub fn tab_count() -> usize {
    WINDOW.with_borrow(|w| w.as_ref().map_or(0, |w| w.tabs.len()))
}

pub fn take_acts() -> Vec<Act> {
    ACTS.with_borrow_mut(std::mem::take)
}

/// The recipe at `index` of the shown tab, for its "+".
pub fn recipe_view(index: usize) -> Option<View> {
    WINDOW.with_borrow(|w| match w.as_ref()?.tabs.get(w.as_ref()?.tab)?.cards.get(index)? {
        Card::Recipe(v) => Some(v.clone()),
        _ => None,
    })
}

/// The icon of the tab shown.
pub fn shown_icon() -> Option<String> {
    WINDOW.with_borrow(|w| {
        let w = w.as_ref()?;
        w.tabs.get(w.tab).map(|t| t.icon.clone())
    })
}

/// A page turn of the window's own; the layer doesn't need to hear it.
pub fn turn_page(by: i32, area: f32) {
    WINDOW.with_borrow_mut(|w| {
        let Some(w) = w.as_mut() else { return };
        let Some(tab) = w.tabs.get(w.tab) else { return };
        let n = pages(tab, area).len() as i32;
        w.page = (w.page as i32 + by).rem_euclid(n.max(1)) as usize;
    });
}

fn geometry(game: &Game<'_>) -> (f32, f32, f32) {
    let (gw, gh) = game.gui_size();
    let height = (gh - 34.0).clamp(150.0, 250.0);
    let x = ((gw - WIDTH) / 2.0).floor();
    let y = ((gh - height) / 2.0).floor() + 12.0;
    (x, y, height)
}

/// The area of the window's cards.
pub fn cards_area(game: &Game<'_>) -> f32 {
    geometry(game).2 - CARDS_TOP - 6.0
}

/// The first card of each page.
fn pages(tab: &Tab, area: f32) -> Vec<usize> {
    let mut starts = vec![0];
    let mut used = 0.0;
    for (i, c) in tab.cards.iter().enumerate() {
        let h = c.height() + 2.0;
        if used > 0.0 && used + h > area {
            starts.push(i);
            used = 0.0;
        }
        used += h;
    }
    starts
}

fn inside(p: (f32, f32), r: [f32; 4]) -> bool {
    p.0 >= r[0] && p.1 >= r[1] && p.0 < r[0] + r[2] && p.1 < r[1] + r[3]
}

fn native(stack: &Stack) -> NativeStack {
    NativeStack { item: stack.item.clone(), count: stack.count, components: stack.components.clone() }
}

/// `hud`'s start: over the window, the screen under it has no pointer (no highlights, no tooltips).
pub fn hud_before(game: &mut Game<'_>) {
    TINTED.set(false);
    if !is_open() {
        return;
    }
    let (x, _, _) = geometry(game);
    if game.cursor.0 < x + WIDTH + 2.0 {
        HIDDEN_CURSOR.set(Some(game.cursor));
        game.cursor = (-10000.0, -10000.0);
    }
}

pub fn hud_after(game: &mut Game<'_>, batch: &mut HudBatch) {
    if let Some(cursor) = HIDDEN_CURSOR.take() {
        game.cursor = cursor;
    }
    if is_open() && !crate::game::port_ae2_guide::is_open() {
        tint(game, batch);
    }
    crate::game::port_jei_creative::draw(game, batch);
    if is_open() && !crate::game::port_ae2_guide::is_open() {
        draw(game, batch);
    }
}

/// With the window open the screen steps back, all of it: called before the panel draws, so its items stay bright.
pub fn tint(game: &Game<'_>, batch: &mut HudBatch) {
    if is_open() && !TINTED.replace(true) {
        batch.fill(&game.gui.atlas, 0.0, 0.0, batch.width, batch.height, rgba(16, 16, 16, 200));
    }
}

fn bevel(game: &Game<'_>, batch: &mut HudBatch, [x, y, w, h]: [f32; 4], fill: u32, dark: u32, light: u32) {
    let atlas = &game.gui.atlas;
    batch.fill(atlas, x, y, w, h, fill);
    batch.fill(atlas, x, y, w, 1.0, dark);
    batch.fill(atlas, x, y, 1.0, h, dark);
    batch.fill(atlas, x, y + h - 1.0, w, 1.0, light);
    batch.fill(atlas, x + w - 1.0, y, 1.0, h, light);
}

fn button(game: &Game<'_>, batch: &mut HudBatch, rect: [f32; 4], label: &str, hits: &mut Hits, act: Act, id: &str) {
    let hover = inside(game.cursor, rect);
    let atlas = &game.gui.atlas;
    let sprite = if hover { "gui/sprites/widget/button_highlighted" } else { "gui/sprites/widget/button" };
    horizon_client::ui::nine_slice(batch, atlas, sprite, rect[0], rect[1], rect[2], rect[3], u32::MAX);
    let w = HudBatch::text_width(atlas, label);
    batch.text(atlas, label, (rect[0] + (rect[2] - w) / 2.0).floor(), rect[1] + ((rect[3] - 8.0) / 2.0).floor(), u32::MAX, true);
    horizon_client::widgets::record(id, rect, true);
    hits.buttons.push((rect, act));
}

/// A slot and what it holds now (its stacks turn once a second), with what its tooltip adds.
fn slot(game: &Game<'_>, batch: &mut HudBatch, x: f32, y: f32, stacks: &[Stack], extra: &[String], seconds: u64, hits: &mut Hits) {
    let atlas = &game.gui.atlas;
    batch.sprite(atlas, "gui/sprites/container/slot", x, y);
    if stacks.is_empty() {
        return;
    }
    let shown = &stacks[seconds as usize % stacks.len()];
    let Some(stack) = crate::game::native_panel::browser_stack(game.res.items(), &native(shown)) else { return };
    game.draw_item(batch, &stack, x + 1.0, y + 1.0);
    let rect = [x, y, SLOT, SLOT];
    if inside(game.cursor, rect) {
        batch.fill(&game.gui.atlas, x + 1.0, y + 1.0, 16.0, 16.0, rgba(255, 255, 255, 128));
    }
    let name = game.pack.names.display_item(&stack.item);
    horizon_client::widgets::record_as(&format!("{name} {}", hits.slots.len()), [x + 1.0, y + 1.0, 16.0, 16.0], true, horizon_client::widgets::Kind::Button);
    hits.slots.push((rect, shown.clone(), extra.to_vec()));
}

/// JEI's grey arrow.
fn arrow(game: &Game<'_>, batch: &mut HudBatch, x: f32, y: f32) {
    let atlas = &game.gui.atlas;
    let grey = rgba(139, 139, 139, 255);
    batch.fill(atlas, x, y + 5.0, 13.0, 4.0, grey);
    for i in 0..7 {
        let f = i as f32;
        batch.fill(atlas, x + 13.0 + f, y + f, 1.0, 14.0 - 2.0 * f, grey);
    }
}

const TEXT: u32 = 0xFF40_4040;

fn draw(game: &mut Game<'_>, batch: &mut HudBatch) {
    let (x0, y0, height) = geometry(game);
    let area = cards_area(game);
    let mut hits = Hits::default();
    let mut hover_lines: Option<Vec<(horizon_client::text::Text, u32)>> = None;
    WINDOW.with_borrow_mut(|window| {
        let Some(w) = window.as_mut() else { return };
        let seconds = w.since.elapsed().as_secs();
        let selected = w.tab.min(w.tabs.len().saturating_sub(1));
        w.tab = selected;
        // The tabs, those around the selected one when they don't all fit.
        let fit = ((WIDTH / (TAB + 2.0)).floor() as usize).max(1);
        let first = if w.tabs.len() <= fit { 0 } else { selected.saturating_sub(fit - 1) };
        for (i, tab) in w.tabs.iter().enumerate().skip(first).take(fit) {
            let tx = x0 + (i - first) as f32 * (TAB + 2.0);
            let rect = [tx, y0 - TAB + 2.0, TAB, TAB];
            let on = i == selected;
            let fill = if on { rgba(198, 198, 198, 255) } else { rgba(139, 139, 139, 255) };
            bevel(game, batch, rect, fill, rgba(255, 255, 255, 255), rgba(85, 85, 85, 255));
            if let Some(stack) = crate::game::native_panel::browser_stack(game.res.items(), &NativeStack { item: tab.icon.clone(), count: 1, components: String::new() }) {
                game.draw_item(batch, &stack, tx + 4.0, y0 - TAB + 6.0);
            }
            horizon_client::widgets::record_as(&format!("{} tab", tab.title), rect, true, horizon_client::widgets::Kind::Tab(on));
            hits.buttons.push((rect, Act::Tab(i)));
            if inside(game.cursor, rect) {
                hover_lines = Some(vec![(horizon_client::text::Text::literal(&tab.title), u32::MAX)]);
            }
        }
        // The window.
        bevel(game, batch, [x0, y0, WIDTH, height], rgba(198, 198, 198, 255), rgba(255, 255, 255, 255), rgba(85, 85, 85, 255));
        let Some(tab) = w.tabs.get(selected) else { return };
        // The blocks it is made on.
        if !tab.catalysts.is_empty() {
            let ch = tab.catalysts.len() as f32 * SLOT + 6.0;
            bevel(game, batch, [x0 - 24.0, y0 + 2.0, 24.0, ch], rgba(198, 198, 198, 255), rgba(255, 255, 255, 255), rgba(85, 85, 85, 255));
            for (i, c) in tab.catalysts.iter().enumerate() {
                slot(game, batch, x0 - 21.0, y0 + 5.0 + i as f32 * SLOT, &[Stack::item(c)], &[], 0, &mut hits);
            }
        }
        // The title bar and the page bar.
        let atlas = &game.gui.atlas;
        let bar = |batch: &mut HudBatch, y: f32, text: &str| {
            batch.fill(atlas, x0 + 18.0, y, WIDTH - 36.0, 14.0, rgba(139, 139, 139, 255));
            let w = HudBatch::text_width(atlas, text);
            batch.text(atlas, text, (x0 + (WIDTH - w) / 2.0).floor(), y + 3.0, u32::MAX, true);
        };
        bar(batch, y0 + 5.0, &tab.title);
        button(game, batch, [x0 + 4.0, y0 + 5.0, 13.0, 14.0], "<", &mut hits, Act::NextTab(-1), "previous-category");
        button(game, batch, [x0 + WIDTH - 17.0, y0 + 5.0, 13.0, 14.0], ">", &mut hits, Act::NextTab(1), "next-category");
        let starts = pages(tab, area);
        let page = w.page.min(starts.len() - 1);
        w.page = page;
        bar(batch, y0 + 21.0, &format!("{}/{}", page + 1, starts.len()));
        button(game, batch, [x0 + 4.0, y0 + 21.0, 13.0, 14.0], "<", &mut hits, Act::Page(-1), "previous-page");
        button(game, batch, [x0 + WIDTH - 17.0, y0 + 21.0, 13.0, 14.0], ">", &mut hits, Act::Page(1), "next-page");
        // The cards.
        let end = starts.get(page + 1).copied().unwrap_or(tab.cards.len());
        let mut y = y0 + CARDS_TOP;
        for i in starts[page]..end {
            let card = &tab.cards[i];
            let h = card.height();
            draw_card(game, batch, card, i, x0 + CARD_X, y, h, seconds, &mut hits);
            y += h + 2.0;
        }
    });
    let cursor = game.cursor;
    if let Some(lines) = hits.slots.iter().find(|h| inside(cursor, h.0)).and_then(|(_, stack, extra)| {
        let stack = crate::game::native_panel::browser_stack(game.res.items(), &native(stack))?;
        let mut lines = game.stack_tooltip(&stack);
        lines.extend(extra.iter().map(|e| (horizon_client::text::Text::literal(e), rgba(170, 170, 170, 255))));
        Some(lines)
    }) {
        hover_lines = Some(lines);
    }
    // A graph says its share at the pointer's height.
    if let Some((rect, bottom, share)) = hits.graphs.iter().find(|h| inside(cursor, h.0)) {
        let i = (((cursor.0 - rect[0]) / rect[2]) * share.len() as f32) as usize;
        if let Some(v) = share.get(i) {
            hover_lines = Some(vec![(horizon_client::text::Text::literal(&format!("Y {}: {}", bottom + i as i32, crate::game::port_jei_collection::percent(f64::from(*v) / 100.0))), u32::MAX)]);
        }
    }
    if let Some(lines) = hits.tip.take() {
        hover_lines = Some(lines.iter().map(|(text, color)| (horizon_client::text::Text::literal(text), *color)).collect());
    }
    if let Some(lines) = hover_lines {
        game.draw_tooltip_lines(batch, &lines, cursor.0, cursor.1);
    }
    HITS.with_borrow_mut(|h| *h = hits);
}

#[allow(clippy::too_many_arguments)]
fn draw_card(game: &Game<'_>, batch: &mut HudBatch, card: &Card, index: usize, x: f32, y: f32, h: f32, seconds: u64, hits: &mut Hits) {
    let width = WIDTH - 2.0 * CARD_X;
    bevel(game, batch, [x, y, width, h], rgba(198, 198, 198, 255), rgba(139, 139, 139, 255), rgba(255, 255, 255, 255));
    let atlas = &game.gui.atlas;
    match card {
        Card::Recipe(view) => {
            let (cols, rows) = match view.layout {
                Layout::Grid => (view.width.max(1) as usize, view.height.max(1) as usize),
                Layout::Cooking => (1, 1),
                Layout::Smithing => (3, 1),
                Layout::Brewing => (2, 1),
                Layout::Single => (1, 1),
            };
            let block = if view.layout == Layout::Cooking { 2.0 * SLOT } else { rows as f32 * SLOT };
            let body = block.max(26.0);
            // A page with nothing to make (CertusGrowthCategory.GETTING_BUDDING_QUARTZ) is its slots and its words.
            let makes = !view.results.is_empty();
            let total = cols as f32 * SLOT + if makes { 8.0 + 20.0 + 8.0 + 26.0 } else { 0.0 };
            let sx = x + ((width - total) / 2.0).floor();
            let sy = y + 4.0 + ((body - block) / 2.0).floor();
            for r in 0..rows {
                for c in 0..cols {
                    let stacks = view.slots.get(r * cols + c).map_or(&[][..], Vec::as_slice);
                    slot(game, batch, sx + c as f32 * SLOT, sy + r as f32 * SLOT, stacks, &[], seconds, hits);
                }
            }
            if view.layout == Layout::Cooking {
                batch.sprite(atlas, "gui/sprites/container/furnace/lit_progress", sx + 2.0, sy + SLOT + 2.0);
            }
            if makes {
                let ax = sx + cols as f32 * SLOT + 8.0;
                let cy = y + 4.0 + (body / 2.0).floor();
                arrow(game, batch, ax, cy - 7.0);
                let rx = ax + 20.0 + 8.0;
                bevel(game, batch, [rx, cy - 13.0, 26.0, 26.0], rgba(139, 139, 139, 255), rgba(55, 55, 55, 255), rgba(255, 255, 255, 255));
                slot(game, batch, rx + 4.0, cy - 9.0, &view.results, &[], seconds, hits);
            }
            if let Some(action) = view.action {
                // On an AE2 terminal the "+" answers for what the network and the player have, as JEI asks its
                // transfer handler each frame.
                let state = crate::game::port_ae2_jei::transfer_state(game, view);
                let enabled = state.as_ref().map_or(action, |s| s.enabled);
                let rect = [x + width - 15.0, y + 4.0 + body - 12.0, 12.0, 12.0];
                if enabled {
                    button(game, batch, rect, "+", hits, Act::Plus(index), &format!("+ {index}"));
                } else {
                    horizon_client::ui::nine_slice(batch, atlas, "gui/sprites/widget/button_disabled", rect[0], rect[1], rect[2], rect[3], u32::MAX);
                    batch.text(atlas, "+", rect[0] + 3.0, rect[1] + 2.0, rgba(160, 160, 160, 255), true);
                }
                if let Some(state) = state {
                    if let Some(tint) = state.tint {
                        batch.fill(atlas, rect[0], rect[1], rect[2], rect[3], tint);
                    }
                    if inside(game.cursor, rect) {
                        for (i, color) in &state.marks {
                            let (c, r) = ((i % cols) as f32, (i / cols) as f32);
                            batch.fill(atlas, sx + c * SLOT + 1.0, sy + r * SLOT + 1.0, 16.0, 16.0, *color);
                        }
                        hits.tip = Some(state.lines);
                    }
                }
            }
            for (i, line) in view.note.lines().enumerate() {
                batch.text(atlas, line, x + 4.0, y + 4.0 + body + 2.0 + i as f32 * 10.0, TEXT, false);
            }
        }
        Card::Source { from, item, lines } => {
            let total = SLOT + 8.0 + 20.0 + 8.0 + 26.0;
            let sx = x + ((width - total) / 2.0).floor();
            slot(game, batch, sx, y + 8.0, from, &[], seconds, hits);
            arrow(game, batch, sx + SLOT + 8.0, y + 10.0);
            let rx = sx + SLOT + 8.0 + 20.0 + 8.0;
            bevel(game, batch, [rx, y + 4.0, 26.0, 26.0], rgba(139, 139, 139, 255), rgba(55, 55, 55, 255), rgba(255, 255, 255, 255));
            slot(game, batch, rx + 4.0, y + 8.0, std::slice::from_ref(item), &[], seconds, hits);
            for (i, line) in lines.iter().enumerate() {
                batch.text(atlas, line, x + 4.0, y + 32.0 + i as f32 * 10.0, TEXT, false);
            }
        }
        Card::Text(lines) => {
            for (i, line) in lines.iter().enumerate() {
                batch.text(atlas, line, x + 4.0, y + 4.0 + i as f32 * 10.0, TEXT, false);
            }
        }
        Card::World { dimension, name, lines } => {
            slot(game, batch, x + 3.0, y + 3.0, &[Stack::item(WORLD_ICONS[*dimension])], &[], 0, hits);
            batch.text(atlas, name, x + 24.0, y + 8.0, TEXT, false);
            for (i, line) in lines.iter().enumerate() {
                batch.text(atlas, line, x + 4.0, y + 23.0 + i as f32 * 10.0, TEXT, false);
            }
        }
        Card::Mob { mob, name, info, drops, wanted } => {
            batch.text(atlas, name, x + 4.0, y + 4.0, TEXT, false);
            for (i, line) in info.iter().enumerate() {
                batch.text(atlas, line, x + 4.0, y + 14.0 + i as f32 * 10.0, TEXT, false);
            }
            let my = y + 14.0 + info.len() as f32 * 10.0;
            let frame = [x + 4.0, my, MOB_AREA, MOB_AREA];
            bevel(game, batch, frame, rgba(139, 139, 139, 255), rgba(55, 55, 55, 255), rgba(255, 255, 255, 255));
            draw_mob(game, batch, mob, mob, None, frame);
            hits.mobs.push((frame, mob.clone()));
            arrow(game, batch, x + 62.0, my + 20.0);
            let (gx, gy) = (x + 84.0, my);
            let cols = 5;
            for (i, d) in drops.iter().take(15).enumerate() {
                let (sx, sy) = (gx + (i % cols) as f32 * SLOT, gy + (i / cols) as f32 * SLOT);
                let stack = Stack { item: d.item.clone(), count: d.count, components: String::new() };
                slot(game, batch, sx, sy, &[stack], std::slice::from_ref(&d.line), 0, hits);
                if d.item == *wanted {
                    let a = &game.gui.atlas;
                    let c = rgba(255, 255, 160, 255);
                    batch.fill(a, sx, sy, SLOT, 1.0, c);
                    batch.fill(a, sx, sy + SLOT - 1.0, SLOT, 1.0, c);
                    batch.fill(a, sx, sy, 1.0, SLOT, c);
                    batch.fill(a, sx + SLOT - 1.0, sy, 1.0, SLOT, c);
                }
            }
        }
        Card::Trade { who, station, level, profession, mob, wants, gives } => {
            batch.text(atlas, who, x + 4.0, y + 4.0, TEXT, false);
            let mut my = y + 14.0;
            if let Some(block) = station {
                batch.text(atlas, "Work Station:", x + 4.0, my + 5.0, TEXT, false);
                let lx = x + 4.0 + HudBatch::text_width(atlas, "Work Station:") + 3.0;
                slot(game, batch, lx, my, &[Stack::item(block)], &[], 0, hits);
                my += 18.0;
            }
            let frame = [x + 4.0, my, MOB_AREA, MOB_AREA];
            bevel(game, batch, frame, rgba(139, 139, 139, 255), rgba(55, 55, 55, 255), rgba(255, 255, 255, 255));
            let key = format!("{mob} {}", profession.map_or(99, u32::from));
            draw_mob(game, batch, mob, &key, profession.map(|p| (p, level.unwrap_or(1) as u8)), frame);
            hits.mobs.push((frame, key));
            let cy = my + 18.0;
            if let Some(level) = level {
                batch.text(atlas, &format!("lv. {level}"), x + 62.0, cy - 12.0, TEXT, false);
            }
            let mut sx = x + 62.0;
            for i in 0..2 {
                let stacks = wants.get(i).map_or(&[][..], std::slice::from_ref);
                slot(game, batch, sx, cy, stacks, &[], 0, hits);
                sx += SLOT;
            }
            arrow(game, batch, sx + 3.0, cy + 2.0);
            slot(game, batch, sx + 25.0, cy, std::slice::from_ref(gives), &[], 0, hits);
        }
        Card::Ore { dimension, name, block, scope, bottom, share, summary, drops } => {
            slot(game, batch, x + 3.0, y + 3.0, &[Stack::item(WORLD_ICONS[*dimension])], &[], 0, hits);
            let a = &game.gui.atlas;
            batch.text(a, name, x + 24.0, y + 3.0, TEXT, false);
            batch.text(a, scope, x + 24.0, y + 12.0, TEXT, false);
            let (gx, gy, gw, gh) = (x + 26.0, y + 24.0, width - 32.0, 40.0);
            slot(game, batch, x + 4.0, gy + 10.0, &[Stack::item(block)], &[], 0, hits);
            let peak = share.iter().copied().fold(0.0f32, f32::max).max(1e-6);
            let dark = rgba(30, 30, 30, 255);
            batch.fill(a, gx - 1.0, gy, 1.0, gh + 1.0, dark);
            batch.fill(a, gx - 1.0, gy + gh, gw + 1.0, 1.0, dark);
            let per = share.len() as f32 / gw;
            for column in 0..gw as usize {
                let from = (column as f32 * per) as usize;
                let to = (((column + 1) as f32 * per) as usize).min(share.len());
                let v = share.get(from..to.max(from + 1)).map_or(0.0, |s| s.iter().copied().fold(0.0f32, f32::max));
                let bar = (v / peak * gh).ceil();
                if bar > 0.0 {
                    batch.fill(a, gx + column as f32, gy + gh - bar, 1.0, bar, dark);
                }
            }
            hits.graphs.push(([gx, gy, gw, gh], *bottom, share.clone()));
            let top = crate::game::port_jei_collection::percent(f64::from(peak) / 100.0);
            batch.text(a, &top, gx + gw - HudBatch::text_width(a, &top), gy - 9.0, TEXT, false);
            let y_low = bottom.to_string();
            let y_high = (bottom + share.len() as i32 - 1).to_string();
            batch.text(a, &y_low, gx - 1.0, gy + gh + 2.0, TEXT, false);
            batch.text(a, &y_high, gx + gw - HudBatch::text_width(a, &y_high), gy + gh + 2.0, TEXT, false);
            for (i, line) in summary.iter().enumerate() {
                batch.text(a, line, x + 4.0, gy + gh + 12.0 + i as f32 * 10.0, TEXT, false);
            }
            if !drops.is_empty() {
                let dy = gy + gh + 12.0 + summary.len() as f32 * 10.0 + 1.0;
                batch.text(a, "Drops", x + 4.0, dy + 5.0, TEXT, false);
                for (i, d) in drops.iter().take(7).enumerate() {
                    slot(game, batch, x + 36.0 + i as f32 * SLOT, dy, &[Stack::item(d)], &[], 0, hits);
                }
            }
        }
    }
}

/// `extractEntityInInventoryFollowsMouse` for a mob, as the mount menu draws its own, turned as dragged.
fn draw_mob(game: &Game<'_>, batch: &mut HudBatch, id: &str, key: &str, villager: Option<(u8, u8)>, [x0, y0, w0, h0]: [f32; 4]) {
    let model = MODELS.with_borrow_mut(|models| {
        if !models.contains_key(key) {
            let kind = game.res.mob_types().get(id)?.clone();
            let at = horizon_core::geometry::Vec3 { x: 0.0, y: 0.0, z: 0.0 };
            let mut mob = Mob::new(1, kind, at, 0.0);
            if let Some((profession, level)) = villager {
                mob.extra.variant2 = profession;
                mob.extra.variant3 = level;
            }
            models.insert(key.to_owned(), mob);
        }
        models.get(key).cloned()
    });
    let Some(mut m) = model else { return };
    let (yaw, pitch) = TURNED.with_borrow(|t| t.get(key).copied().unwrap_or((30.0, -10.0)));
    m.previous = m.position;
    m.body_yaw = 180.0 + yaw;
    m.previous_body_yaw = m.body_yaw;
    m.head_yaw = m.body_yaw;
    m.previous_head_yaw = m.head_yaw;
    m.head_pitch = 0.0;
    m.previous_head_pitch = 0.0;
    m.hurt_time = 0;
    m.death_time = 0;
    let camera = glam::DVec3::new(m.position.x, m.position.y, m.position.z);
    let atlas = &game.gui.atlas;
    let lookup = |name: &str| atlas.sprites.get(name).map(|r| [r.u0, r.v0, r.u1, r.v1]);
    let mut verts = Vec::new();
    horizon_client::mobs::render::render_with(&m, 1.0, camera, &lookup, 240 | 240 << 8, game.res.mob_data(), &[], &mut verts);
    let height = m.kind.height as f32;
    let size = ((w0 - 8.0) / height.max(m.kind.width as f32).max(0.5)).min(26.0);
    let (cx, cy) = (x0 + w0 / 2.0, y0 + h0 / 2.0);
    let gui = glam::Mat4::from_translation(glam::Vec3::new(cx, cy, 0.0))
        * glam::Mat4::from_scale(glam::Vec3::new(size, size, -size))
        * glam::Mat4::from_rotation_x(pitch.to_radians())
        * glam::Mat4::from_translation(glam::Vec3::new(0.0, height / 2.0, 0.0))
        * glam::Mat4::from_rotation_z(std::f32::consts::PI);
    let mut quads: Vec<(f32, [[f32; 2]; 4], [[f32; 2]; 4], u32)> = Vec::new();
    for q in verts.as_chunks::<4>().0 {
        let p: [glam::Vec3; 4] = std::array::from_fn(|i| gui.transform_point3(glam::Vec3::from_array(q[i].pos)));
        let n = (p[1] - p[0]).cross(p[2] - p[0]);
        if n.z >= 0.0 {
            continue;
        }
        let n = n.normalize_or_zero();
        let shade = (0.55 + 0.45 * -n.z - 0.15 * n.y).clamp(0.4, 1.0);
        let c = q[0].color;
        let ch = |s: u32| ((((c >> s) & 255) as f32 * shade) as u32) << s;
        let color = (c & 0xFF00_0000) | ch(0) | ch(8) | ch(16);
        let depth = p.iter().map(|v| v.z).sum::<f32>() / 4.0;
        quads.push((depth, p.map(|v| [v.x, v.y]), std::array::from_fn(|i| q[i].uv), color));
    }
    quads.sort_by(|a, b| a.0.total_cmp(&b.0));
    let start = batch.vertices.len();
    for (_, p, uv, color) in quads {
        batch.quad(p, uv, color);
    }
    batch.clip_since(start, x0 + 1.0, y0 + 1.0, x0 + w0 - 1.0, y0 + h0 - 1.0);
}

/// The dragged mob follows the pointer.
fn turn(cursor: (f32, f32)) {
    if let Some(key) = DRAGGING.with_borrow(Clone::clone) {
        let (x, y) = LAST.replace(cursor);
        let (dx, dy) = (cursor.0 - x, cursor.1 - y);
        TURNED.with_borrow_mut(|t| {
            let e = t.entry(key).or_insert((30.0, -10.0));
            e.0 -= dx * 2.0;
            e.1 = (e.1 + dy * 2.0).clamp(-80.0, 80.0);
        });
    }
}

/// Once a frame, while the left button is held on a mob.
pub fn frame(game: &Game<'_>) {
    turn(game.cursor);
}

/// `native_screen_mouse_button_pressed_pre`.
pub fn press(game: &Game<'_>, mouse: [f32; 2], button: i32, canceled: &mut bool) {
    if !is_open() {
        return;
    }
    let (x0, _, _) = geometry(game);
    if mouse[0] >= x0 + WIDTH + 2.0 {
        return;
    }
    *canceled = true;
    let at = (mouse[0], mouse[1]);
    HITS.with_borrow(|h| {
        if let Some((_, act)) = h.buttons.iter().find(|b| inside(at, b.0)) {
            ACTS.with_borrow_mut(|a| a.push(act.clone()));
        } else if let Some((_, stack, _)) = h.slots.iter().find(|s| inside(at, s.0)) {
            ACTS.with_borrow_mut(|a| a.push(Act::Slot { stack: stack.clone(), uses: button == 1 }));
        } else if let Some((_, key)) = h.mobs.iter().find(|m| inside(at, m.0)) {
            DRAGGING.with_borrow_mut(|d| *d = Some(key.clone()));
            LAST.set(at);
        }
    });
}

/// JEIPlugin.getGuiClickableAreas: the Inscriber screen's arrow, from the window's top left.
const INSCRIBER_AREA: [f32; 4] = [82.0, 39.0, 26.0, 16.0];

fn on_inscriber_area(game: &Game<'_>, screen: &NativeScreenView<'_>) -> bool {
    let [x, y, _, _] = screen.rect;
    let [ax, ay, aw, ah] = INSCRIBER_AREA;
    screen.menu == Some("ae2:inscriber") && !is_open() && game.native_on(crate::game::jei::ID) && inside((screen.mouse[0], screen.mouse[1]), [x + ax, y + ay, aw, ah])
}

/// `native_screen_mouse_button_pressed_pre`: a click on the area opens its category.
pub fn press_area(game: &Game<'_>, screen: &NativeScreenView<'_>, button: i32, canceled: &mut bool) {
    if button == 0 && on_inscriber_area(game, screen) {
        *canceled = true;
        ACTS.with_borrow_mut(|a| a.push(Act::Category("ae2:inscriber".to_owned())));
    }
}

/// `native_screen_render_post`: the area says what a click on it does (IGuiClickableArea's tooltip).
pub fn area_tooltip(game: &Game<'_>, batch: &mut HudBatch, screen: &NativeScreenView<'_>) {
    if on_inscriber_area(game, screen) {
        let lines = vec![(horizon_client::text::Text::literal("Show Recipes"), u32::MAX)];
        game.draw_tooltip_lines(batch, &lines, screen.mouse[0], screen.mouse[1]);
    }
}

/// `native_screen_mouse_button_released_pre`.
pub fn release(mouse: [f32; 2]) {
    turn((mouse[0], mouse[1]));
    DRAGGING.with_borrow_mut(|d| *d = None);
}

/// `native_screen_mouse_scrolled_pre`: the wheel turns the pages.
pub fn scroll(by: f64, canceled: &mut bool) {
    if is_open() {
        *canceled = true;
        ACTS.with_borrow_mut(|a| a.push(Act::Page(if by > 0.0 { -1 } else { 1 })));
    }
}

/// `native_screen_key_pressed_pre`: Escape and the inventory key close it, Backspace goes back a page; a key over a
/// slot looks that stack up.
pub fn key(game: &Game<'_>, mouse: [f32; 2], key: &str, canceled: &mut bool) {
    if !is_open() || game.mod_browser_typing() {
        return;
    }
    let (x0, _, _) = geometry(game);
    if mouse[0] >= x0 + WIDTH + 2.0 && key != "key.keyboard.escape" {
        return;
    }
    *canceled = true;
    match key {
        "key.keyboard.escape" | "key.keyboard.e" => ACTS.with_borrow_mut(|a| a.push(Act::Close)),
        "key.keyboard.backspace" => ACTS.with_borrow_mut(|a| a.push(Act::Back)),
        "key.keyboard.page.up" => ACTS.with_borrow_mut(|a| a.push(Act::Page(-1))),
        "key.keyboard.page.down" => ACTS.with_borrow_mut(|a| a.push(Act::Page(1))),
        _ => {
            let at = (mouse[0], mouse[1]);
            if let Some(stack) = HITS.with_borrow(|h| h.slots.iter().find(|s| inside(at, s.0)).map(|s| s.1.clone())) {
                ACTS.with_borrow_mut(|a| a.push(Act::Key { key: key.to_owned(), stack }));
            }
        }
    }
}
});
