ae2_optional::if_mod_items!("jei", {
// AE2's JEI integration (ae2jeiintegration's JEIPlugin), as a provider to the kit's page flow: its categories (inscriber,
// charger, transform, certus growth, condenser...), the facades in the list, its recipe transfer on the terminals, and
// its meteorites in the sources.
ae2_optional::kit_use!(layer as kit_layer);
ae2_optional::kit_use!(window as kit_window);
ae2_optional::kit_use!(collection as kit_collection);
ae2_optional::kit_use!(panel as kit_panel);
use crate::game::Game;
use crate::game::jei::{self, Layout, Stack, View};
use crate::game::native_hooks::{NativeScreenView, NativeStack};
use self::kit_layer::Provider;
use self::kit_window::{Act, Card, Tab, Transfer};
use horizon_client::hud::{HudBatch, rgba};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// The Crafting tab's icon.
const CRAFTING: &str = "minecraft:crafting_table";

/// JEIPlugin.registerRecipeCatalysts, AE2's own categories: U on one opens its category whole.
const CATALYSTS: [&str; 5] = ["ae2:condenser", "ae2:inscriber", "ae2:charger", "ae2:crank", "ae2:entropy_manipulator"];

/// One of AE2's recipe tabs: the item on it, how its recipes are laid out, its recipes.
struct Category {
    icon: &'static str,
    title: String,
    catalysts: &'static [&'static str],
    layout: Layout,
    /// The grid's size for `Layout::Grid`; 0 fits each recipe.
    width: u32,
    height: u32,
    recipes: Vec<Ae2Recipe>,
}

struct Ae2Recipe {
    /// Each slot's alternatives (empty: an empty cell).
    slots: Vec<Vec<Stack>>,
    results: Vec<Stack>,
    note: String,
}

/// The open terminal's list as the server last sent it: amounts by id, and what the grid can craft.
struct Stock {
    amounts: HashMap<String, i64>,
    craftable: HashSet<String>,
}

thread_local! {
    static CATEGORIES: RefCell<Option<Vec<Category>>> = const { RefCell::new(None) };
    static FACADES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    /// The list a moment ago: every "+" on the page asks each frame, and the list can be long.
    static STOCK: RefCell<Option<(std::time::Instant, Stock)>> = const { RefCell::new(None) };
    /// The panel's list this layer has last seen (`Shown.version`): one comparison a frame until it changes.
    static PANEL_SEEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `ItemModText`: AE2's words for its JEI integration, from the content's language files.
fn text(key: &str, english: &str) -> String {
    horizon_client::lang::get_or(&format!("ae2.rei_jei_integration.{key}"), english)
}

/// `Component.translatable`.
fn tr(key: &str, english: &str) -> String {
    horizon_client::lang::get_or(key, english)
}

/// The kit's page flow asks here.
pub fn provider() -> Provider {
    Provider {
        is_catalyst: |item| CATALYSTS.contains(&item),
        // JEIPlugin.registerRecipeCatalysts: the crafting terminals craft too.
        station_catalysts,
        crafting_views: |item, uses| FACADES.with_borrow(|f| f.as_ref().map_or_else(Vec::new, |f| facade_views(f, item, uses))),
        tabs,
        sources: meteor_tabs,
        note: |item| NOTES.iter().find(|(i, _)| *i == item).map(|(_, n)| *n),
        takes_recipes: ae2_terminal_open,
        transfer_state,
        mark_recipes,
        plus,
        opens_on_click: |s| is_block_facade(&s.item, &s.components),
        ..Provider::new("ae2")
    }
}

fn station_catalysts(icon: &str) -> &'static [&'static str] {
    const TERMINALS: &[&str] = &["ae2:crafting_terminal", "ae2:wireless_crafting_terminal"];
    if icon == CRAFTING { TERMINALS } else { &[] }
}

/// `native_frame`.
pub fn frame(game: &mut Game<'_>) {
    kit_layer::register(provider());
    if game.native_player().is_none() {
        CATEGORIES.with_borrow_mut(|c| *c = None);
        FACADES.with_borrow_mut(|f| *f = None);
        return;
    }
    if FACADES.with_borrow(Option::is_none) {
        let facades: Vec<String> = game
            .native_data("data/ae2/hz/facades.json")
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|table| table.as_object().map(|o| o.keys().cloned().collect()))
            .unwrap_or_default();
        FACADES.with_borrow_mut(|f| *f = Some(facades));
    }
    list_facades(game);
}

/// AE2's tabs for a page.
fn tabs(game: &Game<'_>, item: &Stack, uses: bool) -> Vec<Tab> {
    if CATEGORIES.with_borrow(Option::is_none) {
        let made = categories(game);
        CATEGORIES.with_borrow_mut(|c| *c = Some(made));
    }
    CATEGORIES.with_borrow(|c| {
        let mut out = Vec::new();
        for c in c.iter().flatten() {
            let wanted = |s: &Stack| s.item == item.item;
            // U on the category's catalyst: all of it.
            let whole = uses && CATALYSTS.contains(&item.item.as_str()) && c.catalysts.contains(&item.item.as_str());
            let cards: Vec<Card> = c
                .recipes
                .iter()
                .filter(|r| whole || if uses { r.slots.iter().flatten().any(wanted) } else { r.results.iter().any(wanted) })
                .map(|r| Card::Recipe(ae2_view(game, c, r)))
                .collect();
            if !cards.is_empty() {
                out.push(Tab { icon: c.icon.to_owned(), title: c.title.clone(), catalysts: c.catalysts.iter().map(|s| (*s).to_owned()).collect(), cards });
            }
        }
        out
    })
}

