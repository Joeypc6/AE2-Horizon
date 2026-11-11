ae2_optional::if_kit_items!("ae2", {
// AE2's guide (`appeng.client.guidebook`): its Markdown pages, shipped in the content mod under
// data/ae2/ae2guide/ with an index of the items each page is about, its navigation and categories, shown over any
// screen. Hold the guide key (W unless rebound) over an item, in a slot or JEI's panel, to open its page; links move between
// pages, Backspace and Back go back, Escape and x close it.
//
// The pages are AE2's own Markdown with its tags: headings, paragraphs (bold, italic, code, links, item links,
// line breaks, config values), lists (nested, numbered), tables, quotes, code blocks, pictures, notes (`<div>`),
// item pictures and grids (`<ItemImage>`, `<BlockImage>`, `<ItemGrid>`, `<Row>`, `<Column>`), recipes
// (`<RecipeFor>`, `<Recipe>`), page lists (`<SubPages>`, `<CategoryIndex>`) and scenes (`<GameScene>`).
use crate::game::Game;
use horizon_client::hud::HudBatch;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

const LINE: f32 = 11.0;
/// The page's margin inside the panel.
const PAD: f32 = 12.0;
const TEXT: u32 = 0x3A3A3A;
const LINK: u32 = 0x2050C8;

#[derive(Clone)]
enum Link {
    Page(String),
    Item(String),
}

/// One laid-out band of the page: text (legacy-styled), item icons, filled boxes, pictures and scene blocks, each
/// placed from the band's top left.
#[derive(Clone, Default)]
struct Line {
    texts: Vec<(f32, f32, String, u32, Option<Link>)>,
    icons: Vec<(f32, f32, String, f32)>,
    fills: Vec<([f32; 4], u32)>,
    sprites: Vec<([f32; 4], String)>,
    /// A scene's blocks (AE2's GameScene): each one's place (x, y), size and item, back to front, as last drawn.
    scene: Vec<(f32, f32, f32, String)>,
    /// The scene in 3D: each block's place about the scene's middle and item, the size of a block on the page,
    /// where the middle is drawn and the view's turn (yaw, pitch in radians; dragging turns it).
    scene3d: Option<Scene3d>,
    height: f32,
}

#[derive(Clone, Default)]
struct Scene3d {
    blocks: Vec<([f32; 3], String)>,
    annotations: Vec<Annotation>,
    unit: f32,
    center: [f32; 2],
    yaw: f32,
    pitch: f32,
}

/// A scene's mark (`<LineAnnotation>`, `<BoxAnnotation>`, `<DiamondAnnotation>`), in the scene's coordinates about its
/// middle, with the text it shows under the pointer.
#[derive(Clone)]
struct Annotation {
    shape: AnnotationShape,
    color: u32,
    thickness: f32,
    tip: Vec<String>,
}

#[derive(Clone, Copy)]
enum AnnotationShape {
    Line([f32; 3], [f32; 3]),
    Box([f32; 3], [f32; 3]),
    Diamond([f32; 3]),
}

#[derive(Default)]
struct Guide {
    /// The mod whose pages are open or were last asked for, and every item's mod and page.
    ns: String,
    all_items: HashMap<String, (String, String)>,
    /// Since when `hovered`'s item has been under the pointer.
    hovered_since: Option<std::time::Instant>,
    /// The open screen, when it came up, and its last frame.
    screen: String,
    screen_since: Option<std::time::Instant>,
    screen_frame: Option<std::time::Instant>,
    /// Item id → page, page → title, and the navigation (parent, position, icon, categories) from index.json.
    items: HashMap<String, String>,
    titles: HashMap<String, String>,
    parents: HashMap<String, String>,
    positions: HashMap<String, i64>,
    icons: HashMap<String, String>,
    categories: HashMap<String, Vec<String>>,
    loaded: bool,
    open: Option<String>,
    history: Vec<String>,
    lines: Vec<Line>,
    scroll: f32,
    /// The panel's size, set as a page opens (it fits the window).
    size: [f32; 2],
    /// The item under the pointer and when (a tooltip shows it), for the guide key.
    hovered: Option<(String, std::time::Instant)>,
    /// A scene being dragged: its line, and whether the pointer moved (a press without a move opens the block's page).
    drag: Option<(usize, bool, String)>,
}

thread_local! {
    static GUIDE: RefCell<Guide> = RefCell::new(Guide::default());
    /// The window's recipes (crafting, cooking, smithing...), refreshed by the frame (it can't be made while
    /// drawing): what `<RecipeFor>` shows.
    static RECIPES: RefCell<Option<Arc<crate::game::native_hooks::NativeRecipes>>> = const { RefCell::new(None) };
}

/// The frame's part: the recipes the guide draws from.
pub(in crate::game) fn set_recipes(recipes: Option<Arc<crate::game::native_hooks::NativeRecipes>>) {
    if recipes.is_some() {
        RECIPES.with_borrow_mut(|r| *r = recipes);
    }
}

/// Whether the guide still waits for the recipes (the frame fetches them).
pub(in crate::game) fn needs_recipes() -> bool {
    RECIPES.with_borrow(|r| r.is_none())
}

/// Reads the open mod's index.json once: items, titles, navigation, without the pages whose other mods are missing.
fn load_index(game: &Game<'_>) {
    if GUIDE.with_borrow(|g| g.loaded) {
        return;
    }
    let ns = GUIDE.with_borrow(|g| g.ns.clone());
    let Some(bytes) = game.native_data(&format!("data/{ns}/ae2guide/index.json")) else { return };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return };
    let map = |key: &str| -> HashMap<String, String> {
        v.get(key)
            .and_then(|o| o.as_object())
            .into_iter()
            .flatten()
            .filter_map(|(k, p)| Some((k.clone(), p.as_str()?.to_owned())))
            .collect()
    };
    // A page about working with other mods (`requires:` in its front matter) is there only when they all are.
    let hidden: Vec<String> = v
        .get("requires")
        .and_then(|o| o.as_object())
        .into_iter()
        .flatten()
        .filter(|(_, ids)| !present(game, ids.as_array().into_iter().flatten().filter_map(|x| x.as_str())))
        .map(|(page, _)| page.clone())
        .collect();
    GUIDE.with_borrow_mut(|g| {
        g.loaded = true;
        g.items = map("items");
        g.titles = map("titles");
        // The page titles in the game's language, where the index has them (`titles_<lang>`).
        for (page, title) in map(&format!("titles_{}", horizon_client::lang::code())) {
            g.titles.insert(page, title);
        }
        g.parents = map("parents");
        g.icons = map("icons");
        g.positions = map("positions").into_iter().filter_map(|(k, p)| Some((k, p.parse().ok()?))).collect();
        g.categories = v
            .get("categories")
            .and_then(|o| o.as_object())
            .into_iter()
            .flatten()
            .map(|(k, c)| (k.clone(), c.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_owned)).collect()))
            .collect();
        for page in &hidden {
            g.titles.remove(page);
            g.parents.remove(page);
            g.positions.remove(page);
            g.icons.remove(page);
            g.items.retain(|_, p| p != page);
            g.parents.retain(|_, p| p != page);
            for list in g.categories.values_mut() {
                list.retain(|p| p != page);
            }
        }
    });
}

/// A page of the open mod in the game's language (`ae2guide/_<lang>/<page>`, as AE2's guide keeps its translations),
/// else as written.
fn page_data(game: &Game<'_>, page: &str) -> Option<Vec<u8>> {
    let ns = current_ns();
    let lang = horizon_client::lang::code();
    if lang != "en_us"
        && let Some(bytes) = game.native_data(&format!("data/{ns}/ae2guide/_{lang}/{page}"))
    {
        return Some(bytes);
    }
    game.native_data(&format!("data/{ns}/ae2guide/{page}"))
}

/// Whether every item of `ids` is in the game with its mod running: an item of another mod stands for that mod (its
/// namespace is its native id). The item alone isn't enough: a content pack can bring the items without the mod.
fn present<'a>(game: &Game<'_>, ids: impl IntoIterator<Item = &'a str>) -> bool {
    ids.into_iter().all(|id| {
        let id = item_id(id);
        let ns = id.split_once(':').map_or("", |(ns, _)| ns);
        game.res.items().get(&id).is_some() && (ns == "minecraft" || game.native_on(ns))
    })
}

