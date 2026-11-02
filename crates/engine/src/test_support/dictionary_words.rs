//! Every word the vendored dictionary pair and the Singapore supplement hold, listed as Hunspell's
//! `unmunch` lists them: each `.dic` entry, and the entry with each affix its flags name applied,
//! a suffix, a prefix, or one of each where both classes allow the cross product. The vendored
//! affix file's flags are single characters and its rules carry no continuation classes, so one
//! level of affixes is the whole of it. An entry flagged to appear only inside a compound is not a
//! word on its own, and the compounds of numbers the affix file's compound rules build are not
//! listed. A test checks every word listed against spellbook's own lookup.

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use regex::Regex;

use super::dictionary_path;

/// The flag of an entry that appears only inside a compound, from the affix file's
/// `ONLYINCOMPOUND` line.
const ONLY_IN_COMPOUND: char = '_';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Prefix,
    Suffix,
}

/// One rule of an affix class.
struct Affix {
    strip: String,
    add: String,
    /// The rule's condition, anchored at the start of the word for a prefix and at its end for a
    /// suffix.
    condition: Regex,
}

/// The rules an affix flag names.
struct AffixClass {
    kind: Kind,
    cross_product: bool,
    affixes: Vec<Affix>,
}

impl Affix {
    /// `word` with this affix applied, or `None` where the rule's condition does not hold.
    fn applied(&self, kind: Kind, word: &str) -> Option<String> {
        if !self.condition.is_match(word) {
            return None;
        }
        match kind {
            Kind::Suffix => word
                .strip_suffix(self.strip.as_str())
                .map(|stem| format!("{stem}{}", self.add)),
            Kind::Prefix => word
                .strip_prefix(self.strip.as_str())
                .map(|stem| format!("{}{stem}", self.add)),
        }
    }
}

/// Every word the pair and the supplement hold, as written, computed once.
pub(crate) fn dictionary_words() -> &'static BTreeSet<String> {
    static WORDS: OnceLock<BTreeSet<String>> = OnceLock::new();
    WORDS.get_or_init(|| {
        let read = |name| std::fs::read_to_string(dictionary_path(name)).unwrap();
        let classes = affix_classes(&read("en_GB.aff"));
        let mut words = BTreeSet::new();
        // The first line is the entry count.
        for line in read("en_GB.dic").lines().skip(1) {
            // A tab opens the entry's morphological fields.
            let entry = line.split('\t').next().unwrap_or_default().trim();
            let (word, flags) = entry.split_once('/').unwrap_or((entry, ""));
            if word.is_empty() || flags.contains(ONLY_IN_COMPOUND) {
                continue;
            }
            expand(word, flags, &classes, &mut words);
        }
        for word in read("en_sg_supplement.txt").lines().map(str::trim) {
            if !word.is_empty() {
                words.insert(word.to_owned());
            }
        }
        words
    })
}

/// Each `PFX` and `SFX` class of `aff`, by its flag.
fn affix_classes(aff: &str) -> HashMap<char, AffixClass> {
    let mut classes: HashMap<char, AffixClass> = HashMap::new();
    for line in aff.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let kind = match fields.first() {
            Some(&"PFX") => Kind::Prefix,
            Some(&"SFX") => Kind::Suffix,
            _ => continue,
        };
        let flag = fields[1].chars().next().unwrap();
        match fields[2..] {
            // The class's header: whether it combines with the other kind, and its rule count.
            [cross_product @ ("Y" | "N"), count] if count.parse::<usize>().is_ok() => {
                classes.insert(
                    flag,
                    AffixClass {
                        kind,
                        cross_product: cross_product == "Y",
                        affixes: Vec::new(),
                    },
                );
            }
            [strip, add, condition, ..] => {
                let unless_zero = |field: &str| {
                    if field == "0" {
                        String::new()
                    } else {
                        field.to_owned()
                    }
                };
                classes.get_mut(&flag).unwrap().affixes.push(Affix {
                    strip: unless_zero(strip),
                    add: unless_zero(add),
                    condition: condition_regex(condition, kind),
                });
            }
            _ => panic!("an affix line of another shape: {line:?}"),
        }
    }
    classes
}

/// A Hunspell affix condition, a run of characters, `.` for any one, and bracketed classes, as a
/// regex anchored where the affix attaches.
fn condition_regex(condition: &str, kind: Kind) -> Regex {
    let mut source = String::new();
    for character in condition.chars() {
        match character {
            // The vendored file writes `^` only to negate a class it opens, and `.` only outside a
            // class, where it is any character: both read as a regex reads them.
            '[' | ']' | '^' | '.' => source.push(character),
            _ => source.push_str(&regex::escape(&character.to_string())),
        }
    }
    Regex::new(&match kind {
        Kind::Prefix => format!("^(?:{source})"),
        Kind::Suffix => format!("(?:{source})$"),
    })
    .unwrap()
}

/// Adds `word` and every form its `flags` give it to `words`.
fn expand(
    word: &str,
    flags: &str,
    classes: &HashMap<char, AffixClass>,
    words: &mut BTreeSet<String>,
) {
    words.insert(word.to_owned());
    let named: Vec<&AffixClass> = flags
        .chars()
        .filter_map(|flag| classes.get(&flag))
        .collect();
    let mut crossable_suffixed = Vec::new();
    for class in named.iter().filter(|class| class.kind == Kind::Suffix) {
        for suffixed in class
            .affixes
            .iter()
            .filter_map(|affix| affix.applied(Kind::Suffix, word))
        {
            if class.cross_product {
                crossable_suffixed.push(suffixed.clone());
            }
            words.insert(suffixed);
        }
    }
    for class in named.iter().filter(|class| class.kind == Kind::Prefix) {
        for affix in &class.affixes {
            words.extend(affix.applied(Kind::Prefix, word));
            if class.cross_product {
                for suffixed in &crossable_suffixed {
                    words.extend(affix.applied(Kind::Prefix, suffixed));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use spellbook::Dictionary;

    use super::*;

    #[test]
    fn spellbook_accepts_every_word_listed() {
        let read = |name| std::fs::read_to_string(dictionary_path(name)).unwrap();
        let mut dictionary = Dictionary::new(&read("en_GB.aff"), &read("en_GB.dic")).unwrap();
        for word in read("en_sg_supplement.txt").lines().map(str::trim) {
            if !word.is_empty() {
                dictionary.add(word).unwrap();
            }
        }
        let rejected: Vec<&String> = dictionary_words()
            .iter()
            .filter(|word| !dictionary.check(word))
            .collect();
        assert!(
            rejected.is_empty(),
            "{} rejected: {rejected:?}",
            rejected.len()
        );
    }

    #[test]
    fn an_affixed_form_is_listed_beside_its_entry() {
        let words = dictionary_words();
        // `pivot/dSMOo`, `intricate/PY`, `exemplify/SGRnD`, and `appear/GADSE`, whose `A` and `E`
        // are the prefixes `re` and `dis`, each crossing with the suffixes.
        for word in [
            "pivot",
            "pivotal",
            "pivotally",
            "intricately",
            "intricateness",
            "exemplification",
            "exemplifying",
            "reappeared",
            "disappearing",
        ] {
            assert!(words.contains(word), "{word}");
        }
        assert!(words.len() > 200_000, "{}", words.len());
    }
}