/// On the pattern encoding terminal every recipe (not a source) gets the "+" that encodes it
/// (EncodePatternTransferHandler is universal); on a crafting terminal only the Crafting tab's do
/// (UseCraftingRecipeTransfer takes `RecipeTypes.CRAFTING` and no other: a machine's recipe has none).
fn mark_recipes(game: &Game<'_>, tabs: &mut Vec<Tab>) {
    if !ae2_terminal_open(game) {
        return;
    }
    let encoding = encoding_open(game);
    for t in tabs {
        let crafting = t.icon == CRAFTING;
        for c in &mut t.cards {
            if let Card::Recipe(v) = c {
                let takes = if encoding { !v.results.is_empty() } else { crafting && v.layout == Layout::Grid };
                v.action = takes.then_some(true);
            }
        }
    }
}

/// The "+" on an AE2 terminal: the recipe fills it.
fn plus(game: &mut Game<'_>, index: usize) {
    let Some(view) = kit_window::recipe_view(index) else { return };
    // What the last frame drew may have gone since.
    if transfer_state(game, &view).is_some_and(|s| !s.enabled) {
        return;
    }
    let crafting = kit_window::shown_icon().as_deref() == Some(CRAFTING) && view.layout == Layout::Grid;
    // UseCraftingRecipeTransfer: craftMissing is Control held (the encoder has nothing to craft).
    let craft_missing = crafting && !encoding_open(game) && game.has_control_down();
    let wanted = if craft_missing { to_craft(game, &view) } else { Vec::new() };
    transfer(game, &view, crafting, false);
    if !wanted.is_empty() {
        // JEI goes back to the screen, where the confirm dialog opens.
        kit_window::close();
        kit_layer::clear_history();
        crate::game::port_ae2_me::jei_craft_missing(wanted);
    }
}

fn meteor_tabs(game: &Game<'_>, item: &str) -> Vec<Tab> {
    meteor_cards(game, item).map_or_else(Vec::new, |cards| vec![kit_collection::tab("ae2:mysterious_cube", "Meteorites", &["ae2:meteorite_compass"], cards)])
}

/// How AE2's world generation gives an item: the presses, the cube, and what meteorites are made of.
fn meteor_cards(game: &Game<'_>, item: &str) -> Option<Vec<Card>> {
    const FROM_METEORITE: [&str; 9] = [
        "ae2:sky_stone_block",
        "ae2:flawless_budding_quartz",
        "ae2:flawed_budding_quartz",
        "ae2:chipped_budding_quartz",
        "ae2:damaged_budding_quartz",
        "ae2:small_quartz_bud",
        "ae2:medium_quartz_bud",
        "ae2:large_quartz_bud",
        "ae2:quartz_cluster",
    ];
    let press = item.starts_with("ae2:") && item.ends_with("_press");
    if !press && item != "ae2:mysterious_cube" && !FROM_METEORITE.contains(&item) {
        return None;
    }
    let mut cards = Vec::new();
    if press {
        cards.push(kit_collection::source(game, "ae2:mysterious_cube", item, 1, "Break a Mysterious Cube: it drops all four presses (Silk Touch keeps the cube instead)."));
    }
    if press || item == "ae2:mysterious_cube" {
        cards.push(kit_collection::source(game, "ae2:sky_stone_block", "ae2:mysterious_cube", 1, "One sits at the center of every meteorite, under the sky stone."));
        cards.push(kit_collection::source(game, "ae2:meteorite_compass", "ae2:mysterious_cube", 1, "A Meteorite Compass points to the nearest cube."));
    } else {
        cards.push(kit_collection::source(game, "ae2:sky_stone_block", item, 1, "Found in and around meteorites."));
    }
    cards.push(Card::World {
        dimension: 0,
        name: kit_collection::DIMENSIONS[0].to_owned(),
        lines: kit_collection::wrap_text(game, "Meteorites: one try in each area of 32 x 32 chunks (512 x 512 blocks), at least 8 chunks apart, in any biome."),
    });
    Some(cards)
}

const NOTES: &[(&str, &str)] = &[
    ("ae2:hz_crank_handle", "Part of the crank's model; not an item to collect."),
    ("ae2:hz_sky_stone_chest_lid", "Part of the chest's model; not an item to collect."),
    ("ae2:hz_smooth_sky_stone_chest_lid", "Part of the chest's model; not an item to collect."),
    ("ae2:wrapped_generic_stack", "How AE2 shows a stack inside a pattern; not an item to collect."),
    ("ae2:creative_item_cell", "Creative mode only."),
    ("ae2:creative_fluid_cell", "Creative mode only."),
    ("ae2:creative_energy_cell", "Creative mode only."),
];

/// JEIPlugin.getGuiClickableAreas: the Inscriber screen's arrow, from the window's top left.
const INSCRIBER_AREA: [f32; 4] = [82.0, 39.0, 26.0, 16.0];