/// A page's `<IfMods items="a:x,b:y">...</IfMods>` parts: kept (without the tags) when those items are all in the game,
/// dropped when one is missing, so a page shows how mods work together only when they are installed together.
fn only_present(game: &Game<'_>, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<IfMods") {
        out.push_str(&rest[..at]);
        let tag_end = rest[at..].find('>').map_or(rest.len(), |e| at + e + 1);
        let tag = &rest[at..tag_end];
        let body_end = rest[tag_end..].find("</IfMods>").map_or(rest.len(), |e| tag_end + e);
        let ids = attr(tag, "items").unwrap_or_default();
        if present(game, ids.split(',').map(str::trim).filter(|s| !s.is_empty())) {
            out.push_str(&rest[tag_end..body_end]);
        }
        rest = rest.get(body_end + "</IfMods>".len()..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// Whether a page is about `item` (the tooltip's hint).
pub(in crate::game) fn has_page(item: &str) -> bool {
    GUIDE.with_borrow(|g| g.all_items.contains_key(item))
}

/// The tooltip saw `item` under the pointer.
pub(in crate::game) fn hovered(item: &str) {
    let now = std::time::Instant::now();
    GUIDE.with_borrow_mut(|g| {
        // How long this one has been under the pointer, without a gap.
        let same = g.hovered.as_ref().is_some_and(|(i, at)| i == item && now.duration_since(*at).as_millis() < 150);
        if !same {
            g.hovered_since = Some(now);
        }
        g.hovered = Some((item.to_owned(), now));
    });
}

/// A frame of an open screen: when this screen came up, which the W key waits on.
pub(in crate::game) fn screen_frame(id: &str, menu: Option<&str>) {
    let now = std::time::Instant::now();
    let which = format!("{id} {}", menu.unwrap_or(""));
    GUIDE.with_borrow_mut(|g| {
        if g.screen != which || g.screen_frame.is_none_or(|at| now.duration_since(at).as_millis() > 250) {
            g.screen = which;
            g.screen_since = Some(now);
        }
        g.screen_frame = Some(now);
    });
}

/// Whether a guide page is open.
pub(in crate::game) fn is_open() -> bool {
    GUIDE.with_borrow(|g| g.open.is_some())
}

/// Opens the page about `item`, if there is one.
pub(in crate::game) fn open_item(game: &Game<'_>, item: &str) -> bool {
    load_all(game);
    let Some((ns, page)) = GUIDE.with_borrow(|g| g.all_items.get(item).cloned()) else { return false };
    use_mod(&ns);
    open_page(game, &page, true);
    true
}

/// The panel's size for the window: as large as AE2's guide window, never past the window's edges.
fn panel_size(game: &Game<'_>) -> [f32; 2] {
    let (w, h) = game.gui_size();
    [(w - 24.0).clamp(220.0, 430.0), (h - 24.0).clamp(140.0, 330.0)]
}

/// Opens a page of the open mod, laid out for the window (`push`: Back returns to the page before).
fn open_page(game: &Game<'_>, page: &str, push: bool) {
    load_index(game);
    let Some(bytes) = page_data(game, &page) else { return };
    let text = only_present(game, &String::from_utf8_lossy(&bytes));
    let size = panel_size(game);
    let (lines, _) = layout(game, page, &text, size[0] - 2.0 * PAD - 8.0);
    GUIDE.with_borrow_mut(|g| {
        if push && let Some(cur) = g.open.take() {
            g.history.push(cur);
        }
        g.open = Some(page.to_owned());
        g.lines = lines;
        g.scroll = 0.0;
        g.size = size;
    });
}

/// The open mod's first page: `index.md`, else the page no other page is under, first in the index's order.
fn root_page() -> Option<String> {
    GUIDE.with_borrow(|g| {
        if g.titles.contains_key("index.md") {
            return Some("index.md".to_owned());
        }
        g.titles.keys().filter(|p| !g.parents.contains_key(*p)).min_by_key(|p| (g.positions.get(*p).copied().unwrap_or(i64::MAX), (*p).clone())).cloned()
    })
}

/// The title bar's "< Back" and "Contents", in the game's language, each with its left edge: right-aligned before the
/// close cross, so a longer word in another language moves the buttons rather than overlapping them.
fn header_buttons(game: &Game<'_>, x: f32, pw: f32) -> ((String, f32), (String, f32)) {
    let back = format!("< {}", horizon_client::lang::get_or("gui.back", "Back"));
    let contents = horizon_client::lang::get_or("gui.ae2guide.contents", contents_word()).to_string();
    let back_x = x + pw - 22.0 - game.native_text_width(&back);
    let contents_x = back_x - 14.0 - game.native_text_width(&contents);
    ((back, back_x), (contents, contents_x))
}

/// "Contents" in the languages the ports' guides are written in (vanilla has no such word; a pack's
/// `gui.ae2guide.contents` wins).
fn contents_word() -> &'static str {
    match horizon_client::lang::code().as_str() {
        "de_de" => "Inhalt",
        "fr_fr" | "fr_ca" => "Sommaire",
        "es_es" | "es_mx" | "es_ar" => "Índice",
        "it_it" => "Indice",
        "pt_br" | "pt_pt" => "Conteúdo",
        "nl_nl" => "Inhoud",
        "pl_pl" => "Spis treści",
        "ru_ru" => "Содержание",
        "uk_ua" => "Зміст",
        "tr_tr" => "İçindekiler",
        "ja_jp" => "目次",
        "ko_kr" => "목차",
        "zh_cn" => "目录",
        "zh_tw" | "zh_hk" => "目錄",
        _ => "Contents",
    }
}

/// Back a page, or shut the guide on the first.
fn back(game: &Game<'_>) {
    let prev = GUIDE.with_borrow_mut(|g| g.history.pop());
    match prev {
        Some(p) => open_page(game, &p, false),
        None => close(),
    }
}

/// Shuts the guide and forgets its history.
fn close() {
    GUIDE.with_borrow_mut(|g| {
        g.open = None;
        g.lines.clear();
        g.history.clear();
    });
}

/// A page path relative to `from`'s folder, `..` resolved (`ae2:` paths from the guide's root).
fn resolve(from: &str, target: &str) -> String {
    let target = target.split('#').next().unwrap_or(target);
    if let Some(abs) = target.strip_prefix(&format!("{}:", current_ns())) {
        return abs.to_owned();
    }
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for t in target.split('/') {
        match t {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            t => parts.push(t),
        }
    }
    parts.join("/")
}

/// An item id as pages write it, with the open mod's namespace when it has none.
fn item_id(id: &str) -> String {
    if id.contains(':') { id.to_owned() } else { format!("{}:{id}", current_ns()) }
}

/// An attribute of a tag: `id="x"` (or `id={x}`).
fn attr(tag: &str, name: &str) -> Option<String> {
    for (open, close) in [("=\"", '"'), ("={", '}')] {
        let key = format!(" {name}{open}");
        if let Some(at) = tag.find(&key).map(|a| a + key.len())
            && let Some(end) = tag[at..].find(close)
        {
            return Some(tag[at..at + end].to_owned());
        }
    }
    None
}

/// AE2's config values the pages quote (`<ae2:ConfigValue name=".."/>`), at their defaults.
fn config_value(name: &str) -> Option<&'static str> {
    match name {
        "crystalResonanceGeneratorRate" => Some("20"),
        _ => None,
    }
}

/// A word of running text with its look.
#[derive(Clone)]
struct Word {
    text: String,
    colour: u32,
    link: Option<Link>,
    bold: bool,
    italic: bool,
    /// A forced line break after it (`<br/>`).
    br: bool,
}

