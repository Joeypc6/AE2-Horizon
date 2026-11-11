"""Writes mixins/ae2/ui_lang.rs: AE2's English strings by key, so the menus (drawn from English text) follow the game's language.
Args: the ae2-content folder, the output file."""
import json
import sys

content, out = sys.argv[1:3]
lang = json.load(open(f'{content}/assets/ae2/lang/en_us.json', encoding='utf8'))
# A menu's words first: the same English can also name a block or an item.
order = sorted(lang, key=lambda k: (not k.startswith('gui.'), not k.startswith('block.'), k))
seen, rows = set(), []
# What this port wrote differently from AE2's text, with the key that says the same.
for english, key in (("Inventory", "container.inventory"), ("Network offline", "theoneprobe.ae2.device_offline"), ("Pattern Access", "gui.ae2.PatternAccessTerminal"), ("Statistics", "gui.stats")):
    seen.add(english)
    rows.append(f'    ({json.dumps(english)}, {json.dumps(key)}),')
for key in order:
    english = lang[key]
    if english in seen or len(english) < 2 or key.startswith('ae2.guide') or '\n' in english:
        continue
    seen.add(english)
    rows.append(f'    ({json.dumps(english, ensure_ascii=False)}, {json.dumps(key)}),')
TAIL = r'''
];

/// A string with `%s` in it, as what's around them, and its key.
struct Pattern {
    parts: Vec<&'static str>,
    key: &'static str,
}

#[derive(Default)]
struct Table {
    exact: HashMap<&'static str, &'static str>,
    patterns: Vec<Pattern>,
    /// What each string came to, in `language`.
    done: HashMap<String, String>,
    language: String,
}

thread_local! {
    static TABLE: RefCell<Table> = RefCell::new(Table::default());
}

fn placeholders(text: &'static str) -> Vec<&'static str> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find('%') {
        let tail = &rest[i + 1..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        let skip = if tail[digits..].starts_with('$') { digits + 1 } else { 0 };
        let tail = &tail[skip..];
        if tail.starts_with('s') || tail.starts_with('d') {
            parts.push(&rest[..i]);
            rest = &tail[1..];
        } else {
            break;
        }
    }
    parts.push(rest);
    parts
}

fn captures<'a>(parts: &[&str], text: &'a str) -> Option<Vec<&'a str>> {
    let mut rest = text.strip_prefix(parts[0])?;
    let mut out = Vec::new();
    for (i, part) in parts.iter().enumerate().skip(1) {
        let last = i == parts.len() - 1;
        let at = if last && part.is_empty() {
            rest.len()
        } else if last {
            rest.rfind(part)?
        } else {
            rest.find(part)?
        };
        if last && at + part.len() != rest.len() {
            return None;
        }
        out.push(&rest[..at]);
        rest = &rest[at + part.len()..];
    }
    Some(out)
}

fn fill(local: &str, args: &[&str]) -> String {
    let mut out = String::new();
    let mut next = 0;
    let mut rest = local;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        let numbered = tail[digits..].starts_with('$');
        let after = if numbered { &tail[digits + 1..] } else { tail };
        if after.starts_with('s') || after.starts_with('d') {
            let index = if numbered { tail[..digits].parse::<usize>().unwrap_or(1).saturating_sub(1) } else { next };
            next += 1;
            out.push_str(args.get(index).copied().unwrap_or(""));
            rest = &after[1..];
        } else {
            out.push('%');
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

/// `text` in the game's language, its leading colour codes kept.
pub fn ui(text: &str) -> String {
    let language = horizon_client::lang::get_or("gui.ae2.AutoCrafting", "");
    // English needs nothing.
    if language.is_empty() || language == "Auto-Crafting" {
        return text.to_owned();
    }
    TABLE.with_borrow_mut(|t| {
        if t.exact.is_empty() {
            for (english, key) in ENGLISH {
                if english.contains('%') {
                    let parts = placeholders(english);
                    if parts.len() > 1 && parts.iter().any(|p| p.len() > 1) {
                        t.patterns.push(Pattern { parts, key });
                    }
                }
                t.exact.entry(english).or_insert(key);
            }
            t.patterns.sort_by_key(|p| std::cmp::Reverse(p.parts.iter().map(|x| x.len()).sum::<usize>()));
        }
        if t.language != language {
            t.language = language;
            t.done.clear();
        }
        if let Some(done) = t.done.get(text) {
            return done.clone();
        }
        let mut body = text;
        while let Some(rest) = body.strip_prefix('\u{a7}') {
            let mut chars = rest.chars();
            if chars.next().is_none() {
                break;
            }
            body = chars.as_str();
        }
        let colours = &text[..text.len() - body.len()];
        let local = match t.exact.get(body) {
            Some(key) => Some(horizon_client::lang::get_or(key, body)),
            None => t.patterns.iter().find_map(|p| {
                let args = captures(&p.parts, body)?;
                Some(fill(&horizon_client::lang::get_or(p.key, ""), &args))
            }),
        };
        let done = local.filter(|s| !s.is_empty()).map_or_else(|| text.to_owned(), |l| format!("{colours}{l}"));
        if t.done.len() > 4096 {
            t.done.clear();
        }
        t.done.insert(text.to_owned(), done.clone());
        done
    })
}
'''
HEAD = """// The menus are drawn from AE2's English text, as this port wrote them; this finds each in the game's language.
// The table is written by tools/make_ui_lang.py from AE2's en_us.json. A string that isn't in it stays as it is.
use std::cell::RefCell;
use std::collections::HashMap;

const ENGLISH: &[(&str, &str)] = &[
"""
with open(out, 'w', encoding='utf8', newline='\n') as f:
    f.write(HEAD + '\n'.join(rows) + TAIL)
print(len(rows))