fn on_inscriber_area(game: &Game<'_>, screen: &NativeScreenView<'_>) -> bool {
    let [x, y, _, _] = screen.rect;
    let [ax, ay, aw, ah] = INSCRIBER_AREA;
    screen.menu == Some("ae2:inscriber") && !kit_window::is_open() && game.native_on(jei::ID) && kit_window::inside((screen.mouse[0], screen.mouse[1]), [x + ax, y + ay, aw, ah])
}

/// `native_screen_mouse_button_pressed_pre`: a click on the area opens its category.
pub fn press_area(game: &Game<'_>, screen: &NativeScreenView<'_>, button: i32, canceled: &mut bool) {
    if button == 0 && on_inscriber_area(game, screen) {
        *canceled = true;
        kit_window::push_act(Act::Category("ae2:inscriber".to_owned()));
    }
}

/// `native_screen_render_post`: the area says what a click on it does (IGuiClickableArea's tooltip).
pub fn area_tooltip(game: &Game<'_>, batch: &mut HudBatch, screen: &NativeScreenView<'_>) {
    if on_inscriber_area(game, screen) {
        let lines = vec![(horizon_client::text::Text::literal("Show Recipes"), u32::MAX)];
        game.draw_tooltip_lines(batch, &lines, screen.mouse[0], screen.mouse[1]);
    }
}

/// FacadeRegistryPlugin.make: four cable anchors around the block make four facades of it.
fn facade_view(block: &str) -> View {
    let mut slots: Vec<Vec<Stack>> = vec![Vec::new(); 9];
    for i in [1, 3, 5, 7] {
        slots[i] = one("ae2:cable_anchor");
    }
    slots[4] = one(block);
    View { layout: Layout::Grid, width: 3, height: 3, slots, results: vec![facade_stack(block, 4)], note: String::new(), action: None }
}

/// A facade of `block`, as FacadeItem.createFacadeForItem makes it.
fn facade_stack(block: &str, count: u32) -> Stack {
    let (ns, path) = block.split_once(':').unwrap_or(("minecraft", block));
    let flat = if ns == "minecraft" { path.to_owned() } else { format!("{ns}_{path}") };
    let components = format!("[minecraft:custom_data={{item:\"{block}\"}},minecraft:item_model=\"ae2:hz_facade_{flat}\"]");
    Stack { item: "ae2:facade".to_owned(), count, components }
}

/// A facade of some block, not the one of none that JEI lists.
fn is_block_facade(item: &str, components: &str) -> bool {
    item == "ae2:facade" && !components.is_empty()
}

/// The facades a search leaves: every word in the facade's name, its block's or its block's id (`@ae2` for the mod).
fn facades_for(facades: &[String], names: &HashMap<String, String>, search: &str) -> Vec<Stack> {
    let words: Vec<String> = search.to_lowercase().split_whitespace().map(str::to_owned).collect();
    let facade = names.get("ae2:facade").map_or("facade", String::as_str).to_lowercase();
    facades
        .iter()
        .filter(|block| {
            let name = names.get(*block).map_or("", String::as_str).to_lowercase();
            words.iter().all(|w| match w.strip_prefix('@') {
                Some(m) => "ae2".contains(m),
                None => facade.contains(w.as_str()) || name.contains(w.as_str()) || block.contains(w.as_str()),
            })
        })
        .map(|block| facade_stack(block, 1))
        .collect()
}

/// JEIPlugin, unless `enableFacadesInJEI` is off: the facades in the ingredient list, as AE2's creative tab has them. JEI's viewer lists
/// each item once, so this layer puts them after its list whenever JEI has sent one, and clicks past JEI's end are this
/// layer's alone (`listed`).
fn list_facades(game: &mut Game<'_>) {
    use self::kit_panel as panel;
    let Some((version, search, mut items)) = panel::changed_items(game, jei::ID, PANEL_SEEN.get()) else { return };
    PANEL_SEEN.set(version);
    if !crate::game::port_ae2_me::facades_in_jei() || items.iter().any(|s| is_block_facade(&s.item, &s.components)) || !game.native_on(jei::ID) || !kit_layer::ensure(game) {
        return;
    }
    let added = FACADES.with_borrow(|f| kit_layer::with_names(|names| facades_for(f.as_deref().unwrap_or(&[]), names, &search))).unwrap_or_default();
    if added.is_empty() {
        return;
    }
    // JEI's empty list keeps one invisible slot, which isn't wanted now.
    items.retain(|s| s.item != "minecraft:air");
    items.extend(added.into_iter().map(|s| NativeStack { item: s.item, count: s.count, components: s.components }));
    kit_layer::set_listed(items.iter().map(|s| Stack { item: s.item.clone(), count: s.count, components: s.components.clone() }).collect());
    game.native_panel_items(jei::ID, items);
    PANEL_SEEN.set(panel::items_version(game));
}

/// FacadeRegistryPlugin.getRecipes: a block's uses have its facade, a facade's recipes the recipe of that facade (JEI's
/// viewer stops at 256 of a tab's recipes; the panel's facade, of no block, stands for them all).
fn facade_views(facades: &[String], item: &Stack, uses: bool) -> Vec<View> {
    if uses {
        return facades.iter().filter(|b| **b == item.item).map(|b| facade_view(b)).collect();
    }
    if item.item != "ae2:facade" {
        return Vec::new();
    }
    // FacadeItem.getTextureItem: the block's id, quoted, in the stack's components.
    let of_this: Vec<View> = facades.iter().filter(|b| item.components.contains(&format!("\"{b}\""))).map(|b| facade_view(b)).collect();
    if of_this.is_empty() { facades.iter().map(|b| facade_view(b)).collect() } else { of_this }
}