/// Inline Markdown and AE2's inline tags as words: **bold**, *italic*, `code`, [links](page.md), <ItemLink/>,
/// <ae2:ConfigValue/>, <br/>; other inline tags are left out.
fn inline(game: &Game<'_>, page: &str, s: &str, base: u32, problems: &mut Vec<String>) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    let (mut bold, mut italic, mut code) = (false, false, false);
    let mut cur = String::new();
    let cur_link: Option<Link> = None;
    let mut colour_override: Option<u32> = None;
    let push_word = |out: &mut Vec<Word>, cur: &mut String, bold: bool, italic: bool, code: bool, link: &Option<Link>, colour: Option<u32>| {
        if cur.is_empty() {
            return;
        }
        let colour = colour.unwrap_or(if link.is_some() { LINK } else if code { 0x8A2E7E } else { base });
        out.push(Word { text: std::mem::take(cur), colour, link: link.clone(), bold, italic, br: false });
    };
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let text_at = |i: usize| chars[i..].iter().collect::<String>();
    while i < chars.len() {
        let c = chars[i];
        let rest = || text_at(i);
        if c == '\\' && i + 1 < chars.len() {
            cur.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '`' {
            push_word(&mut out, &mut cur, bold, italic, code, &cur_link, colour_override);
            code = !code;
            i += 1;
            continue;
        }
        if code {
            if c == ' ' {
                push_word(&mut out, &mut cur, bold, italic, code, &cur_link, colour_override);
            } else {
                cur.push(c);
            }
            i += 1;
            continue;
        }
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            push_word(&mut out, &mut cur, bold, italic, code, &cur_link, colour_override);
            bold = !bold;
            i += 2;
            continue;
        }
        if c == '*' || (c == '_' && (i == 0 || chars[i - 1] == ' ' || chars.get(i + 1).is_none_or(|n| *n == ' ' || n.is_ascii_punctuation()))) {
            push_word(&mut out, &mut cur, bold, italic, code, &cur_link, colour_override);
            italic = !italic;
            i += 1;
            continue;
        }
        if c == '[' {
            let r = rest();
            if let Some(close) = r.find("](")
                && let Some(end) = r[close..].find(')')
            {
                push_word(&mut out, &mut cur, bold, italic, code, &cur_link, colour_override);
                let label = &r[1..close];
                let target = &r[close + 2..close + end];
                let link = if target.ends_with(".md") || target.contains(".md#") {
                    Some(Link::Page(resolve(page, target)))
                } else {
                    None
                };
                let mut words = inline(game, page, label, if link.is_some() || target.starts_with("http") { LINK } else { base }, problems);
                for w in &mut words {
                    w.bold |= bold;
                    w.italic |= italic;
                    if link.is_some() {
                        w.link = link.clone();
                    }
                }
                out.extend(words);
                i += r[..close + end + 1].chars().count();
                continue;
            }
        }
        if c == '<' {
            let r = rest();
            let end = r.find('>').map_or(r.len(), |e| e + 1);
            let tag = &r[..end];
            let name: String = tag.trim_start_matches(['<', '/']).chars().take_while(|c| c.is_alphanumeric() || *c == ':').collect();
            push_word(&mut out, &mut cur, bold, italic, code, &cur_link, colour_override);
            match name.as_str() {
                "ItemLink" => {
                    if let Some(id) = attr(tag, "id") {
                        let id = item_id(&id);
                        if game.res.items().get(&id).is_none() {
                            problems.push(format!("ItemLink to an unknown item {id}"));
                        }
                        for w in item_words(&id).split_whitespace() {
                            out.push(Word { text: w.to_owned(), colour: LINK, link: Some(Link::Item(id.clone())), bold, italic, br: false });
                        }
                    }
                }
                "ae2:ConfigValue" => {
                    let name = attr(tag, "name").unwrap_or_default();
                    match config_value(&name) {
                        Some(v) => out.push(Word { text: v.to_owned(), colour: base, link: None, bold, italic, br: false }),
                        None => problems.push(format!("ConfigValue {name} unknown")),
                    }
                }
                "br" => {
                    if let Some(last) = out.last_mut() {
                        last.br = true;
                    }
                }
                "Color" => colour_override = if tag.starts_with("</") { None } else { attr(tag, "color").and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok()) },
                "a" | "span" | "sub" | "sup" | "kbd" | "b" | "i" | "strong" | "em" => {
                    if matches!(name.as_str(), "b" | "strong") {
                        bold = !tag.starts_with("</");
                    }
                    if matches!(name.as_str(), "i" | "em") {
                        italic = !tag.starts_with("</");
                    }
                }
                other if !other.is_empty() => problems.push(format!("inline tag <{other}> left out")),
                _ => cur.push('<'),
            }
            i += tag.chars().count().max(1);
            continue;
        }
        if c == ' ' || c == '\n' || c == '\t' {
            push_word(&mut out, &mut cur, bold, italic, code, &cur_link, colour_override);
            i += 1;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    push_word(&mut out, &mut cur, bold, italic, code, &cur_link, colour_override);
    out
}

/// A word with its bold and italic codes.
fn styled(w: &Word) -> String {
    let mut s = String::new();
    if w.bold {
        s.push_str("§l");
    }
    if w.italic {
        s.push_str("§o");
    }
    s.push_str(&w.text);
    s
}

/// A word's width in the font, styled.
fn word_width(game: &Game<'_>, w: &Word) -> f32 {
    game.native_text_width(&w.text) + if w.bold { w.text.chars().count() as f32 } else { 0.0 }
}

/// Words flowed into lines from `indent` to `width` (a hanging indent for list items).
fn flow(game: &Game<'_>, words: &[Word], indent: f32, width: f32, out: &mut Vec<Line>) {
    if words.is_empty() {
        return;
    }
    let space = game.native_text_width(" ");
    let mut line = Line { height: LINE, ..Default::default() };
    let mut x = indent;
    for w in words {
        let ww = word_width(game, w);
        if x + ww > width && x > indent {
            out.push(std::mem::replace(&mut line, Line { height: LINE, ..Default::default() }));
            x = indent;
        }
        line.texts.push((x, 0.0, styled(w), w.colour, w.link.clone()));
        x += ww + space;
        if w.br {
            out.push(std::mem::replace(&mut line, Line { height: LINE, ..Default::default() }));
            x = indent;
        }
    }
    if !line.texts.is_empty() {
        out.push(line);
    }
}

/// Vertical space between blocks of a page.
fn gap(out: &mut Vec<Line>, h: f32) {
    if out.last().is_some_and(|l| l.texts.is_empty() && l.icons.is_empty() && l.fills.is_empty() && l.sprites.is_empty() && l.scene.is_empty()) {
        return;
    }
    if !out.is_empty() {
        out.push(Line { height: h, ..Default::default() });
    }
}

/// A slot's frame for recipes and item grids (AE2's guide draws vanilla's slot).
fn slot(line: &mut Line, x: f32, y: f32) {
    line.fills.push(([x - 1.0, y - 1.0, 18.0, 18.0], 0xFF373737));
    line.fills.push(([x, y, 17.0, 17.0], 0xFFFFFFFF));
    line.fills.push(([x, y, 16.0, 16.0], 0xFF8B8B8B));
}

/// The ids of an ingredient as a recipe's JSON names it (an item, a tag, a list), the first one shown.
fn ingredient_ids(game: &Game<'_>, v: &serde_json::Value) -> Vec<String> {
    horizon_client::crafting::Ingredient::parse(v, game.res.items()).map(|i| i.order).unwrap_or_default()
}

/// A recipe's result: item and count.
fn result_of(v: &serde_json::Value) -> Option<(String, i64)> {
    let r = v.get("result")?;
    let id = r.get("id").or_else(|| r.get("item")).and_then(|x| x.as_str())?.to_owned();
    Some((id, r.get("count").and_then(|c| c.as_i64()).unwrap_or(1)))
}

/// A recipe box: its station's name, its inputs in their layout, an arrow, what it makes.
fn recipe_box(game: &Game<'_>, title: &str, cells: &[Vec<String>], width: usize, result: &(String, i64)) -> Line {
    let mut line = Line::default();
    line.fills.push(([0.0, 0.0, 0.0, 0.0], 0));
    line.texts.push((2.0, 1.0, format!("§o{title}"), 0x606060, None));
    let rows = cells.len().div_ceil(width.max(1));
    let top = 12.0;
    for (i, ids) in cells.iter().enumerate() {
        let (x, y) = (3.0 + 18.0 * (i % width) as f32, top + 18.0 * (i / width) as f32);
        slot(&mut line, x, y);
        if let Some(id) = ids.first() {
            line.icons.push((x, y, id.clone(), 16.0));
        }
    }
    let ax = 3.0 + 18.0 * width as f32 + 6.0;
    let ay = top + 18.0 * (rows.max(1) - 1) as f32 / 2.0;
    line.texts.push((ax, ay + 4.0, "→".to_owned(), 0x606060, None));
    let rx = ax + game.native_text_width("→") + 8.0;
    slot(&mut line, rx, ay);
    line.icons.push((rx, ay, result.0.clone(), 16.0));
    if result.1 > 1 {
        let l = result.1.to_string();
        line.texts.push((rx + 17.0 - game.native_text_width(&l), ay + 9.0, l, 0xFFFFFF, None));
    }
    // The box behind it all.
    let w = rx + 22.0;
    let h = top + 18.0 * rows.max(1) as f32 + 4.0;
    line.fills[0] = ([0.0, 0.0, w, h], 0xFFD6D6CE);
    line.height = h + 4.0;
    line
}

/// AE2's `<RecipeFor id=..>` (every recipe that makes the item) or `<Recipe id=..>` (one recipe by its id).
fn recipes(game: &Game<'_>, item: Option<&str>, recipe_id: Option<&str>) -> Vec<Line> {
    let mut out = Vec::new();
    // `<Recipe id>` names it without its namespace (`network/cables/smart_fluix`); recipes carry theirs.
    let wanted = |id: &str, rid: &str| item.is_some_and(|i| i == id) || recipe_id.is_some_and(|r| rid == r || rid.ends_with(&format!(":{r}")));
    RECIPES.with_borrow(|catalog| {
        let Some(catalog) = catalog else { return };
        let name = |i: u32| catalog.items.get(i as usize).map(|x| x.id.clone()).unwrap_or_default();
        for r in &catalog.recipes {
            let Some(first) = r.results.first() else { continue };
            let result = name(first.item);
            if !wanted(&result, &r.id) {
                continue;
            }
            use crate::game::native_hooks::NativeRecipeKind as K;
            let title = match r.kind {
                K::CraftingShaped | K::CraftingShapeless => "Crafting",
                K::Smelting => "Smelting",
                K::Blasting => "Blasting",
                K::Smoking => "Smoking",
                K::Campfire => "Campfire",
                K::Stonecutting => "Stonecutting",
                K::Smithing => "Smithing",
                K::Brewing => "Brewing",
            };
            let width = if r.kind == K::CraftingShaped { r.width.max(1) as usize } else if r.slots.len() > 3 { 3 } else { r.slots.len().max(1) };
            let cells: Vec<Vec<String>> = r.slots.iter().map(|s| s.iter().map(|x| name(x.item)).collect()).collect();
            out.push(recipe_box(game, title, &cells, width, &(result, i64::from(first.count))));
        }
    });
    // AE2's own machines: the inscriber (top, middle, bottom), the charger, in-world transformations.
    for (kind, title) in [("ae2:inscriber", "Inscriber"), ("ae2:charger", "Charger"), ("ae2:transform", "Transformation"), ("ae2:entropy", "Entropy Manipulator")] {
        for r in game.native_recipes_of_type(kind) {
            let Some(result) = result_of(&r.json) else { continue };
            if !wanted(&result.0, &r.id) {
                continue;
            }
            let cells: Vec<Vec<String>> = match kind {
                "ae2:inscriber" => ["top", "middle", "bottom"]
                    .iter()
                    .map(|k| r.json.get("ingredients").and_then(|i| i.get(*k)).map(|v| ingredient_ids(game, v)).unwrap_or_default())
                    .collect(),
                "ae2:charger" => vec![r.json.get("ingredient").map(|v| ingredient_ids(game, v)).unwrap_or_default()],
                _ => r.json.get("ingredients").and_then(|i| i.as_array()).into_iter().flatten().map(|v| ingredient_ids(game, v)).collect(),
            };
            let width = if kind == "ae2:inscriber" { 1 } else { cells.len().clamp(1, 3) };
            out.push(recipe_box(game, title, &cells, width, &result));
        }
    }
    out
}

/// Lines side by side (a `<Row>`): each one's pieces moved right, the row as tall as the tallest.
fn side_by_side(parts: Vec<Line>, width: f32, out: &mut Vec<Line>) {
    let mut row = Line::default();
    let mut x = 0.0f32;
    for p in parts {
        let w = p
            .fills
            .iter()
            .map(|(r, _)| r[0] + r[2])
            .chain(p.icons.iter().map(|i| i.0 + i.3))
            .chain(p.sprites.iter().map(|(r, _)| r[0] + r[2]))
            .fold(0.0f32, f32::max)
            .max(16.0);
        if x > 0.0 && x + w > width {
            out.push(std::mem::take(&mut row));
            x = 0.0;
        }
        row.texts.extend(p.texts.into_iter().map(|(px, py, t, c, l)| (px + x, py, t, c, l)));
        row.icons.extend(p.icons.into_iter().map(|(px, py, id, s)| (px + x, py, id, s)));
        row.fills.extend(p.fills.into_iter().map(|(r, c)| ([r[0] + x, r[1], r[2], r[3]], c)));
        row.sprites.extend(p.sprites.into_iter().map(|(r, n)| ([r[0] + x, r[1], r[2], r[3]], n)));
        row.height = row.height.max(p.height);
        x += w + 8.0;
    }
    if row.height > 0.0 {
        out.push(row);
    }
}

/// A picture of the page (`![alt](path.png)`, shipped as `ae2:gui/guide/<path>`), at most the page's width.
fn picture(game: &Game<'_>, page: &str, src: &str, width: f32, problems: &mut Vec<String>) -> Option<Line> {
    let path = resolve(page, src);
    let name = format!("{}:gui/guide/{}", current_ns(), path.trim_end_matches(".png"));
    let Some([w, h]) = game.native_sprite_size(&name) else {
        problems.push(format!("picture {path} missing"));
        return None;
    };
    let scale = (width / w).min(1.0).min(160.0 / h);
    let (dw, dh) = (w * scale, h * scale);
    Some(Line { sprites: vec![([(width - dw) / 2.0, 0.0, dw, dh], name)], height: dh + 4.0, ..Default::default() })
}

/// Pages listed as links, by position then title (`<SubPages>`, `<CategoryIndex>`).
fn page_list(game: &Game<'_>, pages: Vec<String>, width: f32, out: &mut Vec<Line>) {
    let mut pages: Vec<(i64, String, String, Option<String>)> = GUIDE.with_borrow(|g| {
        pages
            .into_iter()
            .map(|p| {
                let title = g.titles.get(&p).cloned().unwrap_or_else(|| p.clone());
                (g.positions.get(&p).copied().unwrap_or(i64::MAX), title, p.clone(), g.icons.get(&p).cloned())
            })
            .collect()
    });
    pages.sort();
    for (_, title, page, icon) in pages {
        let mut line = Line { height: 18.0, ..Default::default() };
        if let Some(icon) = icon {
            line.icons.push((4.0, 0.0, item_id(&icon), 16.0));
        }
        let words: Vec<Word> = title
            .split_whitespace()
            .map(|t| Word { text: t.to_owned(), colour: LINK, link: Some(Link::Page(page.clone())), bold: false, italic: false, br: false })
            .collect();
        let mut tmp = Vec::new();
        flow(game, &words, 24.0, width, &mut tmp);
        for (i, mut l) in tmp.into_iter().enumerate() {
            if i == 0 {
                line.texts = l.texts.into_iter().map(|(x, _, t, c, k)| (x, 4.0, t, c, k)).collect();
                out.push(std::mem::take(&mut line));
            } else {
                l.height = LINE;
                out.push(l);
            }
        }
    }
}

/// A block of tags and text up to its closing tag (`<Row>`..`</Row>`), from line `i`.
fn tag_block(lines: &[&str], i: usize, name: &str) -> (String, usize) {
    let mut body = String::new();
    let mut j = i;
    let first = lines[i].trim();
    if first.ends_with("/>") && first.starts_with(&format!("<{name}")) {
        return (first.to_owned(), i + 1);
    }
    let mut depth = 0;
    while j < lines.len() {
        let l = lines[j];
        depth += l.matches(&format!("<{name}")).count() as i32;
        depth -= l.matches(&format!("</{name}>")).count() as i32;
        body.push_str(l);
        body.push('\n');
        j += 1;
        if depth <= 0 {
            break;
        }
    }
    (body, j)
}

/// The tags in a block, each `<Tag .../>` or `<Tag ...>` in order.
fn tags_in(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(a) = rest.find('<') {
        let Some(e) = rest[a..].find('>') else { break };
        out.push(rest[a..a + e + 1].to_owned());
        rest = &rest[a + e + 1..];
    }
    out
}

/// Item pictures and recipes of a `<Row>` or `<ItemGrid>`'s tags, each as a piece placed side by side.
fn pieces(game: &Game<'_>, page: &str, tags: &[String], width: f32, problems: &mut Vec<String>) -> Vec<Line> {
    let mut parts = Vec::new();
    for t in tags {
        let name: String = t.trim_start_matches('<').chars().take_while(|c| c.is_alphanumeric()).collect();
        match name.as_str() {
            "ItemImage" | "BlockImage" | "ItemIcon" => {
                let Some(id) = attr(t, "id").map(|i| item_id(&i)) else { continue };
                if game.res.items().get(&id).is_none() {
                    problems.push(format!("{name} of an unknown item {id}"));
                }
                let scale: f32 = attr(t, "scale").and_then(|s| s.parse().ok()).unwrap_or(if name == "BlockImage" { 2.0 } else { 1.0 });
                let size = if name == "ItemIcon" { 16.0 } else { (16.0 * scale).clamp(16.0, 64.0) };
                let id = if name == "BlockImage" {
                    let mut props = Vec::new();
                    let mut rest = t.as_str();
                    while let Some(at) = rest.find(" p:") {
                        rest = &rest[at + 3..];
                        if let Some((k, after)) = rest.split_once("=\"")
                            && let Some((v, _)) = after.split_once('"')
                        {
                            let k = if k == "fullness" { "stage" } else { k };
                            props.push(format!("{k}={v}"));
                        }
                    }
                    format!("{id}|{}", props.join(";"))
                } else {
                    id
                };
                let mut l = Line { height: size + 4.0, ..Default::default() };
                if name == "ItemIcon" {
                    slot(&mut l, 1.0, 1.0);
                    l.icons.push((1.0, 1.0, id, 16.0));
                } else {
                    l.icons.push((0.0, 0.0, id, size));
                }
                parts.push(l);
            }
            "RecipeFor" | "Recipe" => {
                let found = if name == "RecipeFor" {
                    recipes(game, attr(t, "id").map(|i| item_id(&i)).as_deref(), None)
                } else {
                    recipes(game, None, attr(t, "id").as_deref())
                };
                if found.is_empty() {
                    problems.push(format!("{name} {} found no recipe", attr(t, "id").unwrap_or_default()));
                }
                parts.extend(found);
            }
            _ => {}
        }
    }
    let _ = (page, width);
    parts
}

/// The page's Markdown laid out for a page `width` wide, and what in it couldn't be shown (for the audit).
fn layout(game: &Game<'_>, page: &str, text: &str, width: f32) -> (Vec<Line>, Vec<String>) {
    let mut problems = Vec::new();
    let mut body = text;
    if let Some(rest) = text.strip_prefix("---")
        && let Some(end) = rest.find("\n---")
    {
        body = &rest[end + 4..];
    }
    let lines: Vec<&str> = body.lines().collect();
    let mut out: Vec<Line> = Vec::new();
    let mut para: Vec<Word> = Vec::new();
    // The list item being gathered: indent level, its marker, its words.
    let mut item: Option<(usize, String, Vec<Word>)> = None;
    let flush = |para: &mut Vec<Word>, item: &mut Option<(usize, String, Vec<Word>)>, out: &mut Vec<Line>| {
        if !para.is_empty() {
            flow(game, para, 0.0, width, out);
            para.clear();
            gap(out, 4.0);
        }
        if let Some((level, marker, words)) = item.take() {
            let indent = 10.0 * level as f32;
            let mut tmp = Vec::new();
            let mw = game.native_text_width(&marker) + 4.0;
            flow(game, &words, indent + mw.max(10.0), width, &mut tmp);
            if let Some(first) = tmp.first_mut() {
                first.texts.insert(0, (indent, 0.0, marker, 0x606060, None));
            }
            out.extend(tmp);
        }
    };
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        let l = raw.trim();
        // Code blocks.
        if l.starts_with("```") {
            flush(&mut para, &mut item, &mut out);
            i += 1;
            while i < lines.len() && !lines[i].trim().starts_with("```") {
                out.push(Line {
                    texts: vec![(6.0, 1.0, lines[i].to_owned(), 0x303030, None)],
                    fills: vec![([0.0, 0.0, width, LINE], 0xFFDCDCD4)],
                    height: LINE,
                    ..Default::default()
                });
                i += 1;
            }
            i += 1;
            gap(&mut out, 4.0);
            continue;
        }
        if l.is_empty() {
            flush(&mut para, &mut item, &mut out);
            i += 1;
            continue;
        }
        // Headings.
        if l.starts_with('#') {
            flush(&mut para, &mut item, &mut out);
            let level = l.chars().take_while(|c| *c == '#').count();
            let words = inline(game, page, l.trim_start_matches('#').trim(), [0x1E1E5A, 0x1E1E5A, 0x2E2E6A, 0x404040][level.min(3)], &mut problems);
            let words: Vec<Word> = words.into_iter().map(|w| Word { bold: true, ..w }).collect();
            gap(&mut out, 4.0);
            flow(game, &words, 0.0, width, &mut out);
            if level <= 2
                && let Some(last) = out.last_mut()
            {
                last.fills.push(([0.0, LINE - 1.0, width, 1.0], 0xFF9090B0));
                last.height = LINE + 3.0;
            }
            gap(&mut out, 3.0);
            i += 1;
            continue;
        }
        // Rules.
        if l == "---" || l == "***" {
            flush(&mut para, &mut item, &mut out);
            out.push(Line { fills: vec![([0.0, 3.0, width, 1.0], 0xFFA0A0A0)], height: 8.0, ..Default::default() });
            i += 1;
            continue;
        }
        // Tables.
        if l.starts_with('|') {
            flush(&mut para, &mut item, &mut out);
            let mut rows: Vec<Vec<String>> = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('|') {
                let r = lines[i].trim().trim_matches('|');
                let cells: Vec<String> = r.split('|').map(|c| c.trim().to_owned()).collect();
                if !cells.iter().all(|c| c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))) {
                    rows.push(cells);
                }
                i += 1;
            }
            let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
            if cols == 0 {
                continue;
            }
            let col_w = width / cols as f32;
            for (r, row) in rows.iter().enumerate() {
                let mut cells_lines: Vec<Vec<Line>> = Vec::new();
                for c in 0..cols {
                    let mut words = inline(game, page, row.get(c).map_or("", String::as_str), TEXT, &mut problems);
                    if r == 0 {
                        words.iter_mut().for_each(|w| w.bold = true);
                    }
                    let mut tmp = Vec::new();
                    flow(game, &words, 0.0, col_w - 6.0, &mut tmp);
                    cells_lines.push(tmp);
                }
                let h = cells_lines.iter().map(|c| c.len()).max().unwrap_or(1).max(1) as f32 * LINE + 4.0;
                let mut line = Line { height: h, ..Default::default() };
                line.fills.push(([0.0, 0.0, width, h], if r == 0 { 0xFFCFCFE0 } else if r % 2 == 1 { 0xFFF0F0E8 } else { 0xFFE4E4DC }));
                line.fills.push(([0.0, h - 1.0, width, 1.0], 0xFFB0B0B0));
                for (c, cl) in cells_lines.into_iter().enumerate() {
                    for (k, sub) in cl.into_iter().enumerate() {
                        for (x, _, t, colour, link) in sub.texts {
                            line.texts.push((3.0 + col_w * c as f32 + x, 2.0 + LINE * k as f32, t, colour, link));
                        }
                    }
                }
                out.push(line);
            }
            gap(&mut out, 4.0);
            continue;
        }
        // Pictures.
        if l.starts_with("![") {
            flush(&mut para, &mut item, &mut out);
            if let Some(open) = l.find("](")
                && let Some(close) = l[open..].find(')')
                && let Some(p) = picture(game, page, &l[open + 2..open + close], width, &mut problems)
            {
                out.push(p);
            }
            i += 1;
            continue;
        }
        // Quotes.
        if let Some(q) = l.strip_prefix('>') {
            flush(&mut para, &mut item, &mut out);
            let words = inline(game, page, q.trim(), 0x505050, &mut problems);
            let start = out.len();
            flow(game, &words, 10.0, width, &mut out);
            for line in &mut out[start..] {
                line.fills.push(([2.0, -1.0, 2.0, LINE], 0xFF9090B0));
            }
            i += 1;
            continue;
        }
        // AE2's tags.
        if l.starts_with('<') && !l.starts_with("<ItemLink") && !l.starts_with("<br") && !l.starts_with("<ae2:ConfigValue") && !l.starts_with("<a ") {
            flush(&mut para, &mut item, &mut out);
            let name: String = l.trim_start_matches('<').chars().take_while(|c| c.is_alphanumeric()).collect();
            let (block, next) = tag_block(&lines, i, &name);
            let tags = tags_in(&block);
            match name.as_str() {
                "GameScene" => {
                    for t in &tags {
                        if t.starts_with("<ImportStructure")
                            && let Some(src) = attr(t, "src")
                        {
                            let path = resolve(page, &src.replace(".snbt", ".json"));
                            match scene(game, &path, width, &block) {
                                Some(line) => out.push(line),
                                None => problems.push(format!("scene {path} missing")),
                            }
                        }
                    }
                    gap(&mut out, 4.0);
                }
                "Row" | "ItemGrid" => {
                    let parts = pieces(game, page, &tags, width, &mut problems);
                    side_by_side(parts, width, &mut out);
                    gap(&mut out, 4.0);
                }
                "Column" => {
                    let inner: String = block.lines().skip(1).take(block.lines().count().saturating_sub(2)).collect::<Vec<_>>().join("\n");
                    let (sub, more) = layout(game, page, &inner, width);
                    out.extend(sub);
                    problems.extend(more);
                }
                "ItemImage" | "BlockImage" | "ItemIcon" | "RecipeFor" | "Recipe" => {
                    let parts = pieces(game, page, &tags, width, &mut problems);
                    side_by_side(parts, width, &mut out);
                    gap(&mut out, 4.0);
                }
                "div" => {
                    // AE2's notes: a tinted box with its text.
                    let inner: String = block.lines().filter(|l| !l.trim().starts_with("<div") && !l.trim().starts_with("</div")).collect::<Vec<_>>().join(" ");
                    let words = inline(game, page, &inner, 0x1E3A5A, &mut problems);
                    let start = out.len();
                    flow(game, &words, 6.0, width - 6.0, &mut out);
                    for line in &mut out[start..] {
                        line.fills.insert(0, ([0.0, -1.0, width, LINE], 0xFFCFE0F0));
                    }
                    gap(&mut out, 4.0);
                }
                "SubPages" => {
                    let children: Vec<String> = GUIDE.with_borrow(|g| g.parents.iter().filter(|(_, p)| *p == page).map(|(c, _)| c.clone()).collect());
                    page_list(game, children, width, &mut out);
                    gap(&mut out, 4.0);
                }
                "CategoryIndex" => {
                    let cat = attr(&tags[0], "category").unwrap_or_default();
                    let pages: Vec<String> = GUIDE.with_borrow(|g| g.categories.iter().filter(|(_, c)| c.contains(&cat)).map(|(p, _)| p.clone()).collect());
                    if pages.is_empty() {
                        problems.push(format!("CategoryIndex {cat} lists no page"));
                    }
                    page_list(game, pages, width, &mut out);
                    gap(&mut out, 4.0);
                }
                "a" | "br" => {}
                other => problems.push(format!("tag <{other}> left out")),
            }
            i = next.max(i + 1);
            continue;
        }
        // Lists: `- `, `* `, `1. ` (nested by their indent), with their following lines.
        let indent = raw.len() - raw.trim_start().len();
        let marker = if let Some(r) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
            Some(("•".to_owned(), r))
        } else {
            let digits: String = l.chars().take_while(|c| c.is_ascii_digit()).collect();
            (!digits.is_empty()).then(|| l[digits.len()..].strip_prefix(". ").map(|r| (format!("{digits}."), r))).flatten()
        };
        if let Some((m, rest)) = marker {
            flush(&mut para, &mut item, &mut out);
            item = Some((indent / 2, m, inline(game, page, rest, TEXT, &mut problems)));
            i += 1;
            continue;
        }
        if let Some((_, _, words)) = item.as_mut()
            && indent > 0
        {
            words.extend(inline(game, page, l, TEXT, &mut problems));
            i += 1;
            continue;
        }
        if item.is_some() {
            flush(&mut para, &mut item, &mut out);
        }
        para.extend(inline(game, page, l, TEXT, &mut problems));
        i += 1;
    }
    flush(&mut para, &mut item, &mut out);
    (out, problems)
}

