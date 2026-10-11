//! A random creative spark for inventing a character. Every captain is an AI playing a human whose
//! name is its call sign. Language models asked to invent a character converge on the same few
//! names and lives (Elara, Kael, a lighthouse keeper's daughter), so each reader of the guide, and
//! each identity refusal, gets a random naming tradition, two initials, an age band, a birthplace,
//! a former trade, a family shape and a flaw to work from. It never proposes a name: the agent
//! invents it. Server-side randomness only; nothing here is journaled.

/// Naming traditions to draw a name from (centuries of migration mix them freely).
const NAMES_FROM: &[&str] = &[
    "Yoruba",
    "Basque",
    "Tamil",
    "Quechua",
    "Finnish",
    "Mongolian",
    "Welsh",
    "Vietnamese",
    "Amharic",
    "Polish",
    "Samoan",
    "Georgian",
    "Hausa",
    "Icelandic",
    "Malay",
    "Armenian",
    "Maori",
    "Catalan",
    "Bengali",
    "Ukrainian",
    "Tagalog",
    "Somali",
    "Hungarian",
    "Korean",
    "Zulu",
    "Portuguese",
    "Persian",
    "Lithuanian",
    "Thai",
    "Breton",
    "Kurdish",
    "Sicilian",
    "Haitian Creole",
    "Uzbek",
    "Malagasy",
    "Greek",
    "Igbo",
    "Estonian",
    "Punjabi",
    "Scots Gaelic",
    "Turkish",
    "Tibetan",
    "Swahili",
    "Czech",
    "Romanian",
    "Javanese",
    "Albanian",
    "Nepali",
    "Fijian",
    "Latvian",
    "Wolof",
    "Sinhala",
    "Faroese",
    "Khmer",
    "Tigrinya",
];

const AGES: &[&str] = &[
    "in their twenties",
    "in their thirties",
    "in their forties",
    "in their fifties",
    "in their sixties",
    "past seventy",
];

const BIRTHPLACES: &[&str] = &[
    "an ice-mining moon",
    "a generation ship that never arrived",
    "an orbital hydroponic farm",
    "a half-terraformed desert colony",
    "a flooded ocean world",
    "the cabin of a long-haul freighter",
    "a refugee station",
    "a wealthy ring habitat",
    "a backwater asteroid",
    "a Federation naval base",
    "a company town on a gas giant moon",
    "a monastery in a dead star system",
    "a frontier trading post",
    "a crowded core world slum",
    "a quarantine colony",
    "a vineyard world",
];

const TRADES: &[&str] = &[
    "a nurse",
    "a hull welder",
    "a schoolteacher",
    "an accountant",
    "a line cook",
    "a smuggler's bookkeeper",
    "a chaplain",
    "a miner",
    "a translator",
    "a flight instructor",
    "a court stenographer",
    "a hydroponics beekeeper",
    "a debt collector",
    "a tailor",
    "a paramedic",
    "a cartographer",
    "a union organizer",
    "a casino dealer",
    "a veterinarian",
    "a glassblower",
    "a customs inspector",
    "a stage magician",
    "a funeral director",
    "a structural engineer",
    "a radio host",
    "a prison guard",
];

const FAMILIES: &[&str] = &[
    "the eldest of seven",
    "raised by a grandmother",
    "a widowed parent of two",
    "estranged from a twin",
    "newly married",
    "a single parent of a teenager",
    "caring for an ailing father",
    "adopted, searching for a birth mother",
    "divorced twice, close to both exes",
    "from a large, loud extended family",
    "the only child of famous parents",
    "a foster parent of three",
    "the last of their family",
    "engaged to someone back home",
];

const FLAWS: &[&str] = &[
    "stubborn pride",
    "a gambling habit",
    "never keeping a secret",
    "holding grudges",
    "reckless generosity",
    "a fear of being forgotten",
    "vanity",
    "a cowardice they hide",
    "a quick temper",
    "lying to protect people",
    "envy",
    "chronic indecision",
    "trusting too easily",
    "keeping score",
];

const SECRETS: &[&str] = &[
    "a debt nobody knows about",
    "a child they have never met",
    "a crime someone else was punished for",
    "a name they were born with and buried",
    "a promise they already broke",
    "a letter they never sent",
    "a partner who thinks they are dead",
    "an illness they hide",
    "a fortune they lost in one night",
    "a vow to a dead friend",
    "a sibling on the other side of a war",
    "a past in the Federation's service",
];

const LEFT_BEHIND: &[&str] = &[
    "a mother who still writes every week",
    "a sweetheart who would not come",
    "a twin who took the other path",
    "a workshop full of unfinished things",
    "a grave they never visit",
    "a band that broke up without them",
    "a congregation that expelled them",
    "children they send money to",
    "a dog with a neighbour",
    "a business partner who cheated them",
    "a town that blames them",
    "nobody at all",
];

/// Initials that start fewer stock names (no E, K, Q, X, Z).
const LETTERS: &[u8] = b"ABCDFGHIJLMNOPRSTUVWY";

fn pick(len: usize) -> usize {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("os randomness");
    (u64::from_le_bytes(b) % len as u64) as usize
}

fn one(list: &[&'static str]) -> &'static str {
    list[pick(list.len())]
}

/// A short spark for the setup pages: a sound for the name, initials, an age and a trade.
pub fn name_spark() -> String {
    let a = LETTERS[pick(LETTERS.len())] as char;
    format!(
        "a name whose sound borrows from {}, starting with {a}, for a person {}, once {}",
        one(NAMES_FROM),
        one(AGES),
        one(TRADES)
    )
}

/// One random character spark, as one sentence.
pub fn spark() -> String {
    let a = LETTERS[pick(LETTERS.len())] as char;
    let mut b = LETTERS[pick(LETTERS.len())] as char;
    while b == a {
        b = LETTERS[pick(LETTERS.len())] as char;
    }
    format!(
        "an invented name whose sound borrows from {} (starting with {a}, and {b} if it has a second part), for a person {}, born on {}, once {}, {}, whose flaw is {}, who keeps {} secret and left behind {}",
        one(NAMES_FROM),
        one(AGES),
        one(BIRTHPLACES),
        one(TRADES),
        one(FAMILIES),
        one(FLAWS),
        one(SECRETS),
        one(LEFT_BEHIND)
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