fn ae2_view(game: &Game<'_>, c: &Category, r: &Ae2Recipe) -> View {
    let (mut layout, mut width, mut height) = (c.layout, c.width, c.height);
    if layout == Layout::Grid && width == 0 {
        let n = r.slots.len() as u32;
        width = n.clamp(1, 3);
        height = n.div_ceil(width).max(1);
    }
    // A page of a one-slot category with more slots (CertusGrowthCategory.GETTING_BUDDING_QUARTZ): side by side.
    if layout == Layout::Single && r.slots.len() > 1 {
        layout = Layout::Grid;
        width = r.slots.len() as u32;
        height = 1;
    }
    // The note as the card's width has it.
    let note = r.note.split('\n').flat_map(|line| kit_collection::wrap_text(game, line)).collect::<Vec<_>>().join("\n");
    View { layout, width, height, slots: r.slots.clone(), results: r.results.clone(), note, action: None }
}

/// An AE2 terminal that takes recipes is open (`AEGuiContainerHandler`).
fn ae2_terminal_open(game: &Game<'_>) -> bool {
    matches!(game.native_screen_title().as_deref(), Some("ME Pattern Encoding Terminal" | "ME Crafting Terminal")) || crate::game::port_ae2_me::wireless_crafting_open()
}

/// The pattern encoding terminal is open (the crafting terminals fill their grid instead).
fn encoding_open(game: &Game<'_>) -> bool {
    game.native_screen_title().as_deref() == Some("ME Pattern Encoding Terminal")
}

/// The terminal's list, read again four times a second.
fn with_stock<R>(f: impl FnOnce(&Stock) -> R) -> R {
    STOCK.with_borrow_mut(|cached| {
        if cached.as_ref().is_none_or(|(at, _)| at.elapsed().as_millis() >= 250) {
            let (amounts, craftable) = crate::game::port_ae2_me::jei_terminal_stock();
            *cached = Some((std::time::Instant::now(), Stock { amounts, craftable }));
        }
        match cached.as_ref() {
            Some((_, stock)) => f(stock),
            None => f(&Stock { amounts: HashMap::new(), craftable: HashSet::new() }),
        }
    })
}

/// TransferHelper's colours (the ints AE2 gives them, as this window's).
fn blue_text() -> u32 {
    rgba(85, 85, 255, 255)
}

fn red_text() -> u32 {
    rgba(255, 85, 85, 255)
}

fn blue_slot() -> u32 {
    rgba(0, 0, 255, 64)
}

fn red_slot() -> u32 {
    rgba(255, 0, 0, 102)
}

/// What a recipe's "+" says on the open AE2 terminal; none on any other screen.
pub fn transfer_state(game: &Game<'_>, view: &View) -> Option<Transfer> {
    if !ae2_terminal_open(game) {
        return None;
    }
    Some(if encoding_open(game) { encode_state(view) } else { crafting_state(game, view) })
}

/// EncodePatternTransferHandler.findCraftableSlots and its ErrorRenderer: the slots the network can already craft
/// marked, and the tooltip of TransferHelper.createEncodingTooltip.
fn encode_state(view: &View) -> Transfer {
    let craftable: Vec<usize> =
        with_stock(|stock| view.slots.iter().enumerate().filter(|(_, slot)| slot.iter().any(|s| stock.craftable.contains(&s.item))).map(|(i, _)| i).collect());
    let mut lines = vec![(text("encode_pattern", "Encode pattern"), u32::MAX)];
    if !craftable.is_empty() {
        lines.push((text("has_encoded_ingredients", "Highlighted elements are already craftable"), blue_text()));
    }
    Transfer { enabled: true, tint: None, lines, marks: craftable.into_iter().map(|i| (i, blue_slot())).collect() }
}

/// UseCraftingRecipeTransfer.transferRecipe, with CraftingTermMenu.findMissingIngredients: each slot looks in the
/// player's inventory, then the network, then what the network can craft, each counted so that two slots don't both
/// take the one there is.
fn crafting_state(game: &Game<'_>, view: &View) -> Transfer {
    let refused = |why: String, marks: Vec<(usize, u32)>| Transfer {
        enabled: false,
        tint: None,
        lines: vec![(text("move_items", "Move items"), u32::MAX), (why, red_text())],
        marks,
    };
    let wanted: Vec<usize> = (0..view.slots.len()).filter(|i| !view.slots[*i].is_empty()).collect();
    if wanted.is_empty() {
        return refused(text("incompatible_recipe", "Incompatible recipe"), Vec::new());
    }
    let (missing, craftable) = find_ingredients(game, view, &wanted);
    // Nothing at all to move: the "+" is off, and the slots say why.
    if missing.len() == wanted.len() {
        return refused(text("no_items", "Found no compatible items"), missing.into_iter().map(|i| (i, red_slot())).collect());
    }
    // TransferHelper.createCraftingTooltip.
    let mut lines = vec![(text("move_items", "Move items"), u32::MAX)];
    if !craftable.is_empty() {
        let line = if game.has_control_down() { text("will_craft", "Will craft unavailable items") } else { text("ctrl_click_to_craft", "CTRL + click to craft unavailable items") };
        lines.push((line, blue_text()));
    }
    if !missing.is_empty() {
        lines.push((text("missing_items", "Missing items will be skipped"), red_text()));
    }
    // TransferHelper.ORANGE_PLUS_BUTTON_COLOR and BLUE_PLUS_BUTTON_COLOR.
    let tint = if !missing.is_empty() {
        Some(rgba(255, 165, 0, 128))
    } else if !craftable.is_empty() {
        Some(rgba(69, 69, 255, 128))
    } else {
        None
    };
    let marks = missing.into_iter().map(|i| (i, red_slot())).chain(craftable.into_iter().map(|i| (i, blue_slot()))).collect();
    Transfer { enabled: true, tint, lines, marks }
}