/// A structure as an isometric scene (as AE2's guide shows its assemblies): each block's item drawn where it
/// stands, scaled to fit the page's width, farthest first.
fn scene(game: &Game<'_>, path: &str, width: f32, body: &str) -> Option<Line> {
    let bytes = game.native_data(&format!("data/{}/ae2guide/{path}", current_ns()))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let mut blocks: Vec<(i64, i64, i64, String)> = v
        .get("blocks")?
        .as_array()?
        .iter()
        .filter_map(|b| {
            let b = b.as_array()?;
            let mut id = b.get(3)?.as_str()?.to_owned();
            if let Some(props) = b.get(4).and_then(|p| p.as_object()) {
                let list: Vec<String> = props.iter().filter_map(|(k, v)| Some(format!("{k}={}", v.as_str()?))).collect();
                id = format!("{id}|{}", list.join(";"));
            }
            Some((b.first()?.as_i64()?, b.get(1)?.as_i64()?, b.get(2)?.as_i64()?, id))
        })
        .collect();
    if blocks.is_empty() {
        return None;
    }
    blocks.sort_by_key(|b| (b.0 + b.2, b.1));
    let at = |b: &(i64, i64, i64, String)| ((b.0 - b.2) as f32, (b.0 + b.2) as f32 * 0.5 - b.1 as f32);
    let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for b in &blocks {
        let (x, y) = at(b);
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    let steps_w = (x1 - x0) + 2.0;
    let steps_h = (y1 - y0) + 2.0;
    let icon = (2.0 * width / steps_w).min(2.0 * 150.0 / steps_h).clamp(8.0, 32.0);
    let half = icon / 2.0;
    let left = (width - steps_w * half) / 2.0;
    let scene = blocks
        .iter()
        .map(|b| {
            let (x, y) = at(b);
            (left + (x - x0) * half, (y - y0) * half, icon, b.3.clone())
        })
        .collect();
    let h = steps_h * half + 6.0;
    // GameScene's camera: about the structure's middle, AE2's isometric start (30 degrees down, 225 round).
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for b in &blocks {
        for (i, v) in [b.0, b.1, b.2].into_iter().enumerate() {
            lo[i] = lo[i].min(v as f32);
            hi[i] = hi[i].max(v as f32 + 1.0);
        }
    }
    let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
    let mut scene3d = Scene3d {
        blocks: blocks.iter().map(|b| ([b.0 as f32 + 0.5 - mid[0], b.1 as f32 + 0.5 - mid[1], b.2 as f32 + 0.5 - mid[2]], b.3.clone())).collect(),
        annotations: annotations_in(body, mid),
        unit: 1.0,
        center: [width / 2.0, h / 2.0],
        yaw: 225f32.to_radians(),
        pitch: 30f32.to_radians(),
    };
    // The block size that fits the structure's corners, seen from the start view, into the band.
    let r = scene_rotation(&scene3d);
    let (mut sx0, mut sx1, mut sy0, mut sy1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for x in [lo[0], hi[0]] {
        for y in [lo[1], hi[1]] {
            for z in [lo[2], hi[2]] {
                let q = r.transform_point3(glam::Vec3::new(x - mid[0], y - mid[1], z - mid[2]));
                sx0 = sx0.min(q.x);
                sx1 = sx1.max(q.x);
                sy0 = sy0.min(q.y);
                sy1 = sy1.max(q.y);
            }
        }
    }
    scene3d.unit = ((width * 0.9) / (sx1 - sx0).max(0.5)).min((h - 8.0) / (sy1 - sy0).max(0.5)).clamp(6.0, 48.0);
    Some(Line { scene, scene3d: Some(scene3d), fills: vec![([0.0, 0.0, width, h], 0xFFD8D8D0)], height: h + 2.0, ..Default::default() })
}

/// The marks in a `<GameScene>`'s text.
fn annotations_in(body: &str, mid: [f32; 3]) -> Vec<Annotation> {
    let point = |tag: &str, name: &str| -> Option<[f32; 3]> {
        let text = attr(tag, name)?;
        let v: Vec<f32> = text.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        Some([*v.first()? - mid[0], *v.get(1)? - mid[1], *v.get(2)? - mid[2]])
    };
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find('<') {
        rest = &rest[at..];
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..=end];
        let name: String = tag[1..].chars().take_while(|c| c.is_alphanumeric()).collect();
        rest = &rest[end + 1..];
        if !name.ends_with("Annotation") {
            continue;
        }
        // Its text: up to the closing tag, unless the tag closes itself.
        let mut tip = Vec::new();
        if !tag.ends_with("/>") {
            let close = format!("</{name}>");
            if let Some(stop) = rest.find(&close) {
                tip = rest[..stop].lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| l.replace("**", "")).collect();
                rest = &rest[stop + close.len()..];
            }
        }
        let shape = match name.as_str() {
            "LineAnnotation" => point(tag, "from").zip(point(tag, "to")).map(|(a, b)| AnnotationShape::Line(a, b)),
            "BoxAnnotation" => point(tag, "min").zip(point(tag, "max")).map(|(a, b)| AnnotationShape::Box(a, b)),
            "DiamondAnnotation" => point(tag, "pos").map(AnnotationShape::Diamond),
            _ => None,
        };
        let Some(shape) = shape else { continue };
        let color = attr(tag, "color").and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok()).unwrap_or(0xFF_FFFF);
        let thickness = attr(tag, "thickness").and_then(|t| t.parse().ok()).unwrap_or(0.03125);
        out.push(Annotation { shape, color: 0xFF00_0000 | color, thickness, tip });
    }
    out
}

