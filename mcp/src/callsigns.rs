//! A random creative spark for inventing call signs. Language models asked to "invent a call
//! sign" converge on the same few names, so each reader of the guide gets two random themes and
//! three random starting letters to work from. It never proposes names: the agent invents them.
//! Server-side randomness only; nothing here is journaled.

const THEMES: &[&str] = &[
    "a breakfast food",
    "a kind of cheese",
    "a piece of office equipment",
    "an obsolete job",
    "a dance move",
    "a houseplant",
    "a minor injury",
    "a kitchen utensil",
    "a board game",
    "a weather event",
    "a bureaucratic form",
    "a type of hat",
    "a garden pest",
    "a sandwich",
    "a musical instrument",
    "a knot",
    "a soup",
    "a laundry problem",
    "a tax term",
    "a medieval weapon",
    "a pasta shape",
    "a yoga pose",
    "a lost-and-found item",
    "a garden tool",
    "an insect",
    "a cocktail garnish",
    "a fabric",
    "a bad haircut",
    "a road sign",
    "a pastry",
    "a dog breed",
    "a sports injury",
    "a spice",
    "a font",
    "a cloud type",
    "an accounting mistake",
    "a moustache style",
    "a camping mishap",
    "a vegetable",
    "a punctuation mark",
    "a snack food",
    "a ballroom dance",
    "a sea shanty",
    "a fishing lure",
    "a carnival ride",
    "a cleaning product",
    "a deep-sea fish",
    "a geological layer",
    "a filing system",
    "an awkward silence",
    "a tropical fruit",
    "a shoe part",
    "a kite",
    "a picnic disaster",
];

/// Letters that start fewer stock names (no Q, X, Z).
const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPRSTUVWY";

fn pick(len: usize) -> usize {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("os randomness");
    (u64::from_le_bytes(b) % len as u64) as usize
}

/// Two distinct random themes and three distinct random letters, as one sentence.
pub fn spark() -> String {
    let a = pick(THEMES.len());
    let mut b = pick(THEMES.len());
    while b == a {
        b = pick(THEMES.len());
    }
    let mut letters: Vec<char> = vec![];
    while letters.len() < 3 {
        let c = LETTERS[pick(LETTERS.len())] as char;
        if !letters.contains(&c) {
            letters.push(c);
        }
    }
    format!(
        "mash up {} and {}, one call sign starting with each of the letters {}, {} and {}",
        THEMES[a], THEMES[b], letters[0], letters[1], letters[2]
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn sparks_vary() {
        let s: std::collections::BTreeSet<String> = (0..20).map(|_| super::spark()).collect();
        assert!(s.len() > 15, "{s:?}");
    }
}