/// CraftingTermMenu.findMissingIngredients: of the slots `wanted`, those found nowhere and those only the network can
/// craft.
fn find_ingredients(game: &Game<'_>, view: &View, wanted: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let mut inventory: HashMap<&str, u32> = HashMap::new();
    for s in game.inventory.iter().take(36).flatten() {
        *inventory.entry(&*s.item).or_default() += s.count;
    }
    let (mut missing, mut craftable) = (Vec::new(), Vec::new());
    with_stock(|stock| {
        let mut reserved: HashMap<&str, i64> = HashMap::new();
        for &i in wanted {
            let slot = &view.slots[i];
            if let Some(s) = slot.iter().find(|s| inventory.get(s.item.as_str()).is_some_and(|n| *n > 0)) {
                if let Some(n) = inventory.get_mut(s.item.as_str()) {
                    *n -= 1;
                }
                continue;
            }
            if let Some(s) = slot.iter().find(|s| stock.amounts.get(&s.item).copied().unwrap_or(0) > reserved.get(s.item.as_str()).copied().unwrap_or(0)) {
                *reserved.entry(s.item.as_str()).or_default() += 1;
                continue;
            }
            if slot.iter().any(|s| stock.craftable.contains(&s.item)) {
                craftable.push(i);
            } else {
                missing.push(i);
            }
        }
    });
    (missing, craftable)
}

/// FillCraftingGridFromRecipePacket: what the "+" finds nowhere that the network can craft, a key and the slots wanting it
/// (AutoCraftEntry).
fn to_craft(game: &Game<'_>, view: &View) -> Vec<(String, i64)> {
    let wanted: Vec<usize> = (0..view.slots.len()).filter(|i| !view.slots[*i].is_empty()).collect();
    let (_, craftable) = find_ingredients(game, view, &wanted);
    let mut list: Vec<(String, i64)> = Vec::new();
    with_stock(|stock| {
        for i in craftable {
            let Some(s) = view.slots[i].iter().find(|s| stock.craftable.contains(&s.item)) else { continue };
            match list.iter_mut().find(|(id, _)| *id == s.item) {
                Some((_, n)) => *n += 1,
                None => list.push((s.item.clone(), 1)),
            }
        }
    });
    list
}

/// The "+" on an AE2 terminal: the recipe to the server (`ae2:transfer`), which fills the open terminal: `mode`,
/// `size`, then each slot's alternatives and amount (`s index id,id amount`) and the results (`r id amount`). `craft`
/// asks it to start crafting what it finds nowhere (FillCraftingGridFromRecipePacket's craftMissing).
fn transfer(game: &mut Game<'_>, view: &View, crafting: bool, craft_missing: bool) {
    let mut text = format!("mode {}\nsize {} {}\n", if crafting { "crafting" } else { "processing" }, view.width.max(1), view.height.max(1));
    if craft_missing {
        text.push_str("craft\n");
    }
    for (i, slot) in view.slots.iter().enumerate() {
        if let Some(first) = slot.first() {
            let ids: Vec<&str> = slot.iter().map(|s| s.item.as_str()).collect();
            text.push_str(&format!("s {i} {} {}\n", ids.join(","), first.count.max(1)));
        }
    }
    for r in &view.results {
        text.push_str(&format!("r {} {}\n", r.item, r.count.max(1)));
    }
    if let Err(e) = game.native_net_send_to_server(crate::game::port_ae2_me::NET_TRANSFER, text.as_bytes()) {
        log::info!("ae2: recipe transfer not sent: {e}");
    }
}

// ---------------------------------------------------------------------------------------------------------
// AE2's categories (ae2jeiintegration's categories package).

fn categories(game: &Game<'_>) -> Vec<Category> {
    vec![inscriber(game), charger(game), transform(game), certus_growth(), condenser(), entropy(game), attunement(game), fluids(game)]
}

/// An ingredient as a recipe's JSON names it: each item it takes (a tag's items).
fn alternatives(game: &Game<'_>, json: Option<&serde_json::Value>) -> Vec<Stack> {
    json.and_then(|v| game.native_recipe_ingredient(v)).map_or_else(Vec::new, |i| i.order.iter().map(|id| Stack::item(id)).collect())
}

fn result(game: &Game<'_>, json: &serde_json::Value) -> Vec<Stack> {
    json.get("result")
        .and_then(|r| game.native_recipe_result(r))
        .map_or_else(Vec::new, |s| vec![Stack { item: s.item.to_string(), count: s.count as u32, components: String::new() }])
}

fn one(id: &str) -> Vec<Stack> {
    vec![Stack::item(id)]
}