/// A scene's marks over its blocks; the text of the one under the pointer. `band` is the scene's visible rows.
fn draw_annotations(game: &Game<'_>, batch: &mut HudBatch, s: &Scene3d, origin: [f32; 2], band: [f32; 2], mouse: [f32; 2]) -> Option<Vec<String>> {
    let r = scene_rotation(s);
    let place = |p: [f32; 3]| -> [f32; 2] {
        let q = r.transform_point3(glam::Vec3::from(p));
        [origin[0] + s.center[0] + q.x * s.unit, origin[1] + s.center[1] - q.y * s.unit]
    };
    let mut tip = None;
    for a in &s.annotations {
        let width = (a.thickness * s.unit).max(1.0);
        let segment = |batch: &mut HudBatch, from: [f32; 2], to: [f32; 2]| -> bool {
            let length = (to[0] - from[0]).hypot(to[1] - from[1]);
            let steps = length.ceil().max(1.0) as usize;
            let mut near = false;
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let (x, y) = (from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t);
                if y >= band[0] && y < band[1] {
                    game.native_fill(batch, [x - width / 2.0, y - width / 2.0, width, width], a.color);
                }
                near |= (mouse[0] - x).abs() < 3.0 && (mouse[1] - y).abs() < 3.0;
            }
            near
        };
        let hit = match a.shape {
            AnnotationShape::Line(from, to) => segment(batch, place(from), place(to)),
            AnnotationShape::Box(lo, hi) => {
                let corner = |i: usize| place([if i & 1 == 0 { lo[0] } else { hi[0] }, if i & 2 == 0 { lo[1] } else { hi[1] }, if i & 4 == 0 { lo[2] } else { hi[2] }]);
                let mut near = false;
                for i in 0..8 {
                    for bit in [1, 2, 4] {
                        if i & bit == 0 {
                            near |= segment(batch, corner(i), corner(i | bit));
                        }
                    }
                }
                let spots: Vec<[f32; 2]> = (0..8).map(corner).collect();
                let (x0, x1) = (spots.iter().map(|c| c[0]).fold(f32::MAX, f32::min), spots.iter().map(|c| c[0]).fold(f32::MIN, f32::max));
                let (y0, y1) = (spots.iter().map(|c| c[1]).fold(f32::MAX, f32::min), spots.iter().map(|c| c[1]).fold(f32::MIN, f32::max));
                near || (mouse[0] >= x0 && mouse[0] < x1 && mouse[1] >= y0 && mouse[1] < y1)
            }
            AnnotationShape::Diamond(at) => {
                let [x, y] = place(at);
                for row in -4i32..=4 {
                    let half = (4 - row.abs()) as f32;
                    let line = y + row as f32;
                    if line >= band[0] && line < band[1] {
                        game.native_fill(batch, [x - half - 0.5, line, half * 2.0 + 1.0, 1.0], a.color);
                    }
                }
                (mouse[0] - x).abs() + (mouse[1] - y).abs() <= 5.0
            }
        };
        if hit && !a.tip.is_empty() && mouse[1] >= band[0] && mouse[1] < band[1] {
            tip = Some(a.tip.clone());
        }
    }
    tip
}

/// The view's rotation (pitch about x after yaw about y, as an item's display transform turns).
fn scene_rotation(s: &Scene3d) -> glam::Mat4 {
    glam::Mat4::from_rotation_x(s.pitch) * glam::Mat4::from_rotation_y(s.yaw)
}

