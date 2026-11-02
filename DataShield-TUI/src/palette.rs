//! The `:` command palette — a fuzzy, ranked command finder (BRASS LEDGER
//! "P1"). It replaces the free-text parser: instead of typing a command's exact
//! name and arguments, you fuzzy-match over its name, its EN/IT aliases and its
//! localized description, and run the best hit. Secrets never enter the query.
//!
//! The matcher is a small internal fuzzy scorer (the candidate set is a handful
//! of commands, so a subsequence scorer is ample — no external fuzzy dep).

use crate::i18n::Lang;
use crate::registry::CommandDef;

/// Live palette state: the typed query and the highlighted row.
pub struct Palette {
    pub query: crate::textinput::TextInput,
    pub selected: usize,
}

impl Palette {
    pub fn new() -> Self {
        Palette { query: crate::textinput::TextInput::new(), selected: 0 }
    }
}

/// A fuzzy subsequence score: `Some(score)` when every char of `needle` occurs
/// in order in `haystack` (case-insensitive), higher = better. Word-start and
/// contiguous-run matches score more, so `re` ranks `rerollEnc` above a command
/// that merely contains `r…e`. `None` when `needle` isn't a subsequence.
pub fn fuzzy_score(haystack: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let h: Vec<char> = haystack.to_ascii_lowercase().chars().collect();
    let n: Vec<char> = needle.to_ascii_lowercase().chars().collect();
    let mut hi = 0usize;
    let mut score = 0i32;
    let mut streak = 0i32;
    let mut prev: Option<usize> = None;
    for &nc in &n {
        let mut found = None;
        while hi < h.len() {
            if h[hi] == nc {
                found = Some(hi);
                break;
            }
            hi += 1;
        }
        let pos = found?;
        if pos == 0 || !h[pos - 1].is_ascii_alphanumeric() {
            score += 8; // start of a word
        }
        if prev == Some(pos.wrapping_sub(1)) {
            streak += 1;
            score += 4 + streak; // contiguous run
        } else {
            streak = 0;
        }
        score += 1;
        prev = Some(pos);
        hi = pos + 1;
    }
    Some(score)
}

/// The best score of `query` against a command's name, aliases and localized
/// description — name/alias matches weigh most so typing a command's name wins.
pub fn command_score(cmd: &CommandDef, lang: Lang, query: &str) -> Option<i32> {
    let mut best: Option<i32> = None;
    let mut consider = |s: &str, weight: i32| {
        if let Some(sc) = fuzzy_score(s, query) {
            let sc = sc + weight;
            best = Some(best.map_or(sc, |b: i32| b.max(sc)));
        }
    };
    consider(&cmd.name, 40);
    for a in &cmd.aliases {
        consider(a, 30);
    }
    consider(cmd.description(lang), 0);
    best
}

/// The commands matching `query`, best first (stable within equal scores by
/// registry order). An empty query returns all commands in registry order.
pub fn ranked<'a>(cmds: &[&'a CommandDef], lang: Lang, query: &str) -> Vec<&'a CommandDef> {
    if query.trim().is_empty() {
        return cmds.to_vec();
    }
    let mut scored: Vec<(i32, usize, &CommandDef)> = cmds
        .iter()
        .enumerate()
        .filter_map(|(i, c)| command_score(c, lang, query).map(|s| (s, i, *c)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, c)| c).collect()
}