/// `InscriberRecipeCategory`: top press, middle and bottom press over each other, then the result.
fn inscriber(game: &Game<'_>) -> Category {
    let recipes = game
        .native_recipes_of_type("ae2:inscriber")
        .iter()
        .map(|r| {
            let json = &*r.json;
            let ing = |slot: &str| alternatives(game, json.get("ingredients").and_then(|i| i.get(slot)));
            let press = json.get("mode").and_then(|m| m.as_str()) == Some("press");
            Ae2Recipe {
                slots: vec![ing("top"), ing("middle"), ing("bottom")],
                results: result(game, json),
                note: if press { "Press: uses the presses up" } else { "Inscribe: the presses stay" }.to_owned(),
            }
        })
        .collect();
    Category { icon: "ae2:inscriber", title: tr("block.ae2.inscriber", "Inscriber"), catalysts: &["ae2:inscriber"], layout: Layout::Grid, width: 1, height: 3, recipes }
}

/// `ChargerCategory`: the item on the charger's plate and what it becomes.
fn charger(game: &Game<'_>) -> Category {
    let recipes = game
        .native_recipes_of_type("ae2:charger")
        .iter()
        .map(|r| {
            let json = &*r.json;
            Ae2Recipe {
                slots: vec![alternatives(game, json.get("ingredient"))],
                results: result(game, json),
                note: text("charger_required_power", "%d turns or %d AE").replacen("%d", "10", 1).replacen("%d", "1600", 1),
            }
        })
        .collect();
    Category { icon: "ae2:charger", title: tr("block.ae2.charger", "Charger"), catalysts: &["ae2:charger", "ae2:crank"], layout: Layout::Single, width: 1, height: 1, recipes }
}

/// `TransformCategory`: items thrown together into water (or caught in an explosion) become the result.
fn transform(game: &Game<'_>) -> Category {
    let recipes = game
        .native_recipes_of_type("ae2:transform")
        .iter()
        .map(|r| {
            let json = &*r.json;
            let slots = json
                .get("ingredients")
                .and_then(|i| i.as_array())
                .map_or_else(Vec::new, |list| list.iter().map(|v| alternatives(game, Some(v))).collect());
            let circumstance = json.get("circumstance").and_then(|c| c.get("type")).and_then(|t| t.as_str());
            let note = if circumstance == Some("explosion") {
                text("explosion", "Explosion")
            } else {
                format!("{} {}", text("submerge_in", "Submerge in"), tr("block.minecraft.water", "Water"))
            };
            Ae2Recipe { slots, results: result(game, json), note }
        })
        .collect();
    Category { icon: "minecraft:water_bucket", title: text("transform_category", "In-World Transformation"), catalysts: &["minecraft:water_bucket"], layout: Layout::Grid, width: 0, height: 0, recipes }
}

/// `CertusGrowthCategory`: budding quartz grows buds, buds grow into clusters, clusters break into crystals;
/// imperfect budding quartz decays as it grows.
fn certus_growth() -> Category {
    let step = |from: &str, to: &str, note: &str| Ae2Recipe { slots: vec![one(from)], results: one(to), note: note.to_owned() };
    let mut recipes = Vec::new();
    let grows_buds = text("quartz_buds_grow_on_budding_quartz", "Quartz buds grow on budding quartz.");
    recipes.push(step("ae2:flawless_budding_quartz", "ae2:small_quartz_bud", &format!("{grows_buds}\n{}", text("flawless_budding_quartz_description", "Flawless budding quartz never decays when growing buds."))));
    for budding in ["flawed", "chipped", "damaged"] {
        recipes.push(step(&format!("ae2:{budding}_budding_quartz"), "ae2:small_quartz_bud", &grows_buds));
    }
    let accelerated = text("crystal_growth_accelerators_effect", "Powered crystal growth accelerators speed up the growth of adjacent budding quartz.");
    recipes.push(step("ae2:small_quartz_bud", "ae2:medium_quartz_bud", &accelerated));
    recipes.push(step("ae2:medium_quartz_bud", "ae2:large_quartz_bud", &accelerated));
    recipes.push(step("ae2:large_quartz_bud", "ae2:quartz_cluster", &accelerated));
    recipes.push(Ae2Recipe {
        slots: vec![one("ae2:quartz_cluster")],
        results: vec![Stack { item: "ae2:certus_quartz_crystal".into(), count: 4, components: String::new() }],
        note: format!("{}\n{}", text("fully_grown_buds_drop_crystals", "A fully grown quartz bud drops crystals."), text("fortune_applies", "Fortune enchantment applies")),
    });
    let decays = text("imperfect_budding_quartz_decays", "Imperfect budding quartz has a chance to decay when buds grow.");
    recipes.push(step("ae2:flawed_budding_quartz", "ae2:chipped_budding_quartz", &decays));
    recipes.push(step("ae2:chipped_budding_quartz", "ae2:damaged_budding_quartz", &decays));
    recipes.push(step("ae2:damaged_budding_quartz", "ae2:quartz_block", &decays));
    let stacks = |ids: &[&str]| -> Vec<Stack> { ids.iter().map(|id| Stack::item(id)).collect() };
    // BUDDING_QUARTZ_VARIANTS and BUDDING_QUARTZ_DECAY_ORDER: a slot's stacks turn in step with the other's.
    let variants = stacks(&["ae2:damaged_budding_quartz", "ae2:chipped_budding_quartz", "ae2:flawed_budding_quartz", "ae2:flawless_budding_quartz"]);
    let decay = stacks(&["ae2:quartz_block", "ae2:damaged_budding_quartz", "ae2:chipped_budding_quartz", "ae2:flawed_budding_quartz"]);
    // Page.BUD_LOOT.
    recipes.push(Ae2Recipe {
        slots: vec![stacks(&["ae2:small_quartz_bud", "ae2:medium_quartz_bud", "ae2:large_quartz_bud"])],
        results: one("ae2:certus_quartz_dust"),
        note: text("buds_drop_dust_when_not_fully_grown", "A quartz bud drops dust when not fully grown."),
    });
    // Page.BUDDING_QUARTZ_MOVING.
    recipes.push(Ae2Recipe {
        slots: vec![variants],
        results: decay.clone(),
        note: format!(
            "{}\n{}\n{}",
            text("budding_quartz_decays_when_broken", "Budding quartz decays when broken."),
            text("silk_touch_prevents_decay_for_imperfect", "Silk touch prevents decay for imperfect budding quartz."),
            text("spatial_io_never_causes_any_decay", "Spatial I/O never causes any decay.")
        ),
    });
    // Page.GETTING_BUDDING_QUARTZ.
    recipes.push(Ae2Recipe {
        slots: vec![decay, one("ae2:charged_certus_quartz_crystal"), one("ae2:meteorite_compass")],
        results: Vec::new(),
        note: text("budding_quartz_creation_and_worldgen", "Budding quartz can be found in meteorites, or be regenerated using charged quartz in water."),
    });
    Category { icon: "ae2:flawless_budding_quartz", title: text("certus_quartz_growth", "Certus Quartz Growth"), catalysts: &["ae2:growth_accelerator"], layout: Layout::Single, width: 1, height: 1, recipes }
}