/// Each block's place on the page (from the line's top left) and depth, with its item, far to near.
fn scene_projection(s: &Scene3d) -> Vec<(f32, f32, f32, [f32; 3], String)> {
    let r = scene_rotation(s);
    let mut out: Vec<(f32, f32, f32, [f32; 3], String)> = s
        .blocks
        .iter()
        .map(|(p, id)| {
            let q = r.transform_point3(glam::Vec3::from(*p));
            (s.center[0] + q.x * s.unit, s.center[1] - q.y * s.unit, q.z, *p, id.clone())
        })
        .collect();
    out.sort_by(|a, b| a.2.total_cmp(&b.2));
    out
}

/// A block's item model drawn in the scene: its place turned with the view and scaled to the scene's block size
/// (the model's own display transform replaced by the scene's).
/// An id with its block state (`id|key=value;...`): the block and its properties.
fn split_state(id: &str) -> (&str, Vec<(&str, &str)>) {
    match id.split_once('|') {
        Some((b, props)) => (b, props.split(';').filter_map(|kv| kv.split_once('=')).collect()),
        None => (id, Vec::new()),
    }
}

/// The item an id stands for (its block state dropped).
fn item_of(id: &str) -> &str {
    id.split_once('|').map_or(id, |(b, _)| b)
}

/// Block `block` in the state its `props` set (those it has), its model under `m` (block units to GUI pixels, +z
/// toward the viewer) as 3D quads. False when it has no block state.
fn draw_block_state(game: &Game<'_>, batch: &mut HudBatch, block: &str, props: &[(&str, &str)], m: glam::Mat4) -> bool {
    let registry = &game.pack.assets.registry;
    let Some(mut state) = registry.default_state(block).and_then(|id| registry.state(id)).cloned() else { return false };
    for (k, v) in props {
        if let Some(slot) = state.properties.get_mut(*k) {
            *slot = (*v).to_owned();
        }
    }
    let Some(id) = registry.id(&state) else { return false };
    let mut verts = Vec::new();
    game.block_model_vertices(id, 42, m, 0xF0F0, false, &mut verts);
    // Faces whose texture isn't on the GUI atlas (a content mod's block textures) are left out there: then the
    // block is drawn as its item is, which finds every texture, rather than with holes or not at all.
    let faces: usize = game.pack.render.model_catalog.collect_seeded(id, 42).map(|ms| ms.iter().map(|(_, b)| b.faces.len()).sum()).unwrap_or(0);
    if verts.is_empty() || verts.len() / 4 < faces {
        return false;
    }
    for q in verts.chunks_exact(4) {
        // The world's vertex colour is ABGR, the HUD's ARGB: red and blue trade places.
        let c = q[0].color;
        let c = (c & 0xFF00_FF00) | ((c >> 16) & 0xFF) | ((c & 0xFF) << 16);
        batch.quad_3d([q[0].pos, q[1].pos, q[2].pos, q[3].pos], [q[0].uv, q[1].uv, q[2].uv, q[3].uv], c);
    }
    true
}

/// A BlockImage: the block seen as its item would be (30 down, 225 round), `size` pixels.
fn draw_block_image(game: &Game<'_>, batch: &mut HudBatch, id: &str, at: [f32; 2], size: f32) {
    let (block, props) = split_state(id);
    let start = batch.vertices.len();
    let k = size * 0.625;
    let m = glam::Mat4::from_translation(glam::Vec3::new(at[0] + size / 2.0, at[1] + size / 2.0, 0.0))
        * glam::Mat4::from_scale(glam::Vec3::new(k, -k, k))
        * glam::Mat4::from_rotation_x(30f32.to_radians())
        * glam::Mat4::from_rotation_y(225f32.to_radians())
        * glam::Mat4::from_translation(glam::Vec3::splat(-0.5));
    if draw_block_state(game, batch, block, &props, m) {
        batch.depth_since(start, [at[0] - size, at[1] - size, at[0] + 2.0 * size, at[1] + 2.0 * size]);
    } else {
        game.native_item(batch, block, at, size);
    }
}

/// Draws one block of a 3D scene, turned as the scene is.
/// None when drawn; else where its item icon goes (drawn after the scene's depth pass, which would hide it).
fn draw_scene_block(game: &Game<'_>, batch: &mut HudBatch, s: &Scene3d, origin: [f32; 2], rel: [f32; 3], id: &str) -> Option<([f32; 2], f32, String)> {
    let (block, props) = split_state(id);
    let [cx, cy] = [origin[0] + s.center[0], origin[1] + s.center[1]];
    let m = glam::Mat4::from_translation(glam::Vec3::new(cx, cy, 0.0))
        * glam::Mat4::from_scale(glam::Vec3::new(s.unit, -s.unit, s.unit))
        * scene_rotation(s)
        * glam::Mat4::from_translation(glam::Vec3::from(rel) - glam::Vec3::splat(0.5));
    if draw_block_state(game, batch, block, &props, m) {
        return None;
    }
    // A block whose textures the GUI atlas lacks (a content mod's): its item icon, the block seen from the scene's
    // start view, centred where the block is. It keeps its place as the scene turns.
    let q = scene_rotation(s).transform_point3(glam::Vec3::from(rel));
    let size = s.unit * 1.5;
    let at = [cx + q.x * s.unit - size / 2.0, cy - q.y * s.unit - size / 2.0];
    Some((at, size, block.to_owned()))
}

/// `native_screen_mouse_dragged_pre`: dragging a scene turns it (left-right round, up-down over).
pub(in crate::game) fn drag(delta: [f32; 2], canceled: &mut bool) {
    GUIDE.with_borrow_mut(|g| {
        let Some((line, moved, _)) = g.drag.as_mut() else { return };
        *canceled = true;
        if delta[0].abs() + delta[1].abs() > 0.0 {
            *moved = true;
        }
        let line = *line;
        if let Some(s) = g.lines.get_mut(line).and_then(|l| l.scene3d.as_mut()) {
            s.yaw += delta[0] * 0.02;
            s.pitch = (s.pitch + delta[1] * 0.02).clamp(-1.5, 1.5);
        }
    });
}

/// `native_screen_mouse_button_released_pre`: the end of a drag; a press that didn't move opens the block's page.
pub(in crate::game) fn release(game: &Game<'_>, canceled: &mut bool) {
    let Some((_, moved, id)) = GUIDE.with_borrow_mut(|g| g.drag.take()) else { return };
    *canceled = true;
    if !moved && !id.is_empty() {
        let page = GUIDE.with_borrow(|g| g.open.clone());
        let target = GUIDE.with_borrow(|g| g.items.get(item_of(&id)).cloned());
        if let Some(t) = target.filter(|t| Some(t) != page.as_ref()) {
            open_page(game, &t, true);
        }
    }
}

/// Where the panel is: centred in the window.
fn panel(game: &Game<'_>) -> [f32; 4] {
    let (w, h) = game.gui_size();
    let [pw, ph] = GUIDE.with_borrow(|g| g.size);
    [((w - pw) / 2.0).max(0.0).floor(), ((h - ph) / 2.0).max(0.0).floor(), pw, ph]
}

