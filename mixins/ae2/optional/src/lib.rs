//! A mod's integrations are compiled in only when what they work with is in the same build. A native mod is built on
//! the player's computer with the mods they have, so this is decided each time the launcher builds the game. It reads
//! one file, the game's own `src/game.rs` as staged, to see which bundles added what.
//!
//! The kit (the JEI window, Mob Drops, World Gen, the creative panel, the W guide) is copied into each mod that uses
//! it. Each copy adds a marker module `hz_kit_v<version>_<mod>`. The newest copy is the one compiled; the others come
//! to nothing. Two copies of one version: the mod id first in the alphabet.

use proc_macro::{Delimiter, Group, TokenStream, TokenTree};

fn game_rs() -> String {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    std::fs::read_to_string(format!("{dir}/src/game.rs")).unwrap_or_default()
}

fn in_build(name: &str) -> bool {
    game_rs().contains(&format!("mod {name} {{"))
}

/// The mod whose copy of the kit is compiled.
fn winner() -> Option<String> {
    let game = game_rs();
    let mut best: Option<(u32, String)> = None;
    for (at, _) in game.match_indices("mod hz_kit_v") {
        let rest = &game[at + "mod hz_kit_v".len()..];
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
        let Some((version, id)) = rest[..end].split_once('_') else { continue };
        let Ok(version) = version.parse::<u32>() else { continue };
        let better = match &best {
            None => true,
            Some((v, i)) => version > *v || (version == *v && id < i.as_str()),
        };
        if better {
            best = Some((version, id.to_owned()));
        }
    }
    best.map(|(_, id)| id)
}

/// `"name", { kept } [else { otherwise }]`.
fn parts(input: TokenStream) -> (String, TokenStream, TokenStream) {
    let mut tokens = input.into_iter();
    let name = match tokens.next() {
        Some(TokenTree::Literal(l)) => l.to_string().trim_matches('"').to_owned(),
        _ => panic!("the name first"),
    };
    assert!(matches!(tokens.next(), Some(TokenTree::Punct(p)) if p.as_char() == ','), "a comma after the name");
    let kept = match tokens.next() {
        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => g.stream(),
        _ => panic!("a block after the name"),
    };
    let otherwise = match (tokens.next(), tokens.next()) {
        (Some(TokenTree::Ident(i)), Some(TokenTree::Group(g))) if i.to_string() == "else" => g.stream(),
        (None, None) => TokenStream::new(),
        _ => panic!("only `else {{ }}` may follow the block"),
    };
    (name, kept, otherwise)
}

fn block(stream: TokenStream) -> TokenStream {
    TokenTree::Group(Group::new(Delimiter::Brace, stream)).into()
}

fn choose_mod(input: TokenStream) -> TokenStream {
    let (name, kept, otherwise) = parts(input);
    if in_build(&name) { kept } else { otherwise }
}

fn choose_kit(input: TokenStream, needs_jei: bool) -> TokenStream {
    let (id, kept, otherwise) = parts(input);
    let wins = winner().as_deref() == Some(id.as_str());
    if wins && (!needs_jei || in_build("jei")) { kept } else { otherwise }
}

/// Items, kept or left out: `if_mod_items!("jade", { ... });`.
#[proc_macro]
pub fn if_mod_items(input: TokenStream) -> TokenStream {
    choose_mod(input)
}

/// A block's value: `if_mod!("jade", { ... } else { ... })`.
#[proc_macro]
pub fn if_mod(input: TokenStream) -> TokenStream {
    block(choose_mod(input))
}

/// Items of a kit copy, kept when this mod's copy is the newest: `if_kit_items!("ae2", { ... });`.
#[proc_macro]
pub fn if_kit_items(input: TokenStream) -> TokenStream {
    choose_kit(input, false)
}

/// A block's value, kept when this mod's copy is the newest.
#[proc_macro]
pub fn if_kit(input: TokenStream) -> TokenStream {
    block(choose_kit(input, false))
}

/// `if_kit_items!`, and JEI is in the build.
#[proc_macro]
pub fn if_kit_jei_items(input: TokenStream) -> TokenStream {
    choose_kit(input, true)
}

/// `if_kit!`, and JEI is in the build.
#[proc_macro]
pub fn if_kit_jei(input: TokenStream) -> TokenStream {
    block(choose_kit(input, true))
}

/// `kit_use!(window as kit_window);`: the newest copy's module, under a name of this mod's.
#[proc_macro]
pub fn kit_use(input: TokenStream) -> TokenStream {
    let words: Vec<String> = input.into_iter().map(|t| t.to_string()).collect();
    let [part, as_word, name] = words.as_slice() else { panic!("kit_use!(part as name)") };
    assert_eq!(as_word, "as", "kit_use!(part as name)");
    let id = winner().expect("no copy of the kit in this build");
    let path = if part == "panel" { format!("crate::game::native_panel::hz_kit_panel_{id}") } else { format!("crate::game::hz_kit_{part}_{id}") };
    format!("#[allow(unused_imports)] pub(super) use {path} as {name};").parse().expect("kit_use")
}