/// `EntropyManipulatorCategory`: a block (or fluid) heated or cooled, and what it becomes or drops.
fn entropy(game: &Game<'_>) -> Category {
    // Water and lava as the fluids themselves (their animated texture), as AE2's JEI integration shows them.
    let as_stacks = |id: &str| match id {
        "minecraft:water" | "minecraft:flowing_water" => fluid("water", "Water"),
        "minecraft:lava" | "minecraft:flowing_lava" => fluid("lava", "Lava"),
        i => one(i),
    };
    let recipes = game
        .native_recipes_of_type("ae2:entropy")
        .iter()
        .filter_map(|r| {
            let j = &*r.json;
            let input = j.get("input")?;
            let id = input.get("block").or_else(|| input.get("fluid"))?.get("id")?.as_str()?;
            let out = j.get("output");
            let mut results: Vec<Stack> = Vec::new();
            if let Some(b) = out.and_then(|o| o.get("block")).and_then(|b| b.get("id")).and_then(|v| v.as_str())
                && b != "minecraft:air"
            {
                results.extend(as_stacks(b));
            }
            for d in out.and_then(|o| o.get("drops")).and_then(|d| d.as_array()).into_iter().flatten() {
                if let Some(i) = d.get("item").and_then(|i| i.as_str()) {
                    results.push(Stack::item(i));
                }
            }
            let mode = j.get("mode").and_then(|m| m.as_str()).unwrap_or("heat");
            // ENTROPY_MANIPULATOR_HEAT and _COOL, over RIGHT_CLICK and SHIFT_RIGHT_CLICK.
            let note = if mode == "cool" {
                format!("{}\n{}", text("entropy_manipulator_cool", "Cool (%d AE)").replace("%d", "1600"), text("shift_right_click", "Shift+Right-Click"))
            } else {
                format!("{}\n{}", text("entropy_manipulator_heat", "Heat (%d AE)").replace("%d", "1600"), text("right_click", "Right-Click"))
            };
            Some(Ae2Recipe { slots: vec![as_stacks(id)], results, note })
        })
        .collect();
    Category { icon: "ae2:entropy_manipulator", title: tr("item.ae2.entropy_manipulator", "Entropy Manipulator"), catalysts: &["ae2:entropy_manipulator"], layout: Layout::Single, width: 1, height: 1, recipes }
}

/// `AttunementCategory`, as JEIPlugin.registerP2PAttunement fills it: the kinds with an item API first (InitP2PAttunements:
/// every item that has it), then what each kind's `ae2:p2p_attunements/*` tag holds. A list with nothing in it goes.
fn attunement(game: &Game<'_>) -> Category {
    let mut recipes = Vec::new();
    for (kind, description) in [("fe_p2p_tunnel", tr("gui.ae2.P2PAttunementEnergy", "Portable Energy Storage (i.e. Batteries)")), ("fluid_p2p_tunnel", tr("gui.ae2.P2PAttunementFluid", "Portable Fluid Storage (i.e. Tanks, Buckets)"))] {
        let items: Vec<Stack> =
            game.res.items().registry_order.iter().map(|id| &**id).filter(|id| crate::game::port_ae2_me::attunement_api(id) == Some(kind)).map(Stack::item).collect();
        if !items.is_empty() {
            recipes.push(Ae2Recipe { slots: vec![items], results: one(&format!("ae2:{kind}")), note: format!("{} {description}", text("p2p_api_attunement", "Attune with any:")) });
        }
    }
    for kind in ["me_p2p_tunnel", "item_p2p_tunnel", "fluid_p2p_tunnel", "redstone_p2p_tunnel", "light_p2p_tunnel", "fe_p2p_tunnel"] {
        let items = alternatives(game, Some(&serde_json::Value::String(format!("#ae2:p2p_attunements/{kind}"))));
        if !items.is_empty() {
            recipes.push(Ae2Recipe { slots: vec![items], results: one(&format!("ae2:{kind}")), note: text("p2p_tag_attunement", "Attune with any shown item") });
        }
    }
    Category { icon: "ae2:me_p2p_tunnel", title: text("p2p_tunnel_attunement", "P2P Tunnel Attunement"), catalysts: &["ae2:me_p2p_tunnel"], layout: Layout::Single, width: 1, height: 1, recipes }
}