/// `native_screen_render_post`: the guide over the screen, the hovered item's tooltip over it.
pub(in crate::game) fn draw(game: &Game<'_>, batch: &mut HudBatch, mouse: [f32; 2]) {
    let Some(page) = GUIDE.with_borrow(|g| g.open.clone()) else { return };
    let [x, y, pw, ph] = panel(game);
    let (sw, sh) = game.gui_size();
    game.native_fill(batch, [0.0, 0.0, sw, sh], 0x90000000);
    game.native_fill(batch, [x - 1.0, y - 1.0, pw + 2.0, ph + 2.0], 0xFF000000);
    game.native_fill(batch, [x, y, pw, ph], 0xFFECECE4);
    game.native_fill(batch, [x, y, pw, 16.0], 0xFF505070);
    let title = GUIDE.with_borrow(|g| g.titles.get(&page).cloned()).unwrap_or_else(|| page.clone());
    game.native_text(batch, &format!("{} — {title}", current_title()), [x + 6.0, y + 4.0], 0xFFFFFF, false);
    let can_back = GUIDE.with_borrow(|g| !g.history.is_empty());
    let ((back, back_x), (contents, contents_x)) = header_buttons(game, x, pw);
    game.native_text(batch, &back, [back_x, y + 4.0], if can_back { 0xFFFFA0 } else { 0x9090A0 }, false);
    // Contents: the guide's first page, from anywhere in it.
    let home = root_page().is_some_and(|r| r != page);
    game.native_text(batch, &contents, [contents_x, y + 4.0], if home { 0xFFFFA0 } else { 0x9090A0 }, false);
    game.native_text(batch, "x", [x + pw - 12.0, y + 4.0], 0xFFFFFF, false);
    let (lines, scroll) = GUIDE.with_borrow(|g| (g.lines.clone(), g.scroll));
    let top = y + 20.0;
    let bottom = y + ph - 4.0;
    let left = x + PAD;
    let visible = |a: f32, b: f32| a >= top - 1.0 && b <= bottom + 1.0;
    let mut tip: Option<String> = None;
    let mut annotation_tip: Option<Vec<String>> = None;
    let mut ly = top - scroll;
    for line in &lines {
        if ly + line.height >= top && ly <= bottom {
            for (r, c) in &line.fills {
                let (fy, fh) = (ly + r[1], r[3]);
                let (cy0, cy1) = (fy.max(top), (fy + fh).min(bottom));
                if cy1 > cy0 && *c != 0 {
                    game.native_fill(batch, [left + r[0], cy0, r[2], cy1 - cy0], *c);
                }
            }
            for (r, name) in &line.sprites {
                if visible(ly + r[1], ly + r[1] + r[3])
                    && let Some([w, h]) = game.native_sprite_size(name)
                {
                    game.native_sprite(batch, name, [0.0, 0.0, w, h], [left + r[0], ly + r[1], r[2], r[3]]);
                }
            }
            for (px, py, text, colour, _) in &line.texts {
                if visible(ly + py, ly + py + 8.0) {
                    let c = *colour;
                    let rgba = horizon_client::hud::rgba(((c >> 16) & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, (c & 0xFF) as u8, 255);
                    batch.text_styled(&game.gui.atlas, &horizon_client::text::Text::legacy(text), left + px, ly + py, rgba, false);
                }
            }
            for (px, py, id, size) in &line.icons {
                let (ix, iy) = (left + px, ly + py);
                if visible(iy, iy + size) {
                    if id.contains('|') {
                        draw_block_image(game, batch, id, [ix, iy], *size);
                    } else {
                        game.native_item(batch, id, [ix, iy], *size);
                    }
                    if mouse[0] >= ix && mouse[0] < ix + size && mouse[1] >= iy && mouse[1] < iy + size {
                        tip = Some(item_of(id).to_owned());
                    }
                }
            }
            // Part of a scene shows as it scrolls: its quads and marks are kept to the band.
            if ly + line.height > top && ly < bottom && let Some(s) = &line.scene3d {
                let projected = scene_projection(s);
                let start = batch.vertices.len();
                let icons: Vec<_> = projected.iter().filter_map(|(_, _, _, rel, id)| draw_scene_block(game, batch, s, [left, ly], *rel, id)).collect();
                // One model: the scene's quads depth-tested together, kept to its band.
                let band = [left, ly.max(top), left + width_of(line), (ly + line.height).min(bottom)];
                batch.depth_since(start, band);
                // Blocks drawn as icons, far to near, then kept to the band too.
                let icons_start = batch.vertices.len();
                for (at, size, block) in icons {
                    game.native_item(batch, &block, at, size);
                }
                batch.clip_since(icons_start, band[0], band[1], band[2], band[3]);
                if let Some(text) = draw_annotations(game, batch, s, [left, ly], [ly.max(top), (ly + line.height).min(bottom)], mouse) {
                    annotation_tip = Some(text);
                }
                let hits: Vec<(f32, f32, f32, String)> = projected.into_iter().map(|(x, y, _, _, id)| (x - s.unit / 2.0, y - s.unit / 2.0, s.unit, id)).collect();
                let index = lines.iter().position(|l| std::ptr::eq(l, line));
                GUIDE.with_borrow_mut(|g| {
                    if let Some(l) = index.and_then(|i| g.lines.get_mut(i)) {
                        l.scene = hits;
                    }
                });
            } else if visible(ly, ly + line.height - 2.0) {
                for (px, py, size, id) in &line.scene {
                    game.native_item(batch, id, [left + px, ly + py], *size);
                }
                for (px, py, size, id) in line.scene.iter().rev() {
                    let (bx, by) = (left + px, ly + py);
                    if mouse[0] >= bx + size * 0.15 && mouse[0] < bx + size * 0.85 && mouse[1] >= by + size * 0.1 && mouse[1] < by + size * 0.9 {
                        tip = Some(item_of(id).to_owned());
                        break;
                    }
                }
            }
        }
        ly += line.height;
    }
    // The scroll bar.
    let total: f32 = lines.iter().map(|l| l.height).sum();
    let view = bottom - top;
    if total > view {
        let bar = (view * view / total).max(12.0);
        let at = top + (view - bar) * scroll / (total - view);
        game.native_fill(batch, [x + pw - 6.0, top, 4.0, view], 0xFFC8C8C0);
        game.native_fill(batch, [x + pw - 6.0, at, 4.0, bar], 0xFF707090);
    }
    if let Some(text) = annotation_tip {
        let lines: Vec<(horizon_client::text::Text, u32)> = text.iter().map(|l| (horizon_client::text::Text::legacy(l), u32::MAX)).collect();
        game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
    } else if let Some(id) = tip {
        let lines = game.stack_tooltip(&horizon_client::items::ItemStack::new(&id, 1));
        game.draw_tooltip_lines(batch, &lines, mouse[0], mouse[1]);
    }
}

/// `native_screen_mouse_button_pressed_pre`: clicks on the guide (links, pictures, scene blocks, Back, close);
/// none reach the screen under it while it is open.
pub(in crate::game) fn click(game: &Game<'_>, mouse: [f32; 2], canceled: &mut bool) {
    let Some(page) = GUIDE.with_borrow(|g| g.open.clone()) else { return };
    *canceled = true;
    let [x, y, pw, _] = panel(game);
    let [mx, my] = mouse;
    if my >= y && my < y + 16.0 {
        let ((_, back_x), (_, contents_x)) = header_buttons(game, x, pw);
        if mx >= x + pw - 14.0 {
            close();
        } else if mx >= back_x - 4.0 {
            back(game);
        } else if mx >= contents_x - 4.0
            && let Some(root) = root_page().filter(|r| *r != page)
        {
            open_page(game, &root, true);
        }
        return;
    }
    let open_item = |id: &str| {
        let target = GUIDE.with_borrow(|g| g.items.get(id).cloned());
        if let Some(t) = target.filter(|t| *t != page) {
            open_page(game, &t, true);
        }
    };
    let (lines, scroll) = GUIDE.with_borrow(|g| (g.lines.clone(), g.scroll));
    let left = x + PAD;
    let mut ly = y + 20.0 - scroll;
    for line in &lines {
        if my >= ly && my < ly + line.height {
            if line.scene3d.is_some() {
                let mut hit = String::new();
                for (px, py, size, id) in line.scene.iter().rev() {
                    let (bx, by) = (left + px, ly + py);
                    if mx >= bx + size * 0.15 && mx < bx + size * 0.85 && my >= by + size * 0.1 && my < by + size * 0.9 {
                        hit = item_of(id).to_owned();
                        break;
                    }
                }
                let index = lines.iter().position(|l| std::ptr::eq(l, line)).unwrap_or(0);
                GUIDE.with_borrow_mut(|g| g.drag = Some((index, false, hit)));
                return;
            }
            for (px, py, size, id) in line.scene.iter().rev() {
                let (bx, by) = (left + px, ly + py);
                if mx >= bx + size * 0.15 && mx < bx + size * 0.85 && my >= by + size * 0.1 && my < by + size * 0.9 {
                    open_item(item_of(id));
                    return;
                }
            }
            for (px, py, id, size) in &line.icons {
                let (bx, by) = (left + px, ly + py);
                if mx >= bx && mx < bx + size && my >= by && my < by + size {
                    open_item(item_of(id));
                    return;
                }
            }
            for (px, py, text, _, link) in &line.texts {
                let (lx, ty) = (left + px, ly + py);
                let plain = text.replace("§l", "").replace("§o", "");
                if mx >= lx && mx < lx + game.native_text_width(&plain) + 2.0 && my >= ty - 1.0 && my < ty + 9.0 {
                    match link {
                        Some(Link::Page(p)) => {
                            if GUIDE.with_borrow(|g| g.titles.contains_key(p)) {
                                open_page(game, p, true);
                            }
                        }
                        Some(Link::Item(id)) => open_item(id),
                        None => {}
                    }
                    return;
                }
            }
        }
        ly += line.height;
    }
}

/// `native_screen_mouse_scrolled_pre`: the guide scrolls.
pub(in crate::game) fn scroll(by: f64, canceled: &mut bool) {
    GUIDE.with_borrow_mut(|g| {
        if g.open.is_none() {
            return;
        }
        *canceled = true;
        let total: f32 = g.lines.iter().map(|l| l.height).sum();
        let most = (total - (g.size[1] - 24.0)).max(0.0);
        g.scroll = (g.scroll - by as f32 * 22.0).clamp(0.0, most);
    });
}

/// `native_screen_key_pressed_pre`: the guide key over a hovered item opens its page; with the guide open,
/// Escape closes it, Backspace goes back, and no key reaches the screen.
pub(in crate::game) fn key(game: &Game<'_>, menu: Option<&str>, key: &str, repeat: bool, canceled: &mut bool) {
    if is_open() {
        *canceled = true;
        if key.ends_with("escape") {
            close();
        } else if key.ends_with("backspace") {
            back(game);
        } else if key.ends_with("pageup") || key.ends_with("pagedown") {
            let mut c = false;
            scroll(if key.ends_with("pageup") { 8.0 } else { -8.0 }, &mut c);
        }
        return;
    }
    let bound = hotkey(game);
    if (key == bound || bound.rsplit('.').next() == Some(key))
        && !repeat
        && hotkey_enabled()
        && !game.mod_browser_typing()
        && !KEYBOARD_TAKEN.with_borrow(|taken| taken.iter().any(|f| f(game, menu)))
        && GUIDE.with_borrow(|g| g.screen_since.is_some_and(|at| at.elapsed().as_millis() >= 500))
    {
        // JEI hears the key too, on its panel and over a terminal's list: `open_from_key` is its way in.
        ARMED.set(Some(std::time::Instant::now()));
        load_all(game);
        // Only a press of its own, on what is really under the pointer: not W held from walking up to the block
        // (its repeats, or the same press a moment after the screen came up), and not an item the pointer is
        // only passing over.
        let settled = GUIDE.with_borrow(|g| g.hovered_since.is_some_and(|at| at.elapsed().as_millis() >= 100));
        let item = GUIDE.with_borrow(|g| g.hovered.clone())
            .filter(|(i, at)| settled && at.elapsed().as_millis() < 150 && has_page(i))
            .map(|(i, _)| i);
        if let Some(item) = item
            && open_item(game, &item)
        {
            ARMED.set(None);
            *canceled = true;
        }
    }
}

/// `/ae2guide open <page>` (a page, as the tests need) and `/ae2guide audit` (every page laid out: what in it
/// couldn't be shown — tags, items, pictures, recipes, links — written to ae2-guide-audit.txt beside the game's
/// options). Client-side.
#[allow(dead_code)] // The guide's API: each mod calls the parts it needs.
pub(in crate::game) fn command(_game: &Game<'_>, args: &str) -> String {
    // A command runs on the server's thread: the guide (its pictures, the window's recipes) is the render
    // thread's, which picks the request up on its next frame.
    *ASKED.lock().unwrap_or_else(|e| e.into_inner()) = Some(args.trim().to_owned());
    format!("ae2guide {}: on the next frame", args.trim())
}

/// A `/ae2guide` request waiting for the render thread.
static ASKED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The frame's part (render thread): a waiting `/ae2guide` request runs here.
pub(in crate::game) fn frame(game: &Game<'_>) {
    let Some(args) = ASKED.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    let msg = run_command(game, &args);
    log::info!("{msg}");
}

/// The guide's debug commands (`open <page>`, `audit`).
fn run_command(game: &Game<'_>, args: &str) -> String {
    load_all(game);
    // (The command is AE2's; another mod's pages open through their items.)
    if let Some(ns) = ROOTS.with_borrow(|r| r.iter().find(|(n, _, _)| *n == "ae2").or(r.first()).map(|(n, _, _)| *n)) {
        use_mod(ns);
    }
    load_index(game);
    let mut words = args.split_whitespace();
    match words.next() {
        Some("open") => {
            let page = words.next().unwrap_or("index.md").to_owned();
            open_page(game, &page, false);
            format!("guide: {page}")
        }
        Some("audit") => {
            let mut pages: Vec<String> = GUIDE.with_borrow(|g| g.titles.keys().cloned().collect());
            pages.sort();
            let width = 430.0 - 2.0 * PAD - 8.0;
            let mut report = String::new();
            let mut total = 0;
            for p in &pages {
                let Some(bytes) = page_data(game, &p) else {
                    report.push_str(&format!("{p}: unreadable\n"));
                    continue;
                };
                let text = only_present(game, &String::from_utf8_lossy(&bytes));
                let (lines, mut problems) = layout(game, p, &text, width);
                for l in &lines {
                    for (_, _, t, _, link) in &l.texts {
                        let plain = t.replace("§l", "").replace("§o", "");
                        if plain.contains("**") || plain.contains("](") || plain.starts_with('<') || plain.contains("/>") {
                            problems.push(format!("raw markup shown: {plain}"));
                        }
                        if let Some(Link::Page(target)) = link
                            && !GUIDE.with_borrow(|g| g.titles.contains_key(target))
                        {
                            problems.push(format!("link to a missing page {target}"));
                        }
                    }
                    for (_, _, id, _) in &l.icons {
                        if game.res.items().get(id).is_none() {
                            problems.push(format!("icon of an unknown item {id}"));
                        }
                    }
                }
                problems.sort();
                problems.dedup();
                total += problems.len();
                if !problems.is_empty() {
                    report.push_str(&format!("{p}:\n"));
                    for pr in problems {
                        report.push_str(&format!("  {pr}\n"));
                    }
                }
            }
            let recipes = RECIPES.with_borrow(|r| r.as_ref().map_or(0, |c| c.recipes.len()));
            // What the game holds: its recipes, the guide's sprites in the GUI atlas.
            let mut sprites: Vec<String> = game.gui.atlas.sprites.keys().filter(|k| k.starts_with("ae2:gui/")).cloned().collect();
            sprites.sort();
            report.insert_str(0, &format!("recipes known: {recipes}\nae2 gui sprites ({}): {}\n", sprites.len(), sprites.join(" ")));
            let path = std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join("horizon").join("ae2-guide-audit.txt"));
            if let Some(path) = &path {
                let _ = std::fs::write(path, format!("{} pages, {total} problems\n{report}", pages.len()));
            }
            format!("guide audit: {} pages, {total} problems ({recipes} recipes known)", pages.len())
        }
        _ => "ae2guide open <page> | audit".to_owned(),
    }
}