/// `CondenserCategory`: anything put in becomes matter balls or singularities, the component in it holding the power.
fn condenser() -> Category {
    let recipes = vec![
        // CondenserCategory.getTooltipStrings.
        Ae2Recipe { slots: vec![one("ae2:cell_component_1k")], results: one("ae2:matter_ball"), note: tr("gui.tooltips.ae2.MatterBalls", "Condense Into Matter Balls\n%s per item").replace("%s", "256") },
        Ae2Recipe { slots: vec![one("ae2:cell_component_64k")], results: one("ae2:singularity"), note: tr("gui.tooltips.ae2.Singularity", "Condense Into Singularities\n%s per item").replace("%s", "256,000") },
    ];
    Category { icon: "ae2:condenser", title: tr("block.ae2.condenser", "Matter Condenser"), catalysts: &["ae2:condenser"], layout: Layout::Single, width: 1, height: 1, recipes }
}

/// A fluid as AE2's JEI integration shows it: its still texture, animated (the content mod's `hz_fluid_*` model on
/// a bucket's stack), named as the fluid.
fn fluid(id: &str, name: &str) -> Vec<Stack> {
    vec![Stack {
        item: "minecraft:bucket".to_owned(),
        count: 1,
        components: format!("[minecraft:item_model=\"ae2:hz_fluid_{id}\",minecraft:item_name=\"{name}\"]"),
    }]
}

/// The fluids an ME network stores and how each is got. Horizon's fluids are the game's own two (water and lava);
/// a mod's fluid would join this list once Horizon lets mods add fluids.
fn fluids(game: &Game<'_>) -> Category {
    let step = |slots: Vec<Vec<Stack>>, results: Vec<Stack>, note: &str| Ae2Recipe { slots, results, note: note.to_owned() };
    let (water, lava) = (fluid("water", "Water"), fluid("lava", "Lava"));
    let mut recipes = vec![
        step(vec![one("minecraft:water_bucket")], water.clone(), "Empty a bucket (into the world, a tank or an ME terminal)"),
        step(vec![water.clone(), water.clone()], water.clone(), "Two sources beside each other make an endless source"),
        step(vec![one("minecraft:ice")], water.clone(), "Ice melts by light (not in the Nether)"),
        step(vec![one("minecraft:cauldron")], water.clone(), "Rain or snowfall fills a cauldron"),
        step(vec![water.clone(), one("minecraft:bucket")], one("minecraft:water_bucket"), "Fill a bucket from a source, a tank or an ME terminal"),
        step(vec![one("minecraft:lava_bucket")], lava.clone(), "Empty a bucket (into the world, a tank or an ME terminal)"),
        step(vec![one("minecraft:pointed_dripstone"), one("minecraft:cauldron")], lava.clone(), "Dripstone under lava drips it into a cauldron below"),
        step(vec![lava.clone(), one("minecraft:bucket")], one("minecraft:lava_bucket"), "Fill a bucket from a source, a tank or an ME terminal"),
    ];
    // AE2's entropy manipulator: what it turns into water or lava, and what they become.
    for r in game.native_recipes_of_type("ae2:entropy") {
        let json = &*r.json;
        let id_of = |v: Option<&serde_json::Value>| {
            let v = v?;
            v.get("block").or_else(|| v.get("fluid"))?.get("id")?.as_str().map(|s| s.replace("flowing_", ""))
        };
        let (input, output) = (id_of(json.get("input")), id_of(json.get("output")));
        let as_stack = |id: &str| match id {
            "minecraft:water" => water.clone(),
            "minecraft:lava" => lava.clone(),
            other => one(other),
        };
        if let (Some(i), Some(o)) = (input, output)
            && (i.ends_with(":water") || i.ends_with(":lava") || o.ends_with(":water") || o.ends_with(":lava"))
        {
            let mode = json.get("mode").and_then(|m| m.as_str()).unwrap_or("heat");
            recipes.push(step(vec![as_stack(&i)], as_stack(&o), &format!("Entropy Manipulator ({mode})")));
        }
    }
    // Stored by fluid cells in drives and ME chests, as an item network stores items.
    for size in ["1k", "4k", "16k", "64k", "256k"] {
        recipes.push(step(vec![water.clone()], one(&format!("ae2:fluid_storage_cell_{size}")), "Stored in a fluid cell"));
    }
    Category { icon: "minecraft:bucket", title: "Fluids".to_owned(), catalysts: &["minecraft:bucket"], layout: Layout::Grid, width: 0, height: 0, recipes }
}
});