/// An item's description: the first paragraph of its guide page, as plain text (AE2's JEI information pages).
#[allow(dead_code)] // The guide's API: each mod calls the parts it needs.
pub(in crate::game) fn summary(game: &Game<'_>, item: &str) -> Option<String> {
    load_all(game);
    let (ns, _) = GUIDE.with_borrow(|g| g.all_items.get(item).cloned())?;
    use_mod(&ns);
    load_index(game);
    thread_local! {
        static CACHE: RefCell<HashMap<String, Option<String>>> = RefCell::new(HashMap::new());
    }
    if let Some(s) = CACHE.with_borrow(|c| c.get(item).cloned()) {
        return s;
    }
    load_index(game);
    let found = (|| {
        let page = GUIDE.with_borrow(|g| g.items.get(item).cloned())?;
        let bytes = page_data(game, &page)?;
        let text = only_present(game, &String::from_utf8_lossy(&bytes));
        let body = match text.strip_prefix("---").and_then(|r| r.find("\n---").map(|e| r[e + 4..].to_owned())) {
            Some(b) => b,
            None => text,
        };
        let para = body.split("\n\n").map(str::trim).find(|p| !p.is_empty() && !p.starts_with('#') && !p.starts_with('<') && !p.starts_with('!'))?;
        // Links as their words, item links as the item's name, other tags out.
        let mut out = String::new();
        let mut rest = para;
        while !rest.is_empty() {
            if let Some(stripped) = rest.strip_prefix("<ItemLink") {
                let end = stripped.find("/>").map_or(stripped.len(), |e| e + 2);
                if let Some(id) = attr(&stripped[..end], "id") {
                    out.push_str(&item_words(&id));
                }
                rest = &stripped[end..];
            } else if rest.starts_with('<') {
                rest = rest.find('>').map_or("", |e| &rest[e + 1..]);
            } else if rest.starts_with('[') && let Some(close) = rest.find("](") && let Some(end) = rest[close..].find(')') {
                out.push_str(&rest[1..close]);
                rest = &rest[close + end + 1..];
            } else {
                let c = rest.chars().next().unwrap();
                out.push(if c == '\n' { ' ' } else { c });
                rest = &rest[c.len_utf8()..];
            }
        }
        let out = out.replace(['*', '_'], "");
        Some(out.split_whitespace().collect::<Vec<_>>().join(" "))
    })();
    CACHE.with_borrow_mut(|c| c.insert(item.to_owned(), found.clone()));
    found
}

/// A band's width: the furthest its fills reach (a scene's background fills it).
fn width_of(line: &Line) -> f32 {
    line.fills.iter().map(|(r, _)| r[0] + r[2]).fold(0.0, f32::max)
}

// ---- Which mods have pages: each registers itself every frame (`register`), and only what is new does anything.

thread_local! {
    /// (mod id, the window's title, the name in the tooltip's hint).
    static ROOTS: RefCell<Vec<(&'static str, &'static str, &'static str)>> = const { RefCell::new(Vec::new()) };
    static KEYBOARD_TAKEN: RefCell<Vec<fn(&Game<'_>, Option<&str>) -> bool>> = const { RefCell::new(Vec::new()) };
    static HOTKEY: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    /// The bound key as the tooltip's hint says it ("" when it has none).
    static HOTKEY_NAME: RefCell<String> = RefCell::new("W".to_owned());
    /// When `key` last let a W through.
    static ARMED: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

/// W over an item JEI knows of (its panel, a terminal's list, the recipe window): only on the press `key` let through, so
/// held from walking, too soon after the screen came up, or typed into a box, it opens nothing. A press opens one page.
#[allow(dead_code)] // The guide's API: each mod calls the parts it needs.
pub(in crate::game) fn open_from_key(game: &Game<'_>, item: &str) -> bool {
    let fresh = ARMED.take().is_some_and(|at| at.elapsed().as_millis() < 300);
    // An item the pointer is only passing over waits for the next press.
    let passing = GUIDE.with_borrow(|g| {
        g.hovered.as_ref().is_some_and(|(i, _)| i == item) && g.hovered_since.is_none_or(|at| at.elapsed().as_millis() < 100)
    });
    fresh && !passing && open_item(game, item)
}

/// The key that opens a page: W, unless a mod with pages has its `guide` key set to another in Options > Mods.
pub(in crate::game) fn hotkey(game: &Game<'_>) -> String {
    const W: &str = "key.keyboard.w";
    let chosen = ROOTS.with_borrow(|r| r.iter().map(|(ns, _, _)| game.native_mod_key(ns, "guide", W)).find(|k| k != W));
    chosen.unwrap_or_else(|| W.to_owned())
}

/// A mod's pages, in data/<mod>/ae2guide (index.json and Markdown, as AE2's).
pub(in crate::game) fn register(ns: &'static str, title: &'static str, hint: &'static str) {
    ROOTS.with_borrow_mut(|r| {
        if !r.iter().any(|(n, _, _)| *n == ns) {
            r.push((ns, title, hint));
            GUIDE.with_borrow_mut(|g| g.all_items.clear());
        }
    });
}

/// "A text box of this mod has the keyboard": W is a letter there.
#[allow(dead_code)] // The guide's API: each mod calls the parts it needs.
pub(in crate::game) fn register_keyboard_taken(f: fn(&Game<'_>, Option<&str>) -> bool) {
    KEYBOARD_TAKEN.with_borrow_mut(|t| {
        if !t.iter().any(|g| *g as usize == f as usize) {
            t.push(f);
        }
    });
}

/// AEConfig.isGuideHotkeyEnabled, for a mod that has such an option.
#[allow(dead_code)] // The guide's API: each mod calls the parts it needs.
pub(in crate::game) fn set_hotkey_enabled(on: bool) {
    HOTKEY.set(on);
}

/// Whether the guide key works (a mod can turn it off).
pub(in crate::game) fn hotkey_enabled() -> bool {
    HOTKEY.get()
}

/// The mod whose pages are open.
fn current_ns() -> String {
    GUIDE.with_borrow(|g| g.ns.clone())
}

/// The open mod's guide title.
fn current_title() -> &'static str {
    let ns = current_ns();
    ROOTS.with_borrow(|r| r.iter().find(|(n, _, _)| *n == ns).map_or("Guide", |(_, title, _)| *title))
}

/// The open mod's navigation is read afresh when another's pages open.
fn use_mod(ns: &str) {
    GUIDE.with_borrow_mut(|g| {
        if g.ns != ns {
            g.ns = ns.to_owned();
            g.loaded = false;
            g.history.clear();
        }
    });
}

/// Every mod's items.
fn load_all(game: &Game<'_>) {
    if GUIDE.with_borrow(|g| !g.all_items.is_empty()) {
        return;
    }
    let was = current_ns();
    let roots: Vec<&'static str> = ROOTS.with_borrow(|r| r.iter().map(|(n, _, _)| *n).collect());
    for ns in &roots {
        use_mod(ns);
        load_index(game);
        GUIDE.with_borrow_mut(|g| {
            let items: Vec<(String, String)> = g.items.iter().map(|(i, p)| (i.clone(), p.clone())).collect();
            for (item, page) in items {
                g.all_items.insert(item, ((*ns).to_owned(), page));
            }
        });
    }
    if !was.is_empty() {
        use_mod(&was);
        load_index(game);
    }
}

/// `native_tooltip`: the hint under an item that has a page.
pub(in crate::game) fn tooltip(stack: &horizon_client::items::ItemStack, lines: &mut Vec<(horizon_client::text::Text, u32)>) {
    if !hotkey_enabled() {
        return;
    }
    let Some(ns) = GUIDE.with_borrow(|g| g.all_items.get(&*stack.item).map(|(ns, _)| ns.clone())) else { return };
    let Some(hint) = ROOTS.with_borrow(|r| r.iter().find(|(n, _, _)| *n == ns).map(|(_, _, hint)| *hint)) else { return };
    let key = HOTKEY_NAME.with_borrow(|k| k.clone());
    if !key.is_empty() {
        lines.push((horizon_client::text::Text::legacy(&format!("§8Press {key} to open the {hint}")), u32::MAX));
    }
}

/// The frame's part: what needs the game's `&mut`.
pub(in crate::game) fn frame_hook(game: &mut Game<'_>) {
    thread_local! {
        static SAID: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !SAID.replace(true) {
        log::info!("Horizon: the JEI kit in use is ae2's copy, version 14");
    }
    load_all(game);
    {
        let bound = hotkey(game);
        let name = bound.rsplit('.').next().unwrap_or("");
        let name = if name == "unknown" { String::new() } else { name.to_uppercase() };
        HOTKEY_NAME.with_borrow_mut(|k| *k = name);
    }
    if needs_recipes() {
        set_recipes(game.native_recipes());
    }
    frame(game);
}

/// An item's name, as the language files have it.
pub(in crate::game) fn item_words(id: &str) -> String {
    // The name in the player's language; these words only where the language files have none.
    let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
    for kind in ["block", "item"] {
        let key = format!("{kind}.{ns}.{path}");
        let name = horizon_client::lang::get_or(&key, "");
        if !name.is_empty() {
            return name;
        }
    }
    let path = id.split_once(':').map_or(id, |(_, p)| p);
    path.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

});
